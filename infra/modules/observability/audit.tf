# Account trail, not organization-wide ownership. Inspect/adopt any existing
# account trail before apply; staging/prod in one account can otherwise duplicate
# management events. Object keys and AWS principals are restricted linkable data.
resource "aws_s3_bucket" "audit" {
  bucket        = local.audit_bucket
  force_destroy = false
  lifecycle { prevent_destroy = true }
}
resource "aws_s3_bucket_public_access_block" "audit" {
  bucket                  = aws_s3_bucket.audit.id
  block_public_acls       = true
  ignore_public_acls      = true
  block_public_policy     = true
  restrict_public_buckets = true
}
resource "aws_s3_bucket_ownership_controls" "audit" {
  bucket = aws_s3_bucket.audit.id
  rule { object_ownership = "BucketOwnerEnforced" }
}
resource "aws_s3_bucket_versioning" "audit" {
  bucket = aws_s3_bucket.audit.id
  versioning_configuration { status = "Enabled" }
}
resource "aws_s3_bucket_server_side_encryption_configuration" "audit" {
  bucket = aws_s3_bucket.audit.id
  rule {
    # CloudTrail supplies its own encryption context; do not replace it with an S3 bucket key.
    bucket_key_enabled = false
    apply_server_side_encryption_by_default {
      sse_algorithm     = "aws:kms"
      kms_master_key_id = aws_kms_key.telemetry.arn
    }
  }
}
resource "aws_s3_bucket_lifecycle_configuration" "audit" {
  bucket     = aws_s3_bucket.audit.id
  depends_on = [aws_s3_bucket_versioning.audit]
  rule {
    id     = "restricted-audit-retention"
    status = "Enabled"
    filter {}
    expiration { days = var.audit_retention_days }
    noncurrent_version_expiration { noncurrent_days = var.audit_retention_days }
    abort_incomplete_multipart_upload { days_after_initiation = 7 }
  }
}
resource "aws_s3_bucket_policy" "audit" {
  bucket = aws_s3_bucket.audit.id
  policy = jsonencode({ Version = "2012-10-17", Statement = concat([
    { Sid = "TrailAclCheck", Effect = "Allow", Principal = { Service = "cloudtrail.amazonaws.com" }, Action = "s3:GetBucketAcl", Resource = aws_s3_bucket.audit.arn, Condition = { StringEquals = { "aws:SourceArn" = local.trail_arn } } },
    { Sid = "TrailWrite", Effect = "Allow", Principal = { Service = "cloudtrail.amazonaws.com" }, Action = "s3:PutObject", Resource = "${aws_s3_bucket.audit.arn}/AWSLogs/${var.aws_account_id}/*", Condition = { StringEquals = { "aws:SourceArn" = local.trail_arn, "s3:x-amz-acl" = "bucket-owner-full-control" } } },
    { Sid = "DenyInsecureTransport", Effect = "Deny", Principal = "*", Action = "s3:*", Resource = [aws_s3_bucket.audit.arn, "${aws_s3_bucket.audit.arn}/*"], Condition = { Bool = { "aws:SecureTransport" = "false" } } },
    # Operators read through the dedicated role. No deployment/task role reads logs.
    { Sid = "RestrictAuditReads", Effect = "Deny", Principal = "*", Action = ["s3:GetObject", "s3:GetObjectVersion"], Resource = "${aws_s3_bucket.audit.arn}/*", Condition = { StringNotEquals = { "aws:PrincipalArn" = local.operator_arn } } },
    { Sid = "DenyManualDeletion", Effect = "Deny", Principal = "*", Action = ["s3:DeleteObject", "s3:DeleteObjectVersion"], Resource = "${aws_s3_bucket.audit.arn}/*" }
  ], module.audit_encryption.statements) })
}
module "audit_encryption" {
  source      = "../s3-encryption-guards"
  bucket_arn  = aws_s3_bucket.audit.arn
  kms_key_arn = aws_kms_key.telemetry.arn
}
resource "aws_cloudtrail" "management" {
  name                          = var.name
  s3_bucket_name                = aws_s3_bucket.audit.id
  kms_key_id                    = aws_kms_key.telemetry.arn
  is_multi_region_trail         = true
  include_global_service_events = true
  is_organization_trail         = false
  enable_logging                = true
  enable_log_file_validation    = true
  event_selector {
    read_write_type           = "All"
    include_management_events = true
    dynamic "data_resource" {
      for_each = var.audio_data_events ? [1] : []
      content {
        type   = "AWS::S3::Object"
        values = ["${var.audio_bucket_arn}/"]
      }
    }
  }
  depends_on = [aws_s3_bucket_policy.audit, aws_s3_bucket_public_access_block.audit, aws_s3_bucket_versioning.audit, aws_s3_bucket_server_side_encryption_configuration.audit]
}
