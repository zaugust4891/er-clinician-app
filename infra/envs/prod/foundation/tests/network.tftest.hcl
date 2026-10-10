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
run "production_stays_disabled" {
  command = plan
  assert {
    condition     = length(module.network) == 0 && length(data.aws_availability_zones.network) == 0 && output.network == null && module.manifest.version_id == null && length(local.manifest_resources) == 1
    error_message = "Disabled production must create no network or manifest and perform no AZ discovery."
  }
}

run "production_explicitly_enabled" {
  command = apply
  variables {
    enabled = true
    network = { availability_zones = ["us-east-2a", "us-east-2b"], nat_per_az = true }
    # S5 requires production to state its own availability and retention decisions
    # before this root can be enabled at all; see tests/data.tftest.hcl.
    data = {
      db_multi_az                             = true
      db_backup_retention_days                = 35
      audio_retention_days                    = 30
      audio_noncurrent_version_retention_days = 30
    }
  }
  assert {
    condition     = length(module.network) == 1 && length(module.network[0].nat_gateway_ids) == 2 && local.manifest_resources.vpc_id == module.network[0].vpc_id && length(data.aws_availability_zones.network) == 0
    error_message = "Explicitly enabled production must wire the selected network and publish its interfaces without rediscovering pinned AZs."
  }
}
