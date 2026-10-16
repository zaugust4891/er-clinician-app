output "user_pool_id" {
  description = "COGNITO_POOL_ID for the API."
  value       = aws_cognito_user_pool.this.id
}

output "user_pool_arn" {
  description = "Scopes the onboarding task role's AdminGetUser permission."
  value       = aws_cognito_user_pool.this.arn
}

output "client_id" {
  description = "COGNITO_CLIENT_ID for the API; access tokens must carry it as client_id."
  value       = aws_cognito_user_pool_client.native.id
}

output "issuer" {
  description = "The iss every accepted access token carries."
  value       = "https://${aws_cognito_user_pool.this.endpoint}"
}

output "synthetic_smoke_secret_arn" {
  description = "Container for synthetic smoke credentials; null without the staging exception. Feeds ci.smoke_secret_arn."
  value       = one(aws_secretsmanager_secret.synthetic_smoke[*].arn)
}

output "synthetic_smoke_secret_name" {
  description = "Name of that secret; null without the staging exception."
  value       = one(aws_secretsmanager_secret.synthetic_smoke[*].name)
}

output "synthetic_smoke_kms_key_arn" {
  description = "Key for that secret only; null without the staging exception. Feeds ci.smoke_kms_key_arn."
  value       = one(aws_kms_key.synthetic_credentials[*].arn)
}
