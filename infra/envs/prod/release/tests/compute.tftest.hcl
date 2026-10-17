mock_provider "aws" {
  mock_data "aws_caller_identity" { defaults = { account_id = "000000000000" } }
}
variables {
  aws_account_id          = "000000000000"
  deploy_artifacts_bucket = "ernote-deploy-synthetic"
}
override_data {
  target = data.aws_s3_object.manifest[0]
  values = {
    version_id = "synthetic-version"
    body       = <<-JSON
{
  "schema_version": "1.0",
  "project": "ernote",
  "environment": "prod",
  "account_id": "000000000000",
  "region": "us-east-2",
  "generated_by": "synthetic-s8-test",
  "resources": {
    "deploy_artifacts_bucket": "ernote-deploy-synthetic",
    "vpc_id": "vpc-0123456789abcdef0",
    "ecr_repository_url": "000000000000.dkr.ecr.us-east-2.amazonaws.com/ernote-prod",
    "ecs_cluster_arn": "arn:aws:ecs:us-east-2:000000000000:cluster/ernote-prod",
    "private_subnet_ids": "[\"subnet-0123456789abcdef0\", \"subnet-0123456789abcdef1\"]",
    "api_target_group_arn": "arn:aws:elasticloadbalancing:us-east-2:000000000000:targetgroup/ernote-prod-api/1234567890abcdef",
    "api_https_port": "8443",
    "db_endpoint": "synthetic.us-east-2.rds.amazonaws.com",
    "db_port": "5432",
    "db_name": "ernote",
    "audio_bucket": "ernote-prod-audio-synthetic",
    "stt_queue_name": "ernote-prod-stt",
    "cognito_region": "us-east-2",
    "cognito_pool_id": "us-east-2_Synthetic",
    "cognito_client_id": "syntheticclient",
    "api_security_group_id": "sg-0123456789abcdef0",
    "api_task_role_arn": "arn:aws:iam::000000000000:role/ernote-prod-api-task",
    "api_execution_role_arn": "arn:aws:iam::000000000000:role/ernote-prod-api-execution",
    "api_log_group": "/ecs/ernote-prod/api",
    "worker_security_group_id": "sg-0123456789abcdef0",
    "worker_task_role_arn": "arn:aws:iam::000000000000:role/ernote-prod-worker-task",
    "worker_execution_role_arn": "arn:aws:iam::000000000000:role/ernote-prod-worker-execution",
    "worker_log_group": "/ecs/ernote-prod/worker",
    "migration_security_group_id": "sg-0123456789abcdef0",
    "migration_task_role_arn": "arn:aws:iam::000000000000:role/ernote-prod-migration-task",
    "migration_execution_role_arn": "arn:aws:iam::000000000000:role/ernote-prod-migration-execution",
    "migration_log_group": "/ecs/ernote-prod/migration",
    "onboarding_security_group_id": "sg-0123456789abcdef0"
  },
  "secret_refs": {
    "api_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:ernote-prod/db/api-AbCdEf",
    "worker_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:ernote-prod/db/worker-AbCdEf",
    "migration_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:ernote-prod/db/migration-AbCdEf",
    "deepgram": "arn:aws:secretsmanager:us-east-2:000000000000:secret:ernote-prod/deepgram-AbCdEf"
  }
}
JSON
  }
}
run "services_off_by_default" {
  command = plan
  assert {
    condition     = output.compute == null
    error_message = "No candidate or service is deployed by default."
  }
}
run "pinned_candidate_renders_before_activation" {
  command = apply
  variables {
    enabled = true

    image_digest        = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    manifest_version_id = "synthetic-version"
  }
  assert {
    condition     = length(output.compute.task_templates) == 3 && length(output.compute.services) == 0 && output.manifest_version_id == "synthetic-version"
    error_message = "S11 needs a pinned migration template before activating API/worker."
  }
}
run "reject_unpinned_manifest" {
  command = plan
  variables {
    enabled = true

    image_digest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
  }
  expect_failures = [terraform_data.release_guard]
}
run "reject_activation_without_image" {
  command = plan
  variables {
    enabled = true

    services_enabled = true
  }
  expect_failures = [terraform_data.release_guard]
}
run "reject_production_activation_while_disabled" {
  command = plan
  variables {
    services_enabled    = true
    image_digest        = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    manifest_version_id = "synthetic-version"
  }
  expect_failures = [terraform_data.release_guard]
}
