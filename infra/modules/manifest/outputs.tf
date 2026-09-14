output "key" {
  description = "Object key of the published manifest."
  value       = var.key
}

output "s3_uri" {
  description = "s3:// URI of the published manifest."
  value       = "s3://${var.bucket}/${var.key}"
}

output "version_id" {
  description = "S3 version of this manifest. Pin a release to this value so a later publish cannot change what was approved."
  value       = var.enabled ? aws_s3_object.manifest[0].version_id : null
}

output "checksum_sha256" {
  description = "Integrity reference for the published bytes."
  value       = var.enabled ? aws_s3_object.manifest[0].checksum_sha256 : null
}
