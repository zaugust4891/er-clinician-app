output "state_bucket" {
  description = "Bucket that holds every root's Terraform state. Pass as -backend-config=\"bucket=...\"."
  value       = aws_s3_bucket.state.id
}

output "state_kms_key_arn" {
  description = "Encryption key for state and manifests. Pass as -backend-config=\"kms_key_id=...\"."
  value       = aws_kms_key.state.arn
}

output "state_kms_alias" {
  description = "Human-readable alias for the same key."
  value       = aws_kms_alias.state.name
}

output "deploy_artifacts_bucket" {
  description = "Bucket holding each environment's non-secret foundation manifest."
  value       = aws_s3_bucket.deploy.id
}

output "manifest_keys" {
  description = "Environment to manifest object key. The foundation root writes it; the release root reads it."
  value       = local.manifest_keys
}

output "backend_config" {
  description = <<-EOT
    Ready-made `terraform init -backend-config` values for every root. No credentials
    appear here: only bucket names, object keys, the region, and the key ARN.
  EOT
  value = {
    for name, scope in local.state_scopes : name => {
      bucket     = aws_s3_bucket.state.id
      key        = "${scope.prefix}/terraform.tfstate"
      region     = var.aws_region
      kms_key_id = aws_kms_key.state.arn
    }
  }
}

output "state_policy_arns" {
  description = "Managed policies that grant one root's state access. Step 8 S6/S11 attach these to CI roles."
  value       = { for name, policy in aws_iam_policy.state_scope : name => policy.arn }
}

output "state_operator_role_arns" {
  description = "Roles to assume when running Terraform for a given root."
  value       = { for name, role in aws_iam_role.state_operator : name => role.arn }
}
