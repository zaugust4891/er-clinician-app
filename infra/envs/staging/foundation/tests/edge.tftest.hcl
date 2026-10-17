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

override_module {
  target = module.edge[0]
  outputs = {
    api_target_group_arn    = "arn:aws:elasticloadbalancing:us-east-2:000000000000:targetgroup/ernote-staging-api/1234567890abcdef"
    api_base_url            = "https://api.example.test"
    alb_arn                 = "arn:aws:elasticloadbalancing:us-east-2:000000000000:loadbalancer/app/ernote-staging-api/1234567890abcdef"
    alb_arn_suffix          = "app/ernote-staging-api/1234567890abcdef"
    target_group_arn_suffix = "targetgroup/ernote-staging-api/1234567890abcdef"
    waf_acl_arn             = "arn:aws:wafv2:us-east-2:000000000000:regional/webacl/ernote-staging-api/00000000-0000-0000-0000-000000000000"
    certificate_arn         = "arn:aws:acm:us-east-2:000000000000:certificate/00000000-0000-0000-0000-000000000000"
    ingress_enabled         = false
  }
}
override_resource {
  target = module.cluster[0].aws_ecr_repository.app
  values = { repository_url = "000000000000.dkr.ecr.us-east-2.amazonaws.com/ernote-staging" }
}
run "edge_and_private_operator_templates_publish_only_pointers" {
  command = apply
  variables {
    edge                  = { hostname = "api.example.test", hosted_zone_id = "ZSYNTHETIC" }
    operator_image_digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  }
  assert {
    condition = (
      local.manifest_resources.api_target_group_arn == module.edge[0].api_target_group_arn &&
      local.manifest_resources.api_base_url == "https://api.example.test" &&
      !contains(keys(local.manifest_resources), "db_bootstrap_execution_role_arn") &&
      length(module.network[0].security_group_ids) == 6 &&
      !contains(keys(output.operator_task_templates["db-bootstrap"]), "taskRoleArn") &&
      output.operator_task_templates["db-bootstrap"].executionRoleArn == module.identity[0].execution_role_arns["db-bootstrap"] &&
      { for s in output.operator_task_templates["db-bootstrap"].containerDefinitions[0].secrets : s.name => s.valueFrom } == module.identity[0].ecs_secrets["db-bootstrap"] &&
      output.operator_task_templates.onboarding.taskRoleArn == module.identity[0].task_role_arns.onboarding &&
      output.operator_task_networks["db-bootstrap"].awsvpcConfiguration.assignPublicIp == "DISABLED" &&
      output.operator_task_networks.onboarding.awsvpcConfiguration.securityGroups == [module.network[0].security_group_ids.onboarding]
    )
    error_message = "Foundation must publish edge references but keep bootstrap/onboarding privilege and networking operator-only."
  }
}
run "reject_ingress_without_edge" {
  command = plan
  variables { network = { client_ipv4_cidrs = ["192.0.2.1/32"] } }
  expect_failures = [terraform_data.ingress_guard]
}
run "reject_ingress_while_waf_gate_closed" {
  command = plan
  variables {
    edge    = { hostname = "api.example.test", hosted_zone_id = "ZSYNTHETIC" }
    network = { client_ipv4_cidrs = ["192.0.2.1/32"] }
  }
  expect_failures = [terraform_data.ingress_guard]
}
run "reject_redirect_without_edge" {
  command = plan
  variables { network = { http_redirect_enabled = true } }
  expect_failures = [terraform_data.ingress_guard]
}
run "explicit_test_ip_can_open_with_waf" {
  command = plan
  variables {
    edge    = { hostname = "api.example.test", hosted_zone_id = "ZSYNTHETIC", ingress_enabled = true }
    network = { client_ipv4_cidrs = ["192.0.2.1/32"] }
  }
}
