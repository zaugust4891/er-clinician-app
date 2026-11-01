mock_provider "aws" {
  mock_resource "aws_secretsmanager_secret" {
    defaults = { arn = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-AbCdEf" }
  }
}
variables {
  name                      = "ernote-staging"
  aws_region                = "us-east-2"
  aws_account_id            = "000000000000"
  kms_key_arn               = "arn:aws:kms:us-east-2:000000000000:key/00000000-0000-0000-0000-000000000000"
  audio_bucket_arn          = "arn:aws:s3:::ernote-staging-audio"
  stt_queue_arn             = "arn:aws:sqs:us-east-2:000000000000:ernote-staging-jobs"
  stt_dlq_arn               = "arn:aws:sqs:us-east-2:000000000000:ernote-staging-jobs-dlq"
  db_master_user_secret_arn = "arn:aws:secretsmanager:us-east-2:000000000000:secret:rds!db-synthetic-AbCdEf"
  ci = {
    repository              = "synthetic/ernote"
    environment             = "staging"
    smoke_secret_arn        = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-smoke-AbCdEf"
    smoke_kms_key_arn       = "arn:aws:kms:us-east-2:000000000000:key/00000000-0000-0000-0000-000000000000"
    deploy_artifacts_bucket = "synthetic-deploy"
    deploy_kms_key_arn      = "arn:aws:kms:us-east-2:000000000000:key/00000000-0000-0000-0000-000000000000"
    private_subnet_ids      = ["subnet-00000000000000001", "subnet-00000000000000002"]
    subjects = {
      release = "repo:synthetic@123/ernote@456:environment:staging"
    }
  }
}
run "exact_subject_private_migration_and_artifact_scopes" {
  command = apply
  assert {
    condition     = jsondecode(aws_iam_role.ci["release"].assume_role_policy).Statement[0].Condition.StringEquals["token.actions.githubusercontent.com:sub"] == "repo:synthetic@123/ernote@456:environment:staging"
    error_message = "Immutable repository identifiers must be preserved in exact OIDC trust."
  }
  assert {
    condition = (
      one([for s in jsondecode(aws_iam_role_policy.ci_release[0].policy).Statement : s if s.Sid == "RunMigrationOnly"]).Condition.Bool["ecs:auto-assign-public-ip"] == "false" &&
      length(one([for s in jsondecode(aws_iam_role_policy.ci_release[0].policy).Statement : s if s.Sid == "RunMigrationOnly"]).Condition["ForAllValues:StringEquals"]["ecs:subnet"]) == 2 &&
      one([for s in jsondecode(aws_iam_role_policy.ci_release[0].policy).Statement : s if s.Sid == "ListClusterTasks"]).Condition.ArnEquals["ecs:cluster"] == "arn:aws:ecs:us-east-2:000000000000:cluster/ernote-staging"
    )
    error_message = "Migration submission and task discovery must be confined to private staging placement."
  }
  assert {
    condition = (
      jsondecode(aws_iam_role_policy.ci_artifacts["smoke"].policy).Statement[0].Action == ["s3:GetObject"] &&
      jsondecode(aws_iam_role_policy.ci_artifacts["smoke"].policy).Statement[0].Resource == ["arn:aws:s3:::synthetic-deploy/envs/staging/fixtures/smoke.wav"] &&
      jsondecode(aws_iam_role_policy.ci_artifacts["release"].policy).Statement[0].Resource == ["arn:aws:s3:::synthetic-deploy/envs/staging/accepted-release.json", "arn:aws:s3:::synthetic-deploy/envs/staging/releases/*"]
    )
    error_message = "Smoke reads one approved fixture; release can publish only staging release records."
  }
}
run "wrong_repository_subject_is_rejected" {
  command = plan
  variables {
    ci = {
      repository        = "synthetic/ernote"
      environment       = "staging"
      smoke_secret_arn  = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-smoke-AbCdEf"
      smoke_kms_key_arn = "arn:aws:kms:us-east-2:000000000000:key/00000000-0000-0000-0000-000000000000"
      subjects          = { release = "repo:attacker/project:environment:staging" }
    }
  }
  expect_failures = [var.ci]
}
