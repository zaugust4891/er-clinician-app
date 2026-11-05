# S9 durable logging/audit plus runtime signals. No workload is activated here.
module "observability" {
  source                   = "../../../modules/observability"
  count                    = var.enabled ? 1 : 0
  name                     = "${var.project}-${local.environment}"
  aws_account_id           = var.aws_account_id
  aws_region               = var.aws_region
  audio_bucket_arn         = local.audio_bucket_arn
  queue_name               = module.data[0].stt_queue_name
  dlq_name                 = "${module.data[0].stt_queue_name}-dlq"
  db_identifier            = module.data[0].db_identifier
  db_allocated_storage_gib = var.data.db_allocated_storage
  ecs_services = var.cluster.container_insights == "disabled" ? {} : {
    "${var.project}-${local.environment}-api"    = 2
    "${var.project}-${local.environment}-worker" = 1
  }
  edge                    = local.edge_enabled ? { alb = module.edge[0].alb_arn_suffix, target_group = module.edge[0].target_group_arn_suffix } : null
  alarm_emails            = var.telemetry.alarm_emails
  alarms_enabled          = var.telemetry.alarms_enabled
  operator_principal_arns = var.telemetry.operator_principal_arns
  log_retention_days      = var.telemetry.log_retention_days
  audit_retention_days    = var.telemetry.audit_retention_days
  audio_data_events       = var.telemetry.audio_data_events
  monthly_budget_usd      = var.telemetry.monthly_budget_usd
  db_connection_threshold = var.telemetry.db_connection_threshold
  depends_on              = [terraform_data.account_guard, module.identity]
}
