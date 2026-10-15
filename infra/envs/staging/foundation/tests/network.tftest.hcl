mock_provider "aws" {
  mock_resource "aws_db_instance" {
    defaults = { master_user_secret = [{ secret_arn = "arn:aws:secretsmanager:us-east-2:000000000000:secret:rds!db-synthetic-AbCdEf", secret_status = "active", kms_key_id = "arn:aws:kms:us-east-2:000000000000:key/00000000-0000-0000-0000-000000000000" }] }
  }
  mock_data "aws_caller_identity" { defaults = { account_id = "000000000000" } }
  mock_data "aws_availability_zones" { defaults = { names = ["us-east-2c", "us-east-2b", "us-east-2a"] } }
  mock_resource "aws_kms_key" {
    # The RDS provider validates this ARN's shape, and a generated mock value is not a
    # valid ARN. Everything else about the key stays mocked.
    defaults = { arn = "arn:aws:kms:us-east-2:000000000000:key/00000000-0000-0000-0000-000000000000" }
  }
}
variables {
  aws_account_id          = "000000000000"
  deploy_artifacts_bucket = "ernote-deploy-synthetic"
  deploy_kms_key_arn      = "arn:aws:kms:us-east-2:000000000000:key/synthetic"
}
run "network_manifest" {
  command = apply
  assert {
    condition = (length(module.network) == 1 && jsondecode(local.manifest_resources.private_subnet_ids) == module.network[0].private_subnet_ids &&
      local.manifest_resources.api_security_group_id == module.network[0].security_group_ids.api &&
      local.manifest_resources.worker_security_group_id == module.network[0].security_group_ids.worker &&
      local.manifest_resources.migration_security_group_id == module.network[0].security_group_ids.migration &&
      local.manifest_resources.onboarding_security_group_id == module.network[0].security_group_ids.onboarding &&
    local.manifest_resources.api_https_port == "8443")
    error_message = "Staging must publish the real network interfaces with JSON-encoded subnet IDs."
  }
}
run "wrong_account_refused" {
  command = plan
  variables { aws_account_id = "111111111111" }
  expect_failures = [terraform_data.account_guard]
}

override_resource {
  target = module.identity[0].aws_secretsmanager_secret.this["migration_db"]
  values = { arn = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-migration_db-AbCdEf" }
}

override_resource {
  target = module.identity[0].aws_secretsmanager_secret.this["api_db"]
  values = { arn = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-api_db-AbCdEf" }
}

override_resource {
  target = module.identity[0].aws_secretsmanager_secret.this["worker_db"]
  values = { arn = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-worker_db-AbCdEf" }
}

override_resource {
  target = module.identity[0].aws_secretsmanager_secret.this["deepgram"]
  values = { arn = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-deepgram-AbCdEf" }
}

override_resource {
  target = module.identity[0].aws_secretsmanager_secret.this["onboarding_db"]
  values = { arn = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-onboarding_db-AbCdEf" }
}
