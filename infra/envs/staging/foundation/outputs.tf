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

output "data" {
  description = <<-EOT
    Non-secret S5 interfaces: bucket, queue and database identifiers, the data key, and
    the Secrets Manager ARN of the RDS-managed master credential. ARNs and names only —
    the master credential's value stays in Secrets Manager, and S6's one-shot bootstrap
    task is the only thing that reads it.
  EOT
  value       = module.data[0]
}

output "identity" {
  description = "S6 operator interfaces: role/secret ARNs and ECS injection references, never values."
  value       = module.identity[0]
}

output "auth" {
  description = <<-EOT
    Non-secret S7 interfaces: pool/client ids, issuer, and the synthetic smoke secret and
    key ARNs that feed SMOKE_SECRET_ARN and ci.smoke_*. ARNs only, never the credentials.
    Null while auth is disabled; the smoke ARNs are null without the staging exception.
  EOT
  value = var.auth.enabled ? {
    user_pool_id                = module.auth[0].user_pool_id
    client_id                   = module.auth[0].client_id
    issuer                      = module.auth[0].issuer
    synthetic_smoke_secret_arn  = module.auth[0].synthetic_smoke_secret_arn
    synthetic_smoke_secret_name = module.auth[0].synthetic_smoke_secret_name
    synthetic_smoke_kms_key_arn = module.auth[0].synthetic_smoke_kms_key_arn
  } : null
}
