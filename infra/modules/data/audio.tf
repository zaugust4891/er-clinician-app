# ---------------------------------------------------------------------------
# Audio bucket
# ---------------------------------------------------------------------------
# The device PUTs raw encounter audio straight here with a presigned URL minted by the
# API (`audio_upload_intent` in routes.rs). Keys are UUID-only and org-prefixed —
# `<org>/<patient>/<encounter>/<index>-<chunk>` — so no name, label or filename reaches
# object storage. Nothing else writes to this bucket, and no export bucket exists:
# `note/export` returns its text over HTTP and has no S3 consumer to design for yet.

resource "aws_s3_bucket" "audio" {
  bucket = var.audio_bucket_name
}

# Versioning is what makes an overwritten upload recoverable. It is **not** an
# immutability guarantee: a presigned PUT can be replayed against the same key, and the
# current version is whatever landed last. Binding a chunk to one version id or checksum
# at completion is a Step 9 protocol change, tracked in the module README.
resource "aws_s3_bucket_versioning" "audio" {
  bucket = aws_s3_bucket.audio.id
  versioning_configuration {
    status = "Enabled"
  }
}

# ACLs disabled entirely, so access is decided by bucket and IAM policy alone.
resource "aws_s3_bucket_ownership_controls" "audio" {
  bucket = aws_s3_bucket.audio.id
  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_public_access_block" "audio" {
  bucket                  = aws_s3_bucket.audio.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_server_side_encryption_configuration" "audio" {
  bucket = aws_s3_bucket.audio.id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm     = "aws:kms"
      kms_master_key_id = aws_kms_key.data.arn
    }
    # Bucket keys reuse one data key across objects, which is the difference between one
    # KMS request per upload and one per bucket-key period. The encryption context S3
    # sends changes with bucket keys enabled, so S6's `kms:EncryptionContext` conditions
    # must be tested against a real upload rather than assumed.
    bucket_key_enabled = true
  }
}

# ---------------------------------------------------------------------------
# Lifecycle
# ---------------------------------------------------------------------------
# Current objects and superseded versions expire on separate clocks, and abandoned
# multipart uploads are swept up. There is deliberately no storage-class transition:
# the worker downloads pending audio with `GetObject` and has no restore workflow, so
# an archived object would fail transcription rather than delay it.
#
# Expiry is asynchronous — S3 removes objects some time after they qualify. It is not a
# deletion deadline, and it erases none of the other copies: the transcript in
# `audio_chunks`, the `TranscriptReady` event, snapshots, exported notes, or backups.

resource "aws_s3_bucket_lifecycle_configuration" "audio" {
  bucket = aws_s3_bucket.audio.id

  rule {
    id     = "expire-current-audio"
    status = "Enabled"
    filter {}

    expiration {
      days = var.audio_retention_days
    }
  }

  rule {
    id     = "expire-superseded-audio-versions"
    status = "Enabled"
    filter {}

    noncurrent_version_expiration {
      noncurrent_days = var.audio_noncurrent_version_retention_days
    }
  }

  rule {
    id     = "abort-incomplete-uploads"
    status = "Enabled"
    filter {}

    abort_incomplete_multipart_upload {
      days_after_initiation = var.audio_abort_incomplete_multipart_days
    }
  }

  depends_on = [aws_s3_bucket_versioning.audio]
}

# ---------------------------------------------------------------------------
# Bucket policy
# ---------------------------------------------------------------------------
# A bucket policy binds every caller, including an account administrator and including
# Terraform. It is the floor no IAM policy can raise.

locals {
  # Zero or one element: a tuple the optional statement is projected from, so the policy
  # stays one expression instead of a conditional whose branches have different types.
  bucket_administration_lists = length(var.bucket_administration_principal_arns) == 0 ? [] : [sort(tolist(var.bucket_administration_principal_arns))]

  audio_bucket_statements = concat([
    {
      Sid       = "DenyInsecureTransport"
      Effect    = "Deny"
      Principal = "*"
      Action    = "s3:*"
      Resource  = [local.audio_bucket_arn, "${local.audio_bucket_arn}/*"]
      Condition = { Bool = { "aws:SecureTransport" = "false" } }
    },
    {
      Sid       = "DenyOutdatedTls"
      Effect    = "Deny"
      Principal = "*"
      Action    = "s3:*"
      Resource  = [local.audio_bucket_arn, "${local.audio_bucket_arn}/*"]
      Condition = { NumericLessThan = { "s3:TlsVersion" = "1.2" } }
    },
    # The presigned PUT the device performs carries a signature and a Content-Type and
    # no encryption headers at all, and the bucket default encrypts it correctly. These
    # two statements stop a caller from *overriding* that default with SSE-S3 or with a
    # different key. `StringNotEqualsIfExists` is load-bearing: absent header, allowed;
    # present and wrong, denied. Requiring the headers instead would break every
    # currently minted URL and would need the presign contract and client changed first.
    {
      Sid       = "DenyWrongEncryptionAlgorithm"
      Effect    = "Deny"
      Principal = "*"
      Action    = "s3:PutObject"
      Resource  = "${local.audio_bucket_arn}/*"
      Condition = { StringNotEqualsIfExists = { "s3:x-amz-server-side-encryption" = "aws:kms" } }
    },
    {
      Sid       = "DenyWrongEncryptionKey"
      Effect    = "Deny"
      Principal = "*"
      Action    = "s3:PutObject"
      Resource  = "${local.audio_bucket_arn}/*"
      Condition = { StringNotEqualsIfExists = { "s3:x-amz-server-side-encryption-aws-kms-key-id" = aws_kms_key.data.arn } }
    }],
    # Opt-in: pin the bucket's own controls to named principals. Runtime roles are kept
    # away from these actions by their S6 policies; this statement is the second layer,
    # and it is empty by default because locking a bucket to the wrong ARN list is its
    # own kind of outage. `aws:PrincipalArn` reports the role ARN for an assumed role,
    # so list role ARNs here, not STS session ARNs. There is no `aws:SourceVpce`
    # condition anywhere in this policy: the phone and CLI sign uploads from outside
    # the VPC, and a VPC-only deny would break every one of those uploads.
    [for principals in local.bucket_administration_lists : {
      Sid       = "DenyProtectionChangesOutsideAdministration"
      Effect    = "Deny"
      Principal = "*"
      Action = [
        "s3:DeleteBucket",
        "s3:PutBucketPolicy",
        "s3:DeleteBucketPolicy",
        "s3:PutBucketVersioning",
        "s3:PutLifecycleConfiguration",
        "s3:PutEncryptionConfiguration",
        "s3:PutBucketPublicAccessBlock",
        "s3:PutBucketOwnershipControls",
        "s3:PutBucketAcl",
      ]
      Resource  = local.audio_bucket_arn
      Condition = { StringNotLike = { "aws:PrincipalArn" = principals } }
  }])
}

resource "aws_s3_bucket_policy" "audio" {
  bucket = aws_s3_bucket.audio.id
  policy = jsonencode({
    Version   = "2012-10-17"
    Statement = local.audio_bucket_statements
  })

  # A public-access block must exist before a policy is evaluated against it, or the
  # apply can race and fail on a fresh bucket.
  depends_on = [aws_s3_bucket_public_access_block.audio]
}
