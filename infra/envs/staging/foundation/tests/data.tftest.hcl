mock_provider "aws" {
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
    length(local.manifest_secret_refs) == 0)
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
}

run "wrong_account_refused_before_any_data_resource" {
  command = plan
  variables { aws_account_id = "111111111111" }
  expect_failures = [terraform_data.account_guard]
}
