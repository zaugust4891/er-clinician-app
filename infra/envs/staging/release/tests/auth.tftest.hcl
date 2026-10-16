# S7 release-consumer gate. Static fixtures are intentional: Terraform 1.10
# override values cannot reference variables. Every AWS read is mocked; no state
# backend, real manifest, or credentials are used.
mock_provider "aws" {
  mock_data "aws_caller_identity" {
    defaults = { account_id = "000000000000" }
  }
}

variables {
  aws_account_id          = "000000000000"
  deploy_artifacts_bucket = "ernote-deploy-synthetic"
}

run "complete_auth_contract" {
  command = plan
  override_data {
    target = data.aws_s3_object.manifest[0]
    values = {
      version_id = "synthetic-version"
      body       = <<-JSON
        {
          "schema_version": "1.0",
          "project": "ernote",
          "environment": "staging",
          "account_id": "000000000000",
          "region": "us-east-2",
          "generated_by": "synthetic-s7-test",
          "resources": {
            "deploy_artifacts_bucket": "synthetic",
            "vpc_id": "synthetic",
            "private_subnet_ids": "synthetic",
            "api_security_group_id": "synthetic",
            "worker_security_group_id": "synthetic",
            "migration_security_group_id": "synthetic",
            "onboarding_security_group_id": "synthetic",
            "api_https_port": "synthetic",
            "audio_bucket": "synthetic",
            "stt_queue_name": "synthetic",
            "db_endpoint": "synthetic",
            "db_port": "synthetic",
            "db_name": "synthetic",
            "api_task_role_arn": "synthetic",
            "worker_task_role_arn": "synthetic",
            "migration_task_role_arn": "synthetic",
            "api_execution_role_arn": "synthetic",
            "worker_execution_role_arn": "synthetic",
            "migration_execution_role_arn": "synthetic",
            "cognito_pool_id": "us-east-2_Synthetic",
            "cognito_client_id": "syntheticclient",
            "cognito_region": "us-east-2"
          },
          "secret_refs": {
            "api_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-api_db-AbCdEf",
            "worker_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-worker_db-AbCdEf",
            "migration_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-migration_db-AbCdEf",
            "deepgram": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-deepgram-AbCdEf"
          }
        }
      JSON
    }
  }
  assert {
    condition     = output.manifest_version_id == "synthetic-version"
    error_message = "The accepted release must retain the exact manifest version."
  }
}

run "missing_auth_contract" {
  command = plan
  override_data {
    target = data.aws_s3_object.manifest[0]
    values = {
      version_id = "synthetic-version"
      body       = <<-JSON
        {
          "schema_version": "1.0",
          "project": "ernote",
          "environment": "staging",
          "account_id": "000000000000",
          "region": "us-east-2",
          "generated_by": "synthetic-s7-test",
          "resources": {
            "deploy_artifacts_bucket": "synthetic",
            "vpc_id": "synthetic",
            "private_subnet_ids": "synthetic",
            "api_security_group_id": "synthetic",
            "worker_security_group_id": "synthetic",
            "migration_security_group_id": "synthetic",
            "onboarding_security_group_id": "synthetic",
            "api_https_port": "synthetic",
            "audio_bucket": "synthetic",
            "stt_queue_name": "synthetic",
            "db_endpoint": "synthetic",
            "db_port": "synthetic",
            "db_name": "synthetic",
            "api_task_role_arn": "synthetic",
            "worker_task_role_arn": "synthetic",
            "migration_task_role_arn": "synthetic",
            "api_execution_role_arn": "synthetic",
            "worker_execution_role_arn": "synthetic",
            "migration_execution_role_arn": "synthetic"
          },
          "secret_refs": {
            "api_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-api_db-AbCdEf",
            "worker_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-worker_db-AbCdEf",
            "migration_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-migration_db-AbCdEf",
            "deepgram": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-deepgram-AbCdEf"
          }
        }
      JSON
    }
  }
  expect_failures = [terraform_data.manifest_guard]
}

run "wrong_cognito_region" {
  command = plan
  override_data {
    target = data.aws_s3_object.manifest[0]
    values = {
      version_id = "synthetic-version"
      body       = <<-JSON
        {
          "schema_version": "1.0",
          "project": "ernote",
          "environment": "staging",
          "account_id": "000000000000",
          "region": "us-east-2",
          "generated_by": "synthetic-s7-test",
          "resources": {
            "deploy_artifacts_bucket": "synthetic",
            "vpc_id": "synthetic",
            "private_subnet_ids": "synthetic",
            "api_security_group_id": "synthetic",
            "worker_security_group_id": "synthetic",
            "migration_security_group_id": "synthetic",
            "onboarding_security_group_id": "synthetic",
            "api_https_port": "synthetic",
            "audio_bucket": "synthetic",
            "stt_queue_name": "synthetic",
            "db_endpoint": "synthetic",
            "db_port": "synthetic",
            "db_name": "synthetic",
            "api_task_role_arn": "synthetic",
            "worker_task_role_arn": "synthetic",
            "migration_task_role_arn": "synthetic",
            "api_execution_role_arn": "synthetic",
            "worker_execution_role_arn": "synthetic",
            "migration_execution_role_arn": "synthetic",
            "cognito_pool_id": "us-east-2_Synthetic",
            "cognito_client_id": "syntheticclient",
            "cognito_region": "us-east-1"
          },
          "secret_refs": {
            "api_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-api_db-AbCdEf",
            "worker_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-worker_db-AbCdEf",
            "migration_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-migration_db-AbCdEf",
            "deepgram": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-deepgram-AbCdEf"
          }
        }
      JSON
    }
  }
  expect_failures = [terraform_data.manifest_guard]
}

run "pool_from_another_region" {
  command = plan
  override_data {
    target = data.aws_s3_object.manifest[0]
    values = {
      version_id = "synthetic-version"
      body       = <<-JSON
        {
          "schema_version": "1.0",
          "project": "ernote",
          "environment": "staging",
          "account_id": "000000000000",
          "region": "us-east-2",
          "generated_by": "synthetic-s7-test",
          "resources": {
            "deploy_artifacts_bucket": "synthetic",
            "vpc_id": "synthetic",
            "private_subnet_ids": "synthetic",
            "api_security_group_id": "synthetic",
            "worker_security_group_id": "synthetic",
            "migration_security_group_id": "synthetic",
            "onboarding_security_group_id": "synthetic",
            "api_https_port": "synthetic",
            "audio_bucket": "synthetic",
            "stt_queue_name": "synthetic",
            "db_endpoint": "synthetic",
            "db_port": "synthetic",
            "db_name": "synthetic",
            "api_task_role_arn": "synthetic",
            "worker_task_role_arn": "synthetic",
            "migration_task_role_arn": "synthetic",
            "api_execution_role_arn": "synthetic",
            "worker_execution_role_arn": "synthetic",
            "migration_execution_role_arn": "synthetic",
            "cognito_pool_id": "us-east-1_Synthetic",
            "cognito_client_id": "syntheticclient",
            "cognito_region": "us-east-2"
          },
          "secret_refs": {
            "api_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-api_db-AbCdEf",
            "worker_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-worker_db-AbCdEf",
            "migration_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-migration_db-AbCdEf",
            "deepgram": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-deepgram-AbCdEf"
          }
        }
      JSON
    }
  }
  expect_failures = [terraform_data.manifest_guard]
}

run "empty_client_id" {
  command = plan
  override_data {
    target = data.aws_s3_object.manifest[0]
    values = {
      version_id = "synthetic-version"
      body       = <<-JSON
        {
          "schema_version": "1.0",
          "project": "ernote",
          "environment": "staging",
          "account_id": "000000000000",
          "region": "us-east-2",
          "generated_by": "synthetic-s7-test",
          "resources": {
            "deploy_artifacts_bucket": "synthetic",
            "vpc_id": "synthetic",
            "private_subnet_ids": "synthetic",
            "api_security_group_id": "synthetic",
            "worker_security_group_id": "synthetic",
            "migration_security_group_id": "synthetic",
            "onboarding_security_group_id": "synthetic",
            "api_https_port": "synthetic",
            "audio_bucket": "synthetic",
            "stt_queue_name": "synthetic",
            "db_endpoint": "synthetic",
            "db_port": "synthetic",
            "db_name": "synthetic",
            "api_task_role_arn": "synthetic",
            "worker_task_role_arn": "synthetic",
            "migration_task_role_arn": "synthetic",
            "api_execution_role_arn": "synthetic",
            "worker_execution_role_arn": "synthetic",
            "migration_execution_role_arn": "synthetic",
            "cognito_pool_id": "us-east-2_Synthetic",
            "cognito_client_id": "",
            "cognito_region": "us-east-2"
          },
          "secret_refs": {
            "api_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-api_db-AbCdEf",
            "worker_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-worker_db-AbCdEf",
            "migration_db": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-migration_db-AbCdEf",
            "deepgram": "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-deepgram-AbCdEf"
          }
        }
      JSON
    }
  }
  expect_failures = [terraform_data.manifest_guard]
}

