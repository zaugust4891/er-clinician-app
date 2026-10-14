# Mocked apply evaluates resource references, policies and lifecycle rules without AWS
# credentials and without creating anything. Synthetic identifiers only.
mock_provider "aws" {
  mock_resource "aws_kms_key" {
    # The RDS provider validates this ARN's shape, and a generated mock value is not a
    # valid ARN. Everything else about the key stays mocked.
    defaults = { arn = "arn:aws:kms:us-east-2:000000000000:key/00000000-0000-0000-0000-000000000000" }
  }
}

variables {
  name                  = "ernote-synthetic-test"
  aws_region            = "us-east-2"
  aws_account_id        = "000000000000"
  audio_bucket_name     = "ernote-synthetic-test-audio-000000000000-us-east-2"
  db_subnet_group_name  = "ernote-synthetic-test-db"
  db_security_group_ids = ["sg-synthetic-database"]
}

run "synthetic_staging_defaults" {
  command = apply

  # --- encryption -----------------------------------------------------------
  assert {
    condition = (aws_kms_key.data.enable_key_rotation &&
      aws_kms_key.data.deletion_window_in_days == 30 &&
      aws_kms_alias.data.name == "alias/ernote-synthetic-test-data" &&
    aws_kms_alias.data.target_key_id == aws_kms_key.data.key_id)
    error_message = "Application data needs its own rotating key, aliased, with a deletion window that outlives its backups."
  }
  assert {
    condition = (length(jsondecode(aws_kms_key.data.policy).Statement) == 1 &&
    jsondecode(aws_kms_key.data.policy).Statement[0].Principal.AWS == "arn:aws:iam::000000000000:root")
    error_message = "The key policy delegates to account IAM only; every service grant belongs to S6."
  }

  # --- database -------------------------------------------------------------
  assert {
    condition = (!aws_db_instance.this.publicly_accessible &&
      aws_db_instance.this.storage_encrypted &&
      aws_db_instance.this.kms_key_id == aws_kms_key.data.arn &&
      aws_db_instance.this.db_subnet_group_name == "ernote-synthetic-test-db" &&
    aws_db_instance.this.vpc_security_group_ids == toset(["sg-synthetic-database"]))
    error_message = "The database must be private, encrypted with the data key, and placed in S4's isolated subnets and SG."
  }
  assert {
    condition = (aws_db_instance.this.backup_retention_period == 7 &&
      aws_db_instance.this.backup_window == "07:00-07:30" &&
      aws_db_instance.this.maintenance_window == "sun:08:00-sun:09:00" &&
      aws_db_instance.this.copy_tags_to_snapshot &&
    !aws_db_instance.this.delete_automated_backups)
    error_message = "Automated backups must be on, scheduled, tagged, and not discarded when the instance is removed."
  }
  assert {
    condition = (aws_db_instance.this.deletion_protection &&
      !aws_db_instance.this.skip_final_snapshot &&
    aws_db_instance.this.final_snapshot_identifier == "ernote-synthetic-test-pg-final-v1")
    error_message = "Removal must require both turning off deletion protection and taking a named final snapshot."
  }
  assert {
    condition = (aws_db_instance.this.engine == "postgres" &&
      startswith(aws_db_instance.this.engine_version, "16") &&
      aws_db_instance.this.storage_type == "gp3" &&
      aws_db_instance.this.allocated_storage == 50 &&
      aws_db_instance.this.max_allocated_storage == 100 &&
      !aws_db_instance.this.multi_az &&
    aws_db_instance.this.parameter_group_name == aws_db_parameter_group.postgres.name)
    error_message = "Staging runs single-AZ PostgreSQL 16 on capped gp3 storage with this module's parameter group."
  }
  assert {
    condition = (aws_db_instance.this.manage_master_user_password &&
      aws_db_instance.this.master_user_secret_kms_key_id == aws_kms_key.data.arn &&
    aws_db_instance.this.password == null)
    error_message = "The master credential must be RDS-managed under the data key, never a password passed through Terraform state."
  }
  assert {
    condition = (aws_db_parameter_group.postgres.family == "postgres16" &&
      anytrue([
        for parameter in aws_db_parameter_group.postgres.parameter :
        parameter.name == "rds.force_ssl" && parameter.value == "1"
      ]) &&
      anytrue([
        for parameter in aws_db_parameter_group.postgres.parameter :
        parameter.name == "log_statement" && parameter.value == "none"
    ]))
    error_message = "The server must refuse plaintext sessions and must not log statement text."
  }

  # --- audio bucket ---------------------------------------------------------
  assert {
    condition = (aws_s3_bucket_public_access_block.audio.block_public_acls &&
      aws_s3_bucket_public_access_block.audio.block_public_policy &&
      aws_s3_bucket_public_access_block.audio.ignore_public_acls &&
      aws_s3_bucket_public_access_block.audio.restrict_public_buckets &&
      aws_s3_bucket_ownership_controls.audio.rule[0].object_ownership == "BucketOwnerEnforced" &&
    aws_s3_bucket_versioning.audio.versioning_configuration[0].status == "Enabled")
    error_message = "Audio must block public access in all four ways, disable ACLs, and keep versions."
  }
  assert {
    condition = (one([for rule in aws_s3_bucket_server_side_encryption_configuration.audio.rule : one(rule.apply_server_side_encryption_by_default).sse_algorithm]) == "aws:kms" &&
      one([for rule in aws_s3_bucket_server_side_encryption_configuration.audio.rule : one(rule.apply_server_side_encryption_by_default).kms_master_key_id]) == aws_kms_key.data.arn &&
    one([for rule in aws_s3_bucket_server_side_encryption_configuration.audio.rule : rule.bucket_key_enabled]))
    error_message = "Audio must default to SSE-KMS under the data key, with bucket keys reducing KMS request cost."
  }
  assert {
    condition = (length(jsondecode(aws_s3_bucket_policy.audio.policy).Statement) == 6 &&
      alltrue([
        for statement in jsondecode(aws_s3_bucket_policy.audio.policy).Statement : statement.Effect == "Deny"
      ]) &&
      !strcontains(aws_s3_bucket_policy.audio.policy, "SourceVpce") &&
      jsondecode(aws_s3_bucket_policy.audio.policy).Statement[0].Condition.Bool["aws:SecureTransport"] == "false" &&
    jsondecode(aws_s3_bucket_policy.audio.policy).Statement[1].Condition.NumericLessThan["s3:TlsVersion"] == "1.2")
    error_message = "Plain HTTP and TLS below 1.2 must be denied, and no VPC-only condition may block an external presigned PUT."
  }
  assert {
    # Missing SSE headers must not trigger these denies.
    # Explicit wrong algorithms or keys must still trigger them.
    condition = (
      jsondecode(aws_s3_bucket_policy.audio.policy).Statement[2].Condition.Null["s3:x-amz-server-side-encryption"] == "false" &&
      jsondecode(aws_s3_bucket_policy.audio.policy).Statement[2].Condition.StringNotEquals["s3:x-amz-server-side-encryption"] == "aws:kms" &&
      jsondecode(aws_s3_bucket_policy.audio.policy).Statement[3].Condition.Null["s3:x-amz-server-side-encryption-aws-kms-key-id"] == "false" &&
      jsondecode(aws_s3_bucket_policy.audio.policy).Statement[3].Condition.StringNotEquals["s3:x-amz-server-side-encryption-aws-kms-key-id"] == aws_kms_key.data.arn &&
      jsondecode(aws_s3_bucket_policy.audio.policy).Statement[2].Action == "s3:PutObject" &&
      jsondecode(aws_s3_bucket_policy.audio.policy).Statement[3].Action == "s3:PutObject" &&
      !strcontains(aws_s3_bucket_policy.audio.policy, "StringNotEqualsIfExists")
    )
    error_message = "Encryption denies must require a present header and an incorrect algorithm or key."
  }
  assert {
    condition = (
      one([for statement in jsondecode(aws_s3_bucket_policy.audio.policy).Statement : statement if statement.Sid == "DenyKmsWithoutKey"]).Condition.StringEquals["s3:x-amz-server-side-encryption"] == "aws:kms" &&
      one([for statement in jsondecode(aws_s3_bucket_policy.audio.policy).Statement : statement if statement.Sid == "DenyKmsWithoutKey"]).Condition.Null["s3:x-amz-server-side-encryption-aws-kms-key-id"] == "true" &&
      one([for statement in jsondecode(aws_s3_bucket_policy.audio.policy).Statement : statement if statement.Sid == "DenyCustomerProvidedEncryption"]).Condition.Null["s3:x-amz-server-side-encryption-customer-algorithm"] == "false"
    )
    error_message = "Explicit SSE-KMS must name the approved key; SSE-C must be denied. Headerless default encryption must remain eligible."
  }
  # Evaluate only the encryption denies over independent request cases. This
  # exercises the IAM condition logic; real S3/key grants remain live checks.
  assert {
    condition = alltrue([
      for request in [
        { name = "headerless", context = {}, denied = false },
        { name = "approved KMS", context = { "s3:x-amz-server-side-encryption" = "aws:kms", "s3:x-amz-server-side-encryption-aws-kms-key-id" = aws_kms_key.data.arn }, denied = false },
        { name = "SSE-S3", context = { "s3:x-amz-server-side-encryption" = "AES256" }, denied = true },
        { name = "wrong KMS key", context = { "s3:x-amz-server-side-encryption" = "aws:kms", "s3:x-amz-server-side-encryption-aws-kms-key-id" = "arn:aws:kms:us-east-2:000000000000:key/wrong-key" }, denied = true },
        { name = "KMS with no key", context = { "s3:x-amz-server-side-encryption" = "aws:kms" }, denied = true },
        { name = "SSE-C", context = { "s3:x-amz-server-side-encryption-customer-algorithm" = "AES256" }, denied = true }
        ] : anytrue([
          for statement in jsondecode(aws_s3_bucket_policy.audio.policy).Statement : alltrue([
            for operator, entries in statement.Condition : alltrue([
              for key, expected in entries :
              operator == "Null" ? tostring(!contains(keys(request.context), key)) == expected :
              operator == "StringEquals" ? lookup(request.context, key, "") == expected :
              operator == "StringNotEquals" ? lookup(request.context, key, "") != expected : false
            ])
          ]) if contains(["DenyWrongEncryptionAlgorithm", "DenyWrongEncryptionKey", "DenyKmsWithoutKey", "DenyCustomerProvidedEncryption"], statement.Sid)
      ]) == request.denied
    ])
    error_message = "Encryption guards must allow headerless/approved-key requests and deny SSE-S3, wrong keys, KMS without a key and SSE-C."
  }
  assert {
    condition = (length(aws_s3_bucket_lifecycle_configuration.audio.rule) == 3 &&
      alltrue([
        for rule in aws_s3_bucket_lifecycle_configuration.audio.rule :
        rule.status == "Enabled" && length(rule.transition) == 0 && length(rule.noncurrent_version_transition) == 0
    ]))
    error_message = "Three enabled rules and no storage-class transition: the worker cannot transcribe an archived pending object."
  }
  assert {
    condition = (one([for rule in aws_s3_bucket_lifecycle_configuration.audio.rule : rule if rule.id == "expire-current-audio"]).expiration[0].days == 7 &&
      one([for rule in aws_s3_bucket_lifecycle_configuration.audio.rule : rule if rule.id == "expire-superseded-audio-versions"]).noncurrent_version_expiration[0].noncurrent_days == 7 &&
    one([for rule in aws_s3_bucket_lifecycle_configuration.audio.rule : rule if rule.id == "abort-incomplete-uploads"]).abort_incomplete_multipart_upload[0].days_after_initiation == 7)
    error_message = "Current objects, superseded versions and abandoned multipart uploads each need their own explicit expiry."
  }

  # --- queues ---------------------------------------------------------------
  assert {
    condition = (aws_sqs_queue.jobs.name == "ernote-synthetic-test-jobs" &&
    aws_sqs_queue.dlq.name == "${aws_sqs_queue.jobs.name}-dlq")
    error_message = "One standard queue plus the `-dlq` sibling Config::stt_dlq derives from STT_QUEUE."
  }
  assert {
    condition = (aws_sqs_queue.jobs.visibility_timeout_seconds == 300 &&
      aws_sqs_queue.jobs.receive_wait_time_seconds == 20 &&
      aws_sqs_queue.jobs.message_retention_seconds == 345600 &&
      aws_sqs_queue.dlq.message_retention_seconds == 1209600 &&
    jsondecode(aws_sqs_queue.jobs.redrive_policy).maxReceiveCount == 3)
    error_message = "Queue settings must match the worker's 300-second lease, long polling, 4/14-day retention and three attempts."
  }
  assert {
    condition = (endswith(jsondecode(aws_sqs_queue.jobs.redrive_policy).deadLetterTargetArn, ":ernote-synthetic-test-jobs-dlq") &&
      jsondecode(aws_sqs_queue.dlq.redrive_allow_policy).redrivePermission == "byQueue" &&
      length(jsondecode(aws_sqs_queue.dlq.redrive_allow_policy).sourceQueueArns) == 1 &&
    endswith(jsondecode(aws_sqs_queue.dlq.redrive_allow_policy).sourceQueueArns[0], ":ernote-synthetic-test-jobs"))
    error_message = "Failed jobs go to this DLQ, and only this queue may send them there."
  }
  assert {
    condition = (alltrue([
      for queue in [aws_sqs_queue.jobs, aws_sqs_queue.dlq] :
      queue.kms_master_key_id == aws_kms_key.data.arn
      ]) &&
      alltrue([
        for policy in [aws_sqs_queue_policy.jobs, aws_sqs_queue_policy.dlq] :
        jsondecode(policy.policy).Statement[0].Effect == "Deny" &&
        jsondecode(policy.policy).Statement[0].Condition.Bool["aws:SecureTransport"] == "false"
    ]))
    error_message = "Both queues need SSE-KMS under the data key and must deny insecure transport."
  }
  assert {
    condition     = length(jsondecode(aws_sqs_queue_policy.dlq.policy).Statement) == 1
    error_message = "No operator principals were supplied, so the DLQ policy must grant nobody anything."
  }
}

run "explicit_retention_and_operator_grants" {
  command = apply
  variables {
    db_backup_retention_days                = 14
    db_multi_az                             = true
    db_ca_cert_identifier                   = "rds-ca-rsa2048-g1"
    kms_deletion_window_days                = 30
    audio_retention_days                    = 30
    audio_noncurrent_version_retention_days = 3
    queue_retention_days                    = 7
    dlq_retention_days                      = 14
    queue_max_receive_count                 = 5
    dlq_operator_principal_arns             = ["arn:aws:iam::000000000000:role/ernote-operator"]
    bucket_administration_principal_arns    = ["arn:aws:iam::000000000000:role/ernote-tfstate-staging-foundation-operator"]
  }

  assert {
    condition = (aws_db_instance.this.backup_retention_period == 14 &&
      aws_db_instance.this.multi_az &&
    aws_db_instance.this.ca_cert_identifier == "rds-ca-rsa2048-g1")
    error_message = "Longer retention, Multi-AZ and an explicit server CA must all be selectable without editing the module."
  }
  assert {
    condition = (one([for rule in aws_s3_bucket_lifecycle_configuration.audio.rule : rule if rule.id == "expire-current-audio"]).expiration[0].days == 30 &&
    one([for rule in aws_s3_bucket_lifecycle_configuration.audio.rule : rule if rule.id == "expire-superseded-audio-versions"]).noncurrent_version_expiration[0].noncurrent_days == 3)
    error_message = "Current and noncurrent expiry are separate clocks and must be settable independently."
  }
  assert {
    condition = (length(jsondecode(aws_s3_bucket_policy.audio.policy).Statement) == 7 &&
      length(one([for statement in jsondecode(aws_s3_bucket_policy.audio.policy).Statement : statement if statement.Sid == "DenyProtectionChangesOutsideAdministration"]).Condition.StringNotLike["aws:PrincipalArn"]) == 1 &&
      contains(one([for statement in jsondecode(aws_s3_bucket_policy.audio.policy).Statement : statement if statement.Sid == "DenyProtectionChangesOutsideAdministration"]).Condition.StringNotLike["aws:PrincipalArn"], "arn:aws:iam::000000000000:role/ernote-tfstate-staging-foundation-operator") &&
    contains(one([for statement in jsondecode(aws_s3_bucket_policy.audio.policy).Statement : statement if statement.Sid == "DenyProtectionChangesOutsideAdministration"]).Action, "s3:PutLifecycleConfiguration"))
    error_message = "Named administration principals must be the only ones able to change the bucket's own protections."
  }
  assert {
    condition = (length(jsondecode(aws_sqs_queue_policy.dlq.policy).Statement) == 2 &&
      jsondecode(aws_sqs_queue_policy.dlq.policy).Statement[1].Effect == "Allow" &&
      length(jsondecode(aws_sqs_queue_policy.dlq.policy).Statement[1].Principal.AWS) == 1 &&
      contains(jsondecode(aws_sqs_queue_policy.dlq.policy).Statement[1].Principal.AWS, "arn:aws:iam::000000000000:role/ernote-operator") &&
    contains(jsondecode(aws_sqs_queue_policy.dlq.policy).Statement[1].Action, "sqs:StartMessageMoveTask"))
    error_message = "Redrive back to the main queue must be granted to the named operator role only."
  }
  assert {
    condition = (jsondecode(aws_sqs_queue.jobs.redrive_policy).maxReceiveCount == 5 &&
    aws_sqs_queue.jobs.message_retention_seconds == 604800)
    error_message = "Attempt count and main-queue retention must follow their inputs."
  }
}

run "reject_disabled_backups" {
  command = plan
  variables { db_backup_retention_days = 0 }
  expect_failures = [var.db_backup_retention_days]
}
run "reject_key_window_shorter_than_backups" {
  command = plan
  variables {
    db_backup_retention_days = 21
    kms_deletion_window_days = 14
  }
  expect_failures = [var.kms_deletion_window_days]
}
run "reject_dlq_shorter_than_main_queue" {
  command = plan
  variables {
    queue_retention_days = 7
    dlq_retention_days   = 3
  }
  expect_failures = [var.dlq_retention_days]
}
run "reject_visibility_below_worker_lease" {
  command = plan
  variables { queue_visibility_timeout_seconds = 120 }
  expect_failures = [var.queue_visibility_timeout_seconds]
}
run "reject_short_polling" {
  command = plan
  variables { queue_receive_wait_seconds = 0 }
  expect_failures = [var.queue_receive_wait_seconds]
}
run "reject_storage_cap_below_allocation" {
  command = plan
  variables {
    db_allocated_storage     = 50
    db_max_allocated_storage = 20
  }
  expect_failures = [var.db_max_allocated_storage]
}
run "reject_dotted_bucket_name" {
  command = plan
  variables { audio_bucket_name = "ernote.synthetic.audio" }
  expect_failures = [var.audio_bucket_name]
}
run "reject_reserved_master_username" {
  command = plan
  variables { db_master_username = "rdsadmin" }
  expect_failures = [var.db_master_username]
}
run "reject_unsupported_engine_major" {
  command = plan
  variables { postgres_version = "15.7" }
  expect_failures = [var.postgres_version]
}
run "reject_database_without_security_group" {
  command = plan
  variables { db_security_group_ids = [] }
  expect_failures = [var.db_security_group_ids]
}
