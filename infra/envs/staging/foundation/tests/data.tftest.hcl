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

run "data_manifest_and_network_wiring" {
  command = apply

  assert {
    condition = (length(module.identity) == 1 &&
      local.manifest_resources.api_task_role_arn == module.identity[0].task_role_arns.api &&
      local.manifest_resources.worker_execution_role_arn == module.identity[0].execution_role_arns.worker &&
      local.manifest_resources.migration_execution_role_arn == module.identity[0].execution_role_arns.migration &&
      local.manifest_secret_refs.api_db == module.identity[0].secret_arns.api_db &&
      local.manifest_secret_refs.deepgram == module.identity[0].secret_arns.deepgram &&
    length(jsondecode(module.data[0].kms_key_policy_json).Statement) == 6)
    error_message = "S6 must wire distinct roles, injected secret pointers and all five service grants into the data key policy."
  }

  assert {
    condition = (length(module.data) == 1 &&
      local.audio_bucket_name == "ernote-staging-audio-000000000000-us-east-2" &&
      local.manifest_resources.audio_bucket == module.data[0].audio_bucket_name &&
      local.manifest_resources.stt_queue_name == "ernote-staging-jobs" &&
      local.manifest_resources.stt_queue_url == module.data[0].stt_queue_url &&
      local.manifest_resources.stt_dlq_url == module.data[0].stt_dlq_url &&
      local.manifest_resources.db_endpoint == module.data[0].db_host &&
      local.manifest_resources.db_name == module.data[0].db_name &&
    local.manifest_resources.data_kms_key_arn == module.data[0].kms_key_arn)
    error_message = "Staging must publish the S5 bucket, queue, database and key interfaces the release and operators read."
  }
  assert {
    condition = (can(tonumber(local.manifest_resources.db_port)) &&
      !strcontains(jsonencode(local.manifest_resources), "password") &&
      !strcontains(jsonencode(local.manifest_resources), "secretsmanager") &&
      length(local.manifest_secret_refs) == 4 &&
    !contains(values(local.manifest_secret_refs), module.data[0].db_master_user_secret_arn))
    error_message = "The manifest carries a string port and no credential: the RDS-managed master secret stays out of it."
  }
  assert {
    condition = (module.data[0].audio_bucket_name == local.audio_bucket_name &&
      contains(module.network[0].s3_gateway_bucket_arns, local.audio_bucket_arn) &&
    local.audio_bucket_arn == "arn:aws:s3:::${local.audio_bucket_name}")
    error_message = "One composed bucket name must reach both the bucket itself and S4's S3 gateway endpoint policy."
  }
  assert {
    condition = (module.data[0].db_backup_retention_days == 7 &&
    module.data[0].stt_dlq_name == "${module.data[0].stt_queue_name}-dlq")
    error_message = "Staging adopts S0's 7-day backup retention, and the DLQ keeps the name Config::stt_dlq derives."
  }
  assert {
    condition = (length(module.auth) == 0 &&
    length([for key in keys(local.manifest_resources) : key if startswith(key, "cognito_")]) == 0)
    error_message = "S7 Cognito stays off until the sandbox-pool inventory enables it."
  }
  assert {
    condition     = output.auth == null
    error_message = "With auth disabled the root publishes no auth interfaces."
  }
}

run "auth_enabled_publishes_cognito_and_scopes_onboarding" {
  command = plan
  variables {
    auth = { enabled = true }
  }

  assert {
    condition = (length(module.auth) == 1 &&
      local.manifest_resources.cognito_region == "us-east-2" &&
      contains(keys(local.manifest_resources), "cognito_pool_id") &&
    contains(keys(local.manifest_resources), "cognito_client_id"))
    error_message = "Enabling S7 publishes the pool, client and region the API needs."
  }
  # The mocked plan leaves the ARNs unpopulated, so the secret's configured name
  # (known at plan time) proves the smoke secret exists and reaches the root output.
  assert {
    condition = (output.auth != null &&
      output.auth.synthetic_smoke_secret_name == "ernote-staging/synthetic-smoke-credentials" &&
    toset(keys(output.auth)) == toset(["user_pool_id", "client_id", "issuer", "synthetic_smoke_secret_arn", "synthetic_smoke_secret_name", "synthetic_smoke_kms_key_arn"]))
    error_message = "The root exposes the pool/client ids, issuer and the smoke secret/key ARNs that feed SMOKE_SECRET_ARN and ci.smoke_*."
  }
}

run "auth_without_smoke_exception_publishes_no_smoke_arns" {
  command = plan
  variables {
    auth = { enabled = true, synthetic_smoke_exception = false }
  }

  assert {
    condition = (output.auth != null &&
      output.auth.synthetic_smoke_secret_name == null &&
      output.auth.synthetic_smoke_secret_arn == null &&
    output.auth.synthetic_smoke_kms_key_arn == null)
    error_message = "Without the staging exception there is no smoke secret or key to publish."
  }
}

run "wrong_account_refused_before_any_data_resource" {
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
