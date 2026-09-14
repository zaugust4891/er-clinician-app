# ---------------------------------------------------------------------------
# Encryption key
# ---------------------------------------------------------------------------
# One customer-managed KMS key encrypts both buckets created here. Both hold
# deployment metadata rather than clinical content, and the real separation between
# staging and production state is enforced object-by-object in iam.tf. Using one key
# keeps the monthly key charge at one unit; if artifact consumers ever multiply, split
# it then rather than now.

resource "aws_kms_key" "state" {
  description             = "${var.project} Terraform state and deployment artifacts"
  deletion_window_in_days = 30
  enable_key_rotation     = true

  # "Enable IAM policies": the account may delegate use of this key through IAM. It
  # does not grant anything by itself; every grant below is an IAM policy in iam.tf.
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid       = "EnableIAMPolicies"
        Effect    = "Allow"
        Principal = { AWS = "arn:${data.aws_partition.current.partition}:iam::${var.aws_account_id}:root" }
        Action    = "kms:*"
        Resource  = "*"
      }
    ]
  })

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_kms_alias" "state" {
  name          = "alias/${var.project}-tfstate"
  target_key_id = aws_kms_key.state.key_id
}

# ---------------------------------------------------------------------------
# State bucket
# ---------------------------------------------------------------------------

resource "aws_s3_bucket" "state" {
  bucket = local.state_bucket_name

  # Terraform refuses to destroy this bucket even if a future change removes the
  # resource block. Losing state does not lose the cloud resources, but it does lose
  # Terraform's map of them, which is a long and error-prone day.
  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_s3_bucket_versioning" "state" {
  bucket = aws_s3_bucket.state.id
  versioning_configuration {
    status = "Enabled"
  }
}

# Object ownership enforced by the bucket owner disables ACLs entirely, so access is
# decided only by bucket and IAM policy. One mechanism instead of two.
resource "aws_s3_bucket_ownership_controls" "state" {
  bucket = aws_s3_bucket.state.id
  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_public_access_block" "state" {
  bucket                  = aws_s3_bucket.state.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_server_side_encryption_configuration" "state" {
  bucket = aws_s3_bucket.state.id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm     = "aws:kms"
      kms_master_key_id = aws_kms_key.state.arn
    }
    # S3 Bucket Keys cut KMS request cost by reusing a data key across objects.
    bucket_key_enabled = true
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "state" {
  bucket = aws_s3_bucket.state.id

  rule {
    id     = "expire-superseded-state-versions"
    status = "Enabled"
    filter {}

    noncurrent_version_expiration {
      noncurrent_days = var.state_noncurrent_version_retention_days
    }
  }

  rule {
    id     = "abort-incomplete-uploads"
    status = "Enabled"
    filter {}

    abort_incomplete_multipart_upload {
      days_after_initiation = 7
    }
  }

  depends_on = [aws_s3_bucket_versioning.state]
}

# ---------------------------------------------------------------------------
# Deployment-artifact bucket
# ---------------------------------------------------------------------------
# Holds the non-secret foundation manifest each environment publishes. It lives here,
# next to state, rather than inside a foundation stack, because a staging foundation is
# allowed to be torn down and rebuilt and the release pipeline's contract is not.

resource "aws_s3_bucket" "deploy" {
  bucket = local.deploy_bucket_name

  lifecycle {
    prevent_destroy = true
  }
}

resource "aws_s3_bucket_versioning" "deploy" {
  bucket = aws_s3_bucket.deploy.id
  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_ownership_controls" "deploy" {
  bucket = aws_s3_bucket.deploy.id
  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

resource "aws_s3_bucket_public_access_block" "deploy" {
  bucket                  = aws_s3_bucket.deploy.id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_server_side_encryption_configuration" "deploy" {
  bucket = aws_s3_bucket.deploy.id
  rule {
    apply_server_side_encryption_by_default {
      sse_algorithm     = "aws:kms"
      kms_master_key_id = aws_kms_key.state.arn
    }
    bucket_key_enabled = true
  }
}

resource "aws_s3_bucket_lifecycle_configuration" "deploy" {
  bucket = aws_s3_bucket.deploy.id

  rule {
    id     = "expire-superseded-manifests"
    status = "Enabled"
    filter {}

    noncurrent_version_expiration {
      noncurrent_days = 180
    }
  }

  rule {
    id     = "abort-incomplete-uploads"
    status = "Enabled"
    filter {}

    abort_incomplete_multipart_upload {
      days_after_initiation = 7
    }
  }

  depends_on = [aws_s3_bucket_versioning.deploy]
}
