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
  value       = var.enabled ? module.network[0] : null
}

output "data" {
  description = <<-EOT
    Non-secret S5 interfaces: bucket, queue and database identifiers, the data key, and
    the Secrets Manager ARN of the RDS-managed master credential — a pointer, never a
    value. Null while production is disabled.
  EOT
  value       = var.enabled ? module.data[0] : null
}

output "identity" {
  description = "S6 operator interfaces: role/secret ARNs and ECS injection references, never values."
  value       = var.enabled ? module.identity[0] : null
}
