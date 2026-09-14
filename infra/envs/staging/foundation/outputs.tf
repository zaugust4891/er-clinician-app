output "environment" {
  description = "Environment this root owns."
  value       = local.environment
}

output "manifest_s3_uri" {
  description = "Where the release pipeline reads this environment's contract."
  value       = module.manifest.s3_uri
}

output "manifest_version_id" {
  description = <<-EOT
    S3 version of the manifest this apply published. A release should be pinned to a
    specific version so that approving a deployment and applying it cannot silently
    disagree about what the foundation looks like.
  EOT
  value       = module.manifest.version_id
}

output "manifest_checksum_sha256" {
  description = "Integrity reference for the published manifest bytes."
  value       = module.manifest.checksum_sha256
}

output "network" {
  description = "Non-secret S4 interfaces for foundation components and operator inspection; null while production is disabled."
  value       = module.network[0]
}
