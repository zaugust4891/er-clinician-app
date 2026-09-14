# Bucket policies attach to the bucket rather than to a principal, so they apply to
# everyone — including an account administrator. They are the floor that no IAM policy
# can raise.

locals {
  # Reused by both buckets. `Principal = "*"` inside a Deny means "nobody", not
  # "everybody": a Deny with a wildcard principal denies the action for every caller
  # whose request matches the condition.
  bucket_guard_statements = {
    for name, bucket in { state = aws_s3_bucket.state, deploy = aws_s3_bucket.deploy } :
    name => [
      {
        Sid       = "DenyInsecureTransport"
        Effect    = "Deny"
        Principal = "*"
        Action    = "s3:*"
        Resource  = [bucket.arn, "${bucket.arn}/*"]
        Condition = { Bool = { "aws:SecureTransport" = "false" } }
      },
      {
        Sid       = "DenyOutdatedTls"
        Effect    = "Deny"
        Principal = "*"
        Action    = "s3:*"
        Resource  = [bucket.arn, "${bucket.arn}/*"]
        Condition = { NumericLessThan = { "s3:TlsVersion" = "1.2" } }
      },
      # The bucket already encrypts every object with the customer-managed key by
      # default, so an upload that sends no encryption header is still encrypted
      # correctly. These two statements stop a caller from *overriding* that default
      # with SSE-S3 or with some other key. `StringNotEqualsIfExists` means "if the
      # header is present and wrong, deny; if it is absent, the default applies".
      {
        Sid       = "DenyWrongEncryptionAlgorithm"
        Effect    = "Deny"
        Principal = "*"
        Action    = "s3:PutObject"
        Resource  = "${bucket.arn}/*"
        Condition = { StringNotEqualsIfExists = { "s3:x-amz-server-side-encryption" = "aws:kms" } }
      },
      {
        Sid       = "DenyWrongEncryptionKey"
        Effect    = "Deny"
        Principal = "*"
        Action    = "s3:PutObject"
        Resource  = "${bucket.arn}/*"
        Condition = { StringNotEqualsIfExists = { "s3:x-amz-server-side-encryption-aws-kms-key-id" = aws_kms_key.state.arn } }
      },
    ]
  }
}

resource "aws_s3_bucket_policy" "state" {
  bucket = aws_s3_bucket.state.id
  policy = jsonencode({
    Version   = "2012-10-17"
    Statement = local.bucket_guard_statements["state"]
  })

  # A public-access block must exist before a policy is evaluated against it, or the
  # apply can race and fail on a fresh bucket.
  depends_on = [aws_s3_bucket_public_access_block.state]
}

resource "aws_s3_bucket_policy" "deploy" {
  bucket = aws_s3_bucket.deploy.id
  policy = jsonencode({
    Version   = "2012-10-17"
    Statement = local.bucket_guard_statements["deploy"]
  })

  depends_on = [aws_s3_bucket_public_access_block.deploy]
}
