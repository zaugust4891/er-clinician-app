output "environment" {
  description = "Environment this root deploys."
  value       = local.environment
}

output "manifest_version_id" {
  description = "Exact manifest version this plan was built against. Record it with the release."
  value       = local.manifest_required ? data.aws_s3_object.manifest[0].version_id : null
}

output "image_digest" {
  description = "Image digest this release deploys. Null until Step 8 S8 adds the services."
  value       = var.image_digest
}
