output "task_role_arns" {
  description = "Task roles by process. The migration role is deliberately empty; the DB bootstrap has none."
  value       = { for component, role in aws_iam_role.task : component => role.arn }
}

output "execution_role_arns" {
  description = "Execution roles by process. Each injects only that process's own secrets."
  value       = { for component, role in aws_iam_role.execution : component => role.arn }
}

output "secret_arns" {
  description = "Secret containers: pointers, never values. Terraform creates no secret version."
  value       = { for key, secret in aws_secretsmanager_secret.this : key => secret.arn }
}

output "log_group_names" {
  description = "Log groups S9 must create; each execution role writes only its own."
  value       = local.log_group_names
}

output "kms_key_policy_statements" {
  description = "Service-specific IAM delegations with exact role-ARN conditions that the data module appends to the application-data key policy."
  value       = local.kms_key_policy_statements
}

output "ecs_secrets" {
  description = <<-EOT
    Each process's ECS `secrets` entries, environment name => valueFrom. Services
    read AWSCURRENT. Bootstrap sources each login at AWSPENDING; db-secrets.py render
    replaces those entries with exact version IDs before its task is registered.
  EOT
  value = {
    api = {
      DB_USERNAME = "${aws_secretsmanager_secret.this["api_db"].arn}:username::"
      DB_PASSWORD = "${aws_secretsmanager_secret.this["api_db"].arn}:password::"
    }
    worker = {
      DB_USERNAME      = "${aws_secretsmanager_secret.this["worker_db"].arn}:username::"
      DB_PASSWORD      = "${aws_secretsmanager_secret.this["worker_db"].arn}:password::"
      DEEPGRAM_API_KEY = "${aws_secretsmanager_secret.this["deepgram"].arn}:api_key::"
    }
    migration = {
      DB_USERNAME = "${aws_secretsmanager_secret.this["migration_db"].arn}:username::"
      DB_PASSWORD = "${aws_secretsmanager_secret.this["migration_db"].arn}:password::"
    }
    onboarding = {
      DB_USERNAME = "${aws_secretsmanager_secret.this["onboarding_db"].arn}:username::"
      DB_PASSWORD = "${aws_secretsmanager_secret.this["onboarding_db"].arn}:password::"
    }
    db-bootstrap = {
      DB_USERNAME            = "${var.db_master_user_secret_arn}:username::"
      DB_PASSWORD            = "${var.db_master_user_secret_arn}:password::"
      OWNER_DB_USERNAME      = "${aws_secretsmanager_secret.this["migration_db"].arn}:username:AWSPENDING:"
      OWNER_DB_PASSWORD      = "${aws_secretsmanager_secret.this["migration_db"].arn}:password:AWSPENDING:"
      API_DB_USERNAME        = "${aws_secretsmanager_secret.this["api_db"].arn}:username:AWSPENDING:"
      API_DB_PASSWORD        = "${aws_secretsmanager_secret.this["api_db"].arn}:password:AWSPENDING:"
      WORKER_DB_USERNAME     = "${aws_secretsmanager_secret.this["worker_db"].arn}:username:AWSPENDING:"
      ONBOARDING_DB_USERNAME = "${aws_secretsmanager_secret.this["onboarding_db"].arn}:username:AWSPENDING:"
      ONBOARDING_DB_PASSWORD = "${aws_secretsmanager_secret.this["onboarding_db"].arn}:password:AWSPENDING:"
      WORKER_DB_PASSWORD     = "${aws_secretsmanager_secret.this["worker_db"].arn}:password:AWSPENDING:"
    }
  }
}
