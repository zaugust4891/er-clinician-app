mock_provider "aws" {
  mock_resource "aws_kms_key" { defaults = { arn = "arn:aws:kms:us-east-2:000000000000:key/00000000-0000-0000-0000-000000000000" } }
  mock_resource "aws_sns_topic" { defaults = { arn = "arn:aws:sns:us-east-2:000000000000:ernote-staging-alarms" } }
  mock_resource "aws_s3_bucket" { defaults = { arn = "arn:aws:s3:::ernote-staging-audit-000000000000-us-east-2" } }
}
variables {
  name             = "ernote-staging"
  aws_account_id   = "000000000000"
  aws_region       = "us-east-2"
  audio_bucket_arn = "arn:aws:s3:::synthetic-audio"
  queue_name       = "ernote-staging-jobs"
  dlq_name         = "ernote-staging-jobs-dlq"
  db_identifier    = "ernote-staging"
}
run "encrypted_retained_groups_and_scoped_delivery" {
  command = apply
  assert {
    condition     = length(aws_cloudwatch_log_group.app) == 5 && alltrue([for p, g in aws_cloudwatch_log_group.app : g.name == "/ecs/ernote-staging/${p}" && g.retention_in_days == 14 && g.kms_key_id == aws_kms_key.telemetry.arn])
    error_message = "Every ECS entrypoint needs its own retained encrypted group."
  }
  assert {
    condition = alltrue([for p, policy in aws_iam_role_policy.log_encryption : (
      jsondecode(policy.policy).Statement[0].Condition.StringEquals["kms:ViaService"] == "logs.us-east-2.amazonaws.com" &&
      jsondecode(policy.policy).Statement[0].Condition.StringEquals["kms:EncryptionContext:aws:logs:arn"] == "arn:aws:logs:us-east-2:000000000000:log-group:/ecs/ernote-staging/${p}"
    )])
    error_message = "Execution KMS grants must match only the process log group via Logs."
  }
  assert {
    condition     = length(aws_iam_role.operator) == 0 && alltrue([for a in aws_cloudwatch_metric_alarm.runtime : !a.actions_enabled])
    error_message = "Access and runtime notifications stay closed until explicit configuration."
  }
}
run "audit_coverage_and_protection" {
  command = apply
  assert {
    condition     = aws_cloudtrail.management.enable_logging && aws_cloudtrail.management.is_multi_region_trail && aws_cloudtrail.management.include_global_service_events && aws_cloudtrail.management.enable_log_file_validation && !aws_cloudtrail.management.is_organization_trail && aws_cloudtrail.management.event_selector[0].include_management_events && length(aws_cloudtrail.management.event_selector[0].data_resource) == 0
    error_message = "Validated management coverage is mandatory; object events are a separate explicit decision."
  }
  assert {
    condition     = aws_s3_bucket_versioning.audit.versioning_configuration[0].status == "Enabled" && aws_s3_bucket_public_access_block.audit.block_public_policy && !aws_s3_bucket.audit.force_destroy && aws_s3_bucket_ownership_controls.audit.rule[0].object_ownership == "BucketOwnerEnforced"
    error_message = "Audit storage must be versioned, private, owner enforced and not force-deletable."
  }
  assert {
    condition     = alltrue([for sid in ["RestrictAuditReads", "DenyManualDeletion", "DenyInsecureTransport", "DenyWrongEncryptionAlgorithm", "DenyWrongEncryptionKey"] : contains([for statement in jsondecode(aws_s3_bucket_policy.audit.policy).Statement : statement.Sid], sid)])
    error_message = "Audit policy must restrict reads, deletion, transport and encryption overrides."
  }
  assert {
    condition     = [for s in jsondecode(aws_kms_key.telemetry.policy).Statement : s.Condition.StringEquals["aws:SourceArn"] if s.Sid == "TrailEncryption"][0] == local.trail_arn
    error_message = "CloudTrail encryption must be limited to this trail."
  }
}
run "audio_events_are_bucket_scoped" {
  command = apply
  variables { audio_data_events = true }
  assert {
    condition     = aws_cloudtrail.management.event_selector[0].data_resource[0].values == tolist(["arn:aws:s3:::synthetic-audio/"])
    error_message = "Never enable account-wide object events or collect the audit bucket recursively."
  }
}
run "alarm_windows_and_delivery" {
  command = apply
  variables {
    edge                    = { alb = "app/ernote-staging-api/1234567890abcdef", target_group = "targetgroup/ernote-staging-api/1234567890abcdef" }
    alarms_enabled          = true
    alarm_emails            = ["ops@example.test"]
    operator_principal_arns = ["arn:aws:iam::000000000000:role/Operator"]
  }
  assert {
    condition     = length(aws_cloudwatch_metric_alarm.runtime) == 9 && alltrue([for a in aws_cloudwatch_metric_alarm.runtime : a.actions_enabled && a.period == 60 && a.alarm_actions == toset([aws_sns_topic.alarms.arn])])
    error_message = "Nine runtime alarms must reach the configured encrypted SNS topic."
  }
  assert {
    condition     = aws_cloudwatch_metric_alarm.runtime["worker_progress"].treat_missing_data == "breaching" && aws_cloudwatch_metric_alarm.runtime["worker_progress"].evaluation_periods == 6 && aws_cloudwatch_metric_alarm.runtime["api_healthy"].treat_missing_data == "breaching" && aws_cloudwatch_metric_alarm.runtime["queue_age"].treat_missing_data == "notBreaching" && aws_cloudwatch_metric_alarm.runtime["dlq_visible"].threshold == 0 && aws_cloudwatch_metric_alarm.runtime["api_5xx"].datapoints_to_alarm == 3
    error_message = "Missing worker/API signals must alarm; idle queues must not."
  }
  assert {
    condition     = aws_cloudwatch_log_metric_filter.worker["worker_progress"].metric_transformation[0].default_value == null && aws_cloudwatch_log_metric_filter.worker["worker_progress"].metric_transformation[0].dimensions == null && length(aws_sns_topic_subscription.email) == 1
    error_message = "Progress cannot be synthesized or partitioned by clinical identifiers."
  }
  assert {
    condition     = length(aws_budgets_budget.monthly.notification) == 2 && aws_sns_topic.alarms.kms_master_key_id == aws_kms_key.telemetry.arn && length(aws_iam_role.operator) == 1
    error_message = "Actual/forecast spend and scoped operator access must accompany runtime notifications."
  }
}
run "reject_activation_without_destination" {
  command = plan
  variables { alarms_enabled = true }
  expect_failures = [terraform_data.activation]
}
run "reject_unbounded_log_retention" {
  command = plan
  variables { log_retention_days = 0 }
  expect_failures = [var.log_retention_days]
}
run "reject_account_wide_operator_trust" {
  command = plan
  variables { operator_principal_arns = ["arn:aws:iam::000000000000:root"] }
  expect_failures = [var.operator_principal_arns]
}
