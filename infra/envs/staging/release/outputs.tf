output "environment" {
  description = "Environment this root deploys."
  value       = local.environment
}

output "manifest_version_id" {
  description = "Exact manifest version this plan was built against. Record it with the release."
  value       = local.manifest_required ? data.aws_s3_object.manifest[0].version_id : null
}

output "image_digest" {
  description = "Image digest this release deploys. Null until a candidate is supplied."
  value       = var.image_digest
}

output "compute" {
  description = "S8 task templates, services, private migration placement and connection budget; null without a release digest."
  value       = one(module.compute[*])
}
