# Writes one JSON object describing what a foundation stack built, so the release
# pipeline can read a short, reviewed contract instead of being handed read access to
# an entire Terraform state file.
#
# The content is deliberately a pure function of its inputs — there is no timestamp()
# call — so re-running `plan` on unchanged infrastructure shows no diff. When something
# does change, `generated_by` records which commit or workflow run produced it.

resource "aws_s3_object" "manifest" {
  count = var.enabled ? 1 : 0

  bucket       = var.bucket
  key          = var.key
  content_type = "application/json"
  content      = jsonencode(var.manifest)

  server_side_encryption = "aws:kms"
  kms_key_id             = var.kms_key_arn

  # Asking S3 to compute a SHA-256 gives the release pipeline an integrity reference
  # it can check, which an ETag cannot provide for a KMS-encrypted object.
  checksum_algorithm = "SHA256"
}
