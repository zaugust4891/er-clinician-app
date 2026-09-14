mock_provider "aws" {
  mock_data "aws_caller_identity" { defaults = { account_id = "000000000000" } }
  mock_data "aws_availability_zones" { defaults = { names = ["us-east-2c", "us-east-2b", "us-east-2a"] } }
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
