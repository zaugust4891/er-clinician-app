mock_provider "aws" {}
variables {
  name           = "ernote-staging"
  environment    = "staging"
  aws_account_id = "000000000000"
  aws_region     = "us-east-2"
  image_digest   = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  resources = {
    "deploy_artifacts_bucket" : "ernote-deploy-synthetic",
    "vpc_id" : "vpc-0123456789abcdef0",
    "ecr_repository_url" : "000000000000.dkr.ecr.us-east-2.amazonaws.com/ernote-staging",
    "ecs_cluster_arn" : "arn:aws:ecs:us-east-2:000000000000:cluster/ernote-staging",
    "private_subnet_ids" : "[\"subnet-0123456789abcdef0\", \"subnet-0123456789abcdef1\"]",
    "api_target_group_arn" : "arn:aws:elasticloadbalancing:us-east-2:000000000000:targetgroup/ernote-staging-api/1234567890abcdef",
    "api_https_port" : "8443",
    "db_endpoint" : "synthetic.us-east-2.rds.amazonaws.com",
    "db_port" : "5432",
    "db_name" : "ernote",
    "audio_bucket" : "ernote-staging-audio-synthetic",
    "stt_queue_name" : "ernote-staging-stt",
    "cognito_region" : "us-east-2",
    "cognito_pool_id" : "us-east-2_Synthetic",
    "cognito_client_id" : "syntheticclient",
    "api_security_group_id" : "sg-0123456789abcdef0",
    "api_task_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-api-task",
    "api_execution_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-api-execution",
    "api_log_group" : "/ecs/ernote-staging/api",
    "worker_security_group_id" : "sg-0123456789abcdef0",
    "worker_task_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-worker-task",
    "worker_execution_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-worker-execution",
    "worker_log_group" : "/ecs/ernote-staging/worker",
    "migration_security_group_id" : "sg-0123456789abcdef0",
    "migration_task_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-migration-task",
    "migration_execution_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-migration-execution",
    "migration_log_group" : "/ecs/ernote-staging/migration",
    "onboarding_security_group_id" : "sg-0123456789abcdef0"
  }
  secret_refs = {
    "api_db" : "arn:aws:secretsmanager:us-east-2:000000000000:secret:ernote-staging/db/api-AbCdEf",
    "worker_db" : "arn:aws:secretsmanager:us-east-2:000000000000:secret:ernote-staging/db/worker-AbCdEf",
    "migration_db" : "arn:aws:secretsmanager:us-east-2:000000000000:secret:ernote-staging/db/migration-AbCdEf",
    "deepgram" : "arn:aws:secretsmanager:us-east-2:000000000000:secret:ernote-staging/deepgram-AbCdEf"
  }
}
run "templates_do_not_activate_services" {
  command = apply
  assert {
    condition     = length(aws_ecs_service.app) == 0 && length(aws_ecs_task_definition.service) == 0 && length(output.task_templates) == 3
    error_message = "Migration and review must be possible before service creation."
  }
  assert {
    condition = alltrue([for p, t in output.task_templates : (
      t.containerDefinitions[0].image == "000000000000.dkr.ecr.us-east-2.amazonaws.com/ernote-staging@${var.image_digest}" &&
      t.containerDefinitions[0].user == "65532:65532" && t.containerDefinitions[0].readonlyRootFilesystem &&
      !t.containerDefinitions[0].privileged && t.containerDefinitions[0].linuxParameters.capabilities.drop == ["ALL"] &&
      length(t.containerDefinitions[0].mountPoints) == 0 && t.cpu == "512" && t.memory == "1024" &&
      t.networkMode == "awsvpc" && t.runtimePlatform.cpuArchitecture == "X86_64" &&
      t.taskRoleArn == var.resources["${p}_task_role_arn"] && t.executionRoleArn == var.resources["${p}_execution_role_arn"]
    )])
    error_message = "All processes must share the digest and container hardening, with separate scoped roles."
  }
  assert {
    condition = (
      output.task_templates.api.containerDefinitions[0].entryPoint == ["/usr/local/bin/er-server"] &&
      output.task_templates.worker.containerDefinitions[0].entryPoint == ["/usr/local/bin/er-worker"] &&
      output.task_templates.migration.containerDefinitions[0].entryPoint == ["/usr/local/bin/er-migrate"] &&
      length(output.task_templates.api.containerDefinitions[0].secrets) == 2 &&
      length(output.task_templates.worker.containerDefinitions[0].secrets) == 3 &&
      length(output.task_templates.migration.containerDefinitions[0].secrets) == 2 &&
      output.task_templates.worker.containerDefinitions[0].stopTimeout == 120 &&
      output.task_templates.api.containerDefinitions[0].stopTimeout == 60 &&
      length(output.task_templates.worker.containerDefinitions[0].portMappings) == 0 &&
      output.task_templates.api.containerDefinitions[0].portMappings[0].containerPort == 8443
    )
    error_message = "Entrypoints, injection boundaries, listener isolation and drain timeouts must match the binaries."
  }
  assert {
    condition = (
      { for v in output.task_templates.worker.containerDefinitions[0].environment : v.name => v.value }["DB_MAX_CONNECTIONS"] == "5" &&
      { for v in output.task_templates.worker.containerDefinitions[0].environment : v.name => v.value }["SUMMARY_MODEL"] == "sonnet" &&
      { for v in output.task_templates.api.containerDefinitions[0].environment : v.name => v.value }["API_TLS"] == "self-signed" &&
      { for v in output.task_templates.migration.containerDefinitions[0].environment : v.name => v.value }["DB_MAX_CONNECTIONS"] == "1" &&
      output.connection_budget.required == 35 && output.migration_network_configuration.awsvpcConfiguration.assignPublicIp == "DISABLED"
    )
    error_message = "Cloud startup and maximum rollout connection budget must be explicit."
  }
}
run "services_keep_api_capacity_and_stop_worker_before_replacement" {
  command = apply
  variables { services_enabled = true }
  assert {
    condition = (
      length(aws_ecs_service.app) == 2 && alltrue([for t in aws_ecs_task_definition.service : t.skip_destroy]) && aws_ecs_service.app["api"].desired_count == 2 &&
      aws_ecs_service.app["api"].deployment_minimum_healthy_percent == 100 && aws_ecs_service.app["api"].deployment_maximum_percent == 200 &&
      aws_ecs_service.app["worker"].desired_count == 1 && aws_ecs_service.app["worker"].deployment_minimum_healthy_percent == 0 && aws_ecs_service.app["worker"].deployment_maximum_percent == 100 &&
      aws_ecs_service.app["worker"].availability_zone_rebalancing == "DISABLED" && length(aws_ecs_service.app["worker"].load_balancer) == 0 &&
      aws_ecs_service.app["api"].availability_zone_rebalancing == "ENABLED" && aws_ecs_service.app["api"].health_check_grace_period_seconds == 120
    )
    error_message = "API must roll with healthy capacity; worker must have one consumer and no AZ-rebalance overlap or public target."
  }
  assert {
    condition = alltrue([for s in aws_ecs_service.app : (
      s.platform_version == "1.4.0" && !s.enable_execute_command && !s.network_configuration[0].assign_public_ip &&
      s.network_configuration[0].subnets == toset(jsondecode(var.resources.private_subnet_ids)) &&
      s.wait_for_steady_state && s.deployment_circuit_breaker[0].enable && s.deployment_circuit_breaker[0].rollback
    )])
    error_message = "Private Fargate services need pinned platform, no Exec, and a failed-startup rollout gate."
  }
}
run "reject_missing_telemetry" {
  command = plan
  variables {
    resources = {
      "deploy_artifacts_bucket" : "ernote-deploy-synthetic",
      "vpc_id" : "vpc-0123456789abcdef0",
      "ecr_repository_url" : "000000000000.dkr.ecr.us-east-2.amazonaws.com/ernote-staging",
      "ecs_cluster_arn" : "arn:aws:ecs:us-east-2:000000000000:cluster/ernote-staging",
      "private_subnet_ids" : "[\"subnet-0123456789abcdef0\", \"subnet-0123456789abcdef1\"]",
      "api_target_group_arn" : "arn:aws:elasticloadbalancing:us-east-2:000000000000:targetgroup/ernote-staging-api/1234567890abcdef",
      "api_https_port" : "8443",
      "db_endpoint" : "synthetic.us-east-2.rds.amazonaws.com",
      "db_port" : "5432",
      "db_name" : "ernote",
      "audio_bucket" : "ernote-staging-audio-synthetic",
      "stt_queue_name" : "ernote-staging-stt",
      "cognito_region" : "us-east-2",
      "cognito_pool_id" : "us-east-2_Synthetic",
      "cognito_client_id" : "syntheticclient",
      "api_security_group_id" : "sg-0123456789abcdef0",
      "api_task_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-api-task",
      "api_execution_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-api-execution",
      "api_log_group" : "",
      "worker_security_group_id" : "sg-0123456789abcdef0",
      "worker_task_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-worker-task",
      "worker_execution_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-worker-execution",
      "worker_log_group" : "/ecs/ernote-staging/worker",
      "migration_security_group_id" : "sg-0123456789abcdef0",
      "migration_task_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-migration-task",
      "migration_execution_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-migration-execution",
      "migration_log_group" : "/ecs/ernote-staging/migration",
      "onboarding_security_group_id" : "sg-0123456789abcdef0"
    }
  }
  expect_failures = [terraform_data.contract]
}
run "reject_wrong_role" {
  command = plan
  variables {
    resources = {
      "deploy_artifacts_bucket" : "ernote-deploy-synthetic",
      "vpc_id" : "vpc-0123456789abcdef0",
      "ecr_repository_url" : "000000000000.dkr.ecr.us-east-2.amazonaws.com/ernote-staging",
      "ecs_cluster_arn" : "arn:aws:ecs:us-east-2:000000000000:cluster/ernote-staging",
      "private_subnet_ids" : "[\"subnet-0123456789abcdef0\", \"subnet-0123456789abcdef1\"]",
      "api_target_group_arn" : "arn:aws:elasticloadbalancing:us-east-2:000000000000:targetgroup/ernote-staging-api/1234567890abcdef",
      "api_https_port" : "8443",
      "db_endpoint" : "synthetic.us-east-2.rds.amazonaws.com",
      "db_port" : "5432",
      "db_name" : "ernote",
      "audio_bucket" : "ernote-staging-audio-synthetic",
      "stt_queue_name" : "ernote-staging-stt",
      "cognito_region" : "us-east-2",
      "cognito_pool_id" : "us-east-2_Synthetic",
      "cognito_client_id" : "syntheticclient",
      "api_security_group_id" : "sg-0123456789abcdef0",
      "api_task_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-api-task",
      "api_execution_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-api-execution",
      "api_log_group" : "/ecs/ernote-staging/api",
      "worker_security_group_id" : "sg-0123456789abcdef0",
      "worker_task_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-api-task",
      "worker_execution_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-worker-execution",
      "worker_log_group" : "/ecs/ernote-staging/worker",
      "migration_security_group_id" : "sg-0123456789abcdef0",
      "migration_task_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-migration-task",
      "migration_execution_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-migration-execution",
      "migration_log_group" : "/ecs/ernote-staging/migration",
      "onboarding_security_group_id" : "sg-0123456789abcdef0"
    }
  }
  expect_failures = [terraform_data.contract]
}
run "reject_wrong_secret" {
  command = plan
  variables {
    secret_refs = {
      "api_db" : "arn:aws:secretsmanager:us-east-2:000000000000:secret:ernote-staging/db/worker-AbCdEf",
      "worker_db" : "arn:aws:secretsmanager:us-east-2:000000000000:secret:ernote-staging/db/worker-AbCdEf",
      "migration_db" : "arn:aws:secretsmanager:us-east-2:000000000000:secret:ernote-staging/db/migration-AbCdEf",
      "deepgram" : "arn:aws:secretsmanager:us-east-2:000000000000:secret:ernote-staging/deepgram-AbCdEf"
    }
  }
  expect_failures = [terraform_data.contract]
}
run "reject_malformed_subnets" {
  command = plan
  variables {
    resources = {
      "deploy_artifacts_bucket" : "ernote-deploy-synthetic",
      "vpc_id" : "vpc-0123456789abcdef0",
      "ecr_repository_url" : "000000000000.dkr.ecr.us-east-2.amazonaws.com/ernote-staging",
      "ecs_cluster_arn" : "arn:aws:ecs:us-east-2:000000000000:cluster/ernote-staging",
      "private_subnet_ids" : "subnet-one,subnet-two",
      "api_target_group_arn" : "arn:aws:elasticloadbalancing:us-east-2:000000000000:targetgroup/ernote-staging-api/1234567890abcdef",
      "api_https_port" : "8443",
      "db_endpoint" : "synthetic.us-east-2.rds.amazonaws.com",
      "db_port" : "5432",
      "db_name" : "ernote",
      "audio_bucket" : "ernote-staging-audio-synthetic",
      "stt_queue_name" : "ernote-staging-stt",
      "cognito_region" : "us-east-2",
      "cognito_pool_id" : "us-east-2_Synthetic",
      "cognito_client_id" : "syntheticclient",
      "api_security_group_id" : "sg-0123456789abcdef0",
      "api_task_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-api-task",
      "api_execution_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-api-execution",
      "api_log_group" : "/ecs/ernote-staging/api",
      "worker_security_group_id" : "sg-0123456789abcdef0",
      "worker_task_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-worker-task",
      "worker_execution_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-worker-execution",
      "worker_log_group" : "/ecs/ernote-staging/worker",
      "migration_security_group_id" : "sg-0123456789abcdef0",
      "migration_task_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-migration-task",
      "migration_execution_role_arn" : "arn:aws:iam::000000000000:role/ernote-staging-migration-execution",
      "migration_log_group" : "/ecs/ernote-staging/migration",
      "onboarding_security_group_id" : "sg-0123456789abcdef0"
    }
  }
  expect_failures = [terraform_data.contract]
}
run "reject_exhausted_connection_budget" {
  command = plan
  variables { db_connection_budget = 25 }
  expect_failures = [var.db_connection_budget]
}
run "reject_mutable_image" {
  command = plan
  variables { image_digest = "latest" }
  expect_failures = [var.image_digest]
}
