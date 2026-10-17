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

run "disabled_production_creates_no_data_resources" {
  command = plan
  assert {
    condition = (length(module.data) == 0 && length(module.identity) == 0 && output.identity == null &&
      output.data == null && length(module.cluster) == 0 &&
    length(local.manifest_resources) == 1)
    error_message = "Disabled production must create no database, bucket, queue or key, and must publish nothing about them."
  }
}

run "enabling_without_a_retention_decision_is_refused" {
  command = plan
  variables {
    enabled = true
    network = { availability_zones = ["us-east-2a", "us-east-2b"] }
    data = {
      db_multi_az                             = true
      audio_retention_days                    = 30
      audio_noncurrent_version_retention_days = 30
    }
  }
  expect_failures = [var.data]
}

run "enabling_without_an_availability_decision_is_refused" {
  command = plan
  variables {
    enabled = true
    network = { availability_zones = ["us-east-2a", "us-east-2b"] }
    data = {
      db_backup_retention_days                = 35
      audio_retention_days                    = 30
      audio_noncurrent_version_retention_days = 30
    }
  }
  expect_failures = [var.data]
}

run "production_explicitly_enabled_with_its_own_decisions" {
  command = apply
  variables {
    enabled = true
    network = { availability_zones = ["us-east-2a", "us-east-2b"] }
    data = {
      db_multi_az                             = true
      db_backup_retention_days                = 35
      audio_retention_days                    = 30
      audio_noncurrent_version_retention_days = 30
    }
  }
  assert {
    condition = (length(module.data) == 1 &&
      module.data[0].db_backup_retention_days == 35 &&
      local.manifest_resources.audio_bucket == "ernote-prod-audio-000000000000-us-east-2" &&
      local.manifest_resources.stt_queue_name == "ernote-prod-jobs" &&
    contains(module.network[0].s3_gateway_bucket_arns, local.audio_bucket_arn))
    error_message = "An explicitly enabled production root must apply its own retention and publish its own S5 interfaces."
  }
}

run "enabling_without_current_audio_retention_is_refused" {
  command = plan
  variables {
    enabled = true
    network = { availability_zones = ["us-east-2a", "us-east-2b"] }
    data = {
      db_multi_az                             = true
      db_backup_retention_days                = 35
      audio_noncurrent_version_retention_days = 30
    }
  }
  expect_failures = [var.data]
}

run "enabling_without_noncurrent_audio_retention_is_refused" {
  command = plan
  variables {
    enabled = true
    network = { availability_zones = ["us-east-2a", "us-east-2b"] }
    data = {
      db_multi_az              = true
      db_backup_retention_days = 35
      audio_retention_days     = 30
    }
  }
  expect_failures = [var.data]
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
