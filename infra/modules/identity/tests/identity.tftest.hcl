# Mocked apply: no AWS credentials and nothing created. Synthetic identifiers only.
# Policies are built with jsonencode, so these assertions read the exact documents
# IAM, KMS and Secrets Manager would receive.
mock_provider "aws" {
}

variables {
  name                      = "ernote-synthetic-test"
  aws_region                = "us-east-2"
  aws_account_id            = "000000000000"
  kms_key_arn               = "arn:aws:kms:us-east-2:000000000000:key/00000000-0000-0000-0000-000000000000"
  audio_bucket_arn          = "arn:aws:s3:::ernote-synthetic-test-audio"
  stt_queue_arn             = "arn:aws:sqs:us-east-2:000000000000:ernote-synthetic-test-jobs"
  stt_dlq_arn               = "arn:aws:sqs:us-east-2:000000000000:ernote-synthetic-test-jobs-dlq"
  db_master_user_secret_arn = "arn:aws:secretsmanager:us-east-2:000000000000:secret:rds!db-synthetic-AbCdEf"
}

run "runtime_roles_follow_the_code_paths" {
  command = apply

  assert {
    condition = alltrue([
      for role in concat(values(aws_iam_role.task), values(aws_iam_role.execution)) :
      length(jsondecode(role.assume_role_policy).Statement) == 1 &&
      jsondecode(role.assume_role_policy).Statement[0].Principal.Service == "ecs-tasks.amazonaws.com" &&
      jsondecode(role.assume_role_policy).Statement[0].Condition.StringEquals["aws:SourceAccount"] == "000000000000" &&
      jsondecode(role.assume_role_policy).Statement[0].Condition.ArnLike["aws:SourceArn"] == "arn:aws:ecs:us-east-2:000000000000:*"
    ])
    error_message = "Every role must trust only ECS tasks in this account and Region."
  }
  assert {
    condition = (sort(distinct(flatten([
      for statement in jsondecode(aws_iam_role_policy.task["api"].policy).Statement : statement.Action if statement.Effect == "Allow"
      ]))) == sort(["kms:Decrypt", "kms:GenerateDataKey", "s3:GetObject", "s3:ListBucket", "s3:PutObject", "sqs:GetQueueUrl", "sqs:SendMessage"]) &&
      one([for statement in jsondecode(aws_iam_role_policy.task["api"].policy).Statement : statement if statement.Sid == "DistinguishMissingUploads"]).Resource == var.audio_bucket_arn &&
    one([for statement in jsondecode(aws_iam_role_policy.task["api"].policy).Statement : statement if statement.Sid == "EnqueueJobs"]).Resource == var.stt_queue_arn)
    error_message = "The API signs and verifies uploads, lists only its audio bucket and sends only to the main queue."
  }
  assert {
    condition = (sort(distinct(flatten([
      for statement in jsondecode(aws_iam_role_policy.task["worker"].policy).Statement : statement.Action if statement.Effect == "Allow"
      ]))) == sort(["bedrock:InvokeModel", "kms:Decrypt", "s3:GetObject", "sqs:ChangeMessageVisibility", "sqs:DeleteMessage", "sqs:GetQueueUrl", "sqs:ReceiveMessage"]) &&
      one([for statement in jsondecode(aws_iam_role_policy.task["worker"].policy).Statement : statement if statement.Sid == "ConsumeJobs"]).Resource == var.stt_queue_arn &&
    sort(one([for statement in jsondecode(aws_iam_role_policy.task["worker"].policy).Statement : statement if statement.Sid == "ResolveQueues"]).Resource) == sort([var.stt_queue_arn, var.stt_dlq_arn]))
    error_message = "The worker reads audio, consumes only the main queue and looks up both queues."
  }
  assert {
    condition = (
      sort(one([for statement in jsondecode(aws_iam_role_policy.task["api"].policy).Statement : statement if statement.Sid == "EncryptUploads"]).Action) == sort(["kms:GenerateDataKey", "kms:Decrypt"]) &&
      alltrue([
        for component, sid in { api = "EncryptUploads", worker = "DecryptAudio" } :
        one([for statement in jsondecode(aws_iam_role_policy.task[component].policy).Statement : statement if statement.Sid == sid]).Resource == var.kms_key_arn &&
        one([for statement in jsondecode(aws_iam_role_policy.task[component].policy).Statement : statement if statement.Sid == sid]).Condition.StringEquals["kms:ViaService"] == "s3.us-east-2.amazonaws.com" &&
        sort(one([for statement in jsondecode(aws_iam_role_policy.task[component].policy).Statement : statement if statement.Sid == sid]).Condition.StringLike["kms:EncryptionContext:aws:s3:arn"]) == sort([var.audio_bucket_arn, "${var.audio_bucket_arn}/*"])
      ]) &&
      alltrue([
        for component, sid in { api = "EncryptJobs", worker = "DecryptJobs" } :
        one([for statement in jsondecode(aws_iam_role_policy.task[component].policy).Statement : statement if statement.Sid == sid]).Resource == var.kms_key_arn &&
        one([for statement in jsondecode(aws_iam_role_policy.task[component].policy).Statement : statement if statement.Sid == sid]).Condition.StringEquals["kms:ViaService"] == "sqs.us-east-2.amazonaws.com" &&
        one([for statement in jsondecode(aws_iam_role_policy.task[component].policy).Statement : statement if statement.Sid == sid]).Condition.StringEquals["kms:EncryptionContext:aws:sqs:arn"] == var.stt_queue_arn
      ])
    )
    error_message = "Bucket Key uploads need GenerateDataKey and Decrypt, and runtime KMS access must retain the exact audio/queue service and context scopes."
  }
  assert {
    condition = (
      one([for statement in jsondecode(aws_iam_role_policy.task["worker"].policy).Statement : statement if statement.Sid == "InvokeApprovedProfile"]).Resource == "arn:aws:bedrock:us-east-2:000000000000:inference-profile/us.anthropic.claude-sonnet-4-6" &&
      sort(one([for statement in jsondecode(aws_iam_role_policy.task["worker"].policy).Statement : statement if statement.Sid == "InvokeProfileDestinations"]).Resource) == sort([
        "arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-sonnet-4-6",
        "arn:aws:bedrock:us-east-2::foundation-model/anthropic.claude-sonnet-4-6",
        "arn:aws:bedrock:us-west-2::foundation-model/anthropic.claude-sonnet-4-6",
      ]) &&
      one([for statement in jsondecode(aws_iam_role_policy.task["worker"].policy).Statement : statement if statement.Sid == "InvokeProfileDestinations"]).Condition.StringEquals["bedrock:InferenceProfileArn"] == "arn:aws:bedrock:us-east-2:000000000000:inference-profile/us.anthropic.claude-sonnet-4-6"
    )
    error_message = "Bedrock is limited to the approved US profile and its destination models, reachable only through that profile."
  }
  assert {
    condition = alltrue([
      for component, denied in {
        api        = ["bedrock:*", "secretsmanager:*", "sqs:ReceiveMessage", "sqs:DeleteMessage", "sqs:PurgeQueue", "sqs:CreateQueue", "s3:CreateBucket"]
        worker     = ["s3:PutObject", "cognito-idp:*", "iam:*", "sqs:SetQueueAttributes", "kms:PutKeyPolicy"]
        migration  = ["bedrock:*", "sqs:*", "s3:*", "secretsmanager:*"]
        onboarding = ["bedrock:*", "sqs:*", "s3:*"]
        } : length(setsubtract(denied, one([
          for statement in jsondecode(aws_iam_role_policy.task[component].policy).Statement : statement if statement.Effect == "Deny"
      ]).Action)) == 0
    ])
    error_message = "Each task role must explicitly deny the actions outside its boundary."
  }
  assert {
    condition = alltrue(flatten([
      for policy in concat(values(aws_iam_role_policy.task), values(aws_iam_role_policy.execution)) : [
        for statement in jsondecode(policy.policy).Statement :
        alltrue([for action in flatten([statement.Action]) : !strcontains(action, "*")]) &&
        (statement.Action == "ecr:GetAuthorizationToken" || !contains(flatten([statement.Resource]), "*"))
        if statement.Effect == "Allow"
      ]
    ]))
    error_message = "No allow may use an action wildcard such as kms:*, or Resource \"*\" outside registry authentication."
  }
  assert {
    condition = (length([for statement in jsondecode(aws_iam_role_policy.task["migration"].policy).Statement : statement if statement.Effect == "Allow"]) == 0 &&
    length([for statement in jsondecode(aws_iam_role_policy.task["onboarding"].policy).Statement : statement if statement.Effect == "Allow"]) == 0)
    error_message = "Migration makes no AWS call, and onboarding has no Cognito access until S7 supplies its pool."
  }
  assert {
    condition = (
      sort(one([for statement in jsondecode(aws_iam_role_policy.execution["api"].policy).Statement : statement if statement.Sid == "InjectOwnSecrets"]).Resource) == sort([aws_secretsmanager_secret.this["api_db"].arn]) &&
      sort(one([for statement in jsondecode(aws_iam_role_policy.execution["worker"].policy).Statement : statement if statement.Sid == "InjectOwnSecrets"]).Resource) == sort([aws_secretsmanager_secret.this["worker_db"].arn, aws_secretsmanager_secret.this["deepgram"].arn]) &&
      sort(one([for statement in jsondecode(aws_iam_role_policy.execution["migration"].policy).Statement : statement if statement.Sid == "InjectOwnSecrets"]).Resource) == sort([aws_secretsmanager_secret.this["migration_db"].arn]) &&
      sort(one([for statement in jsondecode(aws_iam_role_policy.execution["api"].policy).Statement : statement if statement.Sid == "DenyOtherSecrets"]).NotResource) == sort([aws_secretsmanager_secret.this["api_db"].arn]) &&
      sort(one([for statement in jsondecode(aws_iam_role_policy.execution["worker"].policy).Statement : statement if statement.Sid == "DecryptOwnSecrets"]).Condition.StringEquals["kms:EncryptionContext:SecretARN"]) == sort([aws_secretsmanager_secret.this["worker_db"].arn, aws_secretsmanager_secret.this["deepgram"].arn]) &&
      one([for statement in jsondecode(aws_iam_role_policy.execution["worker"].policy).Statement : statement if statement.Sid == "DecryptOwnSecrets"]).Condition.StringEquals["kms:ViaService"] == "secretsmanager.us-east-2.amazonaws.com"
    )
    error_message = "Each service's execution role injects and decrypts only its own secrets: no Deepgram key for the API, no other login for the worker."
  }
  assert {
    condition = (

      sort(one([for statement in jsondecode(aws_iam_role_policy.execution["db-bootstrap"].policy).Statement : statement if statement.Sid == "InjectBootstrapLogins"]).Resource) == sort([for key in ["api_db", "migration_db", "worker_db", "onboarding_db"] : aws_secretsmanager_secret.this[key].arn]) &&
      one([for statement in jsondecode(aws_iam_role_policy.execution["db-bootstrap"].policy).Statement : statement if statement.Sid == "InjectMasterCredential"]).Resource == var.db_master_user_secret_arn &&
      length([for statement in jsondecode(aws_iam_role_policy.execution["db-bootstrap"].policy).Statement : statement if statement.Sid == "InjectOwnSecrets"]) == 0 &&
      one([for statement in jsondecode(aws_iam_role_policy.execution["onboarding"].policy).Statement : statement if statement.Sid == "InjectOwnSecrets"]).Resource == [aws_secretsmanager_secret.this["onboarding_db"].arn] &&
      alltrue([
        for component in ["api", "worker", "migration", "onboarding"] :
        !strcontains(aws_iam_role_policy.execution[component].policy, "rds!db-synthetic")
      ])
    )
    error_message = "Only the bootstrap receives the master credential, and it receives only the four managed logins and the master."
  }
  assert {
    condition = (length(aws_secretsmanager_secret.this) == 5 &&
      alltrue([
        for secret in values(aws_secretsmanager_secret.this) :
        secret.kms_key_id == var.kms_key_arn && secret.recovery_window_in_days == 7 && !strcontains(lower(secret.name), "anthropic")
      ]) &&
      aws_secretsmanager_secret.this["deepgram"].name == "ernote-synthetic-test/deepgram" &&
      sort(jsondecode(aws_secretsmanager_secret_policy.this["deepgram"].policy).Statement[0].Condition.StringNotEquals["aws:PrincipalArn"]) == sort([aws_iam_role.execution["worker"].arn]) &&
    sort(jsondecode(aws_secretsmanager_secret_policy.this["api_db"].policy).Statement[0].Condition.StringNotEquals["aws:PrincipalArn"]) == sort([aws_iam_role.execution["api"].arn, aws_iam_role.execution["db-bootstrap"].arn]))
    error_message = "Five value-free containers under the data key, no Anthropic secret, and reads denied outside each secret's injecting roles."
  }
  assert {
    condition = (length(output.kms_key_policy_statements) == 5 &&
      alltrue([
        for statement in output.kms_key_policy_statements :
        statement.Effect == "Allow" && statement.Resource == "*" && !contains(statement.Action, "kms:*") &&
        statement.Principal.AWS == "arn:aws:iam::000000000000:root" &&
        length(statement.Condition.StringEquals["aws:PrincipalArn"]) > 0 &&
        statement.Condition.StringEquals["kms:CallerAccount"] == "000000000000" &&
        endswith(statement.Condition.StringEquals["kms:ViaService"], ".us-east-2.amazonaws.com")
      ]) &&
      sort(one([for statement in output.kms_key_policy_statements : statement if statement.Sid == "AudioUploadsViaS3"]).Condition.StringEquals["aws:PrincipalArn"]) == sort([aws_iam_role.task["api"].arn]) &&
      sort(one([for statement in output.kms_key_policy_statements : statement if statement.Sid == "AudioUploadsViaS3"]).Action) == sort(["kms:GenerateDataKey", "kms:Decrypt"]) &&
      one([for statement in output.kms_key_policy_statements : statement if statement.Sid == "JobSendsViaSqs"]).Condition.StringEquals["kms:ViaService"] == "sqs.us-east-2.amazonaws.com" &&
    sort(one([for statement in output.kms_key_policy_statements : statement if statement.Sid == "SecretInjection"]).Condition.StringEquals["aws:PrincipalArn"]) == sort([for component in ["api", "db-bootstrap", "migration", "worker", "onboarding"] : aws_iam_role.execution[component].arn]))
    error_message = "Key grants must delegate to IAM with exact role/service conditions; direct role principals would bypass IAM's encryption-context restrictions."
  }
  assert {
    condition = (output.ecs_secrets.api.DB_PASSWORD == "${aws_secretsmanager_secret.this["api_db"].arn}:password::" &&
      output.ecs_secrets.worker.DEEPGRAM_API_KEY == "${aws_secretsmanager_secret.this["deepgram"].arn}:api_key::" &&
      output.ecs_secrets["db-bootstrap"].DB_PASSWORD == "${var.db_master_user_secret_arn}:password::" &&
      output.ecs_secrets["db-bootstrap"].API_DB_PASSWORD == "${aws_secretsmanager_secret.this["api_db"].arn}:password:AWSPENDING:" &&
    !contains(keys(output.ecs_secrets.api), "DEEPGRAM_API_KEY"))
    error_message = "Task definitions inject each login from its own JSON secret, and the bootstrap reads staged logins."
  }
}

run "onboarding_reads_user_subjects_once_s7_supplies_a_pool" {
  command = apply
  variables {
    cognito_user_pool_arn = "arn:aws:cognito-idp:us-east-2:000000000000:userpool/us-east-2_Synthetic1"
  }

  assert {
    condition = (one([for statement in jsondecode(aws_iam_role_policy.task["onboarding"].policy).Statement : statement if statement.Effect == "Allow"]).Action == "cognito-idp:AdminGetUser" &&
    one([for statement in jsondecode(aws_iam_role_policy.task["onboarding"].policy).Statement : statement if statement.Effect == "Allow"]).Resource == var.cognito_user_pool_arn)
    error_message = "Onboarding may read user subjects in S7's pool and nothing else in Cognito."
  }
}

run "reject_global_bedrock_profile" {
  command = plan
  variables { bedrock_inference_profile_id = "global.anthropic.claude-sonnet-4-6" }
  expect_failures = [var.bedrock_inference_profile_id]
}

run "ci_templates_keep_release_smoke_and_infrastructure_separate" {
  command = apply
  variables {
    name = "ernote-staging"
    ci = {
      repository        = "synthetic/ernote"
      environment       = "staging"
      smoke_secret_arn  = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-smoke-AbCdEf"
      smoke_kms_key_arn = "arn:aws:kms:us-east-2:000000000000:key/00000000-0000-0000-0000-000000000000"
    }
  }
  assert {
    condition = length(aws_iam_role.ci) == 3 && alltrue([
      for component, environment in { infrastructure = "staging-infrastructure", release = "staging", smoke = "staging-smoke" } :
      jsondecode(aws_iam_role.ci[component].assume_role_policy).Statement[0].Condition.StringEquals["token.actions.githubusercontent.com:sub"] == "repo:synthetic/ernote:environment:${environment}" &&
      jsondecode(aws_iam_role.ci[component].assume_role_policy).Statement[0].Condition.StringEquals["token.actions.githubusercontent.com:aud"] == "sts.amazonaws.com"
    ])
    error_message = "Each CI role must trust only its exact repository and protected environment, never a branch/PR wildcard."
  }
  assert {
    condition = (
      aws_iam_role_policy_attachment.ci_state["release"].policy_arn == "arn:aws:iam::000000000000:policy/ernote-tfstate-staging-release" &&
      aws_iam_role_policy_attachment.ci_state["infrastructure"].policy_arn == "arn:aws:iam::000000000000:policy/ernote-tfstate-staging-foundation" &&
      sort(one([for statement in jsondecode(aws_iam_role_policy.ci_release[0].policy).Statement : statement if statement.Sid == "PassApplicationRolesOnly"]).Resource) == sort(flatten([
        for component in ["api", "worker", "migration"] : [aws_iam_role.task[component].arn, aws_iam_role.execution[component].arn]
      ])) &&
      one([for statement in jsondecode(aws_iam_role_policy.ci_release[0].policy).Statement : statement if statement.Sid == "PassApplicationRolesOnly"]).Condition.StringEquals["iam:PassedToService"] == "ecs-tasks.amazonaws.com" &&
      one([for statement in jsondecode(aws_iam_role_policy.ci_release[0].policy).Statement : statement if statement.Sid == "RunMigrationOnly"]).Resource == "arn:aws:ecs:us-east-2:000000000000:task-definition/ernote-staging-migration:*"
    )
    error_message = "Release gets only its state, six application roles and the migration family, with no bootstrap/onboarding PassRole."
  }
  assert {
    condition = alltrue([
      for policy in aws_iam_role_policy.ci_boundary :
      one([for statement in jsondecode(policy.policy).Statement : statement if statement.Sid == "DenyApplicationSecrets"]).Effect == "Deny" &&
      contains(one([for statement in jsondecode(policy.policy).Statement : statement if statement.Sid == "NoRoleChainingOrProviders"]).Action, "sts:AssumeRole")
    ]) && one([for statement in jsondecode(aws_iam_role_policy.ci_smoke[0].policy).Statement : statement if statement.Effect == "Deny"]).NotResource == var.ci.smoke_secret_arn
    error_message = "CI cannot chain to production or retrieve DB/provider credentials; smoke reads its single synthetic secret."
  }
}

run "reject_untrusted_ci_repository_wildcard" {
  command = plan
  variables {
    name = "ernote-staging"
    ci = {
      repository        = "synthetic/*"
      environment       = "staging"
      smoke_secret_arn  = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-smoke-AbCdEf"
      smoke_kms_key_arn = "arn:aws:kms:us-east-2:000000000000:key/00000000-0000-0000-0000-000000000000"
    }
  }
  expect_failures = [var.ci]
}
run "reject_dlq_name_drift" {
  command = plan
  variables { stt_dlq_arn = "arn:aws:sqs:us-east-2:000000000000:ernote-synthetic-test-failed" }
  expect_failures = [var.stt_dlq_arn]
}
run "reject_key_alias" {
  command = plan
  variables { kms_key_arn = "arn:aws:kms:us-east-2:000000000000:alias/ernote-synthetic-test-data" }
  expect_failures = [var.kms_key_arn]
}
run "reject_immediate_secret_deletion" {
  command = plan
  variables { secret_recovery_window_days = 0 }
  expect_failures = [var.secret_recovery_window_days]
}

override_resource {
  target = aws_secretsmanager_secret.this["migration_db"]
  values = { arn = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-migration_db-AbCdEf" }
}

override_resource {
  target = aws_secretsmanager_secret.this["api_db"]
  values = { arn = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-api_db-AbCdEf" }
}

override_resource {
  target = aws_secretsmanager_secret.this["worker_db"]
  values = { arn = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-worker_db-AbCdEf" }
}

override_resource {
  target = aws_secretsmanager_secret.this["deepgram"]
  values = { arn = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-deepgram-AbCdEf" }
}

override_resource {
  target = aws_secretsmanager_secret.this["onboarding_db"]
  values = { arn = "arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-onboarding_db-AbCdEf" }
}
