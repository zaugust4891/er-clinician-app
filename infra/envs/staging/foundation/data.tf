# S5 durable resources: the encrypted database, the audio bucket, and the job queue
# pair the application already expects. Placement comes from S4; IAM grants, database
# logins and secret values come from S6.

module "data" {
  source = "../../../modules/data"
  count  = 1 # Match the indexed interface of the gated production root.

  name              = "${var.project}-${local.environment}"
  aws_region        = var.aws_region
  aws_account_id    = var.aws_account_id
  audio_bucket_name = local.audio_bucket_name

  # Reuse S4's isolated two-AZ subnet group and database security group rather than
  # creating a second placement that could drift from the reviewed network.
  db_subnet_group_name  = module.network[0].db_subnet_group_name
  db_security_group_ids = [module.network[0].security_group_ids.database]

  postgres_version         = var.data.postgres_version
  db_instance_class        = var.data.db_instance_class
  db_allocated_storage     = var.data.db_allocated_storage
  db_max_allocated_storage = var.data.db_max_allocated_storage
  db_name                  = var.data.db_name
  db_master_username       = var.data.db_master_username
  db_ca_cert_identifier    = var.data.db_ca_cert_identifier
  db_backup_window         = var.data.db_backup_window
  db_maintenance_window    = var.data.db_maintenance_window
  db_deletion_protection   = var.data.db_deletion_protection
  db_final_snapshot_suffix = var.data.db_final_snapshot_suffix
  db_apply_immediately     = var.data.db_apply_immediately

  # S0's synthetic-staging defaults, applied here rather than in the shared module so
  # that production is forced to state its own recovery decisions explicitly.
  db_multi_az              = coalesce(var.data.db_multi_az, false)
  db_backup_retention_days = coalesce(var.data.db_backup_retention_days, 7)

  audio_retention_days                    = var.data.audio_retention_days
  audio_noncurrent_version_retention_days = var.data.audio_noncurrent_version_retention_days
  audio_abort_incomplete_multipart_days   = var.data.audio_abort_incomplete_multipart_days
  bucket_administration_principal_arns    = var.data.bucket_administration_principal_arns

  queue_visibility_timeout_seconds = var.data.queue_visibility_timeout_seconds
  queue_receive_wait_seconds       = var.data.queue_receive_wait_seconds
  queue_max_receive_count          = var.data.queue_max_receive_count
  queue_retention_days             = var.data.queue_retention_days
  dlq_retention_days               = var.data.dlq_retention_days
  dlq_operator_principal_arns      = var.data.dlq_operator_principal_arns

  kms_deletion_window_days           = var.data.kms_deletion_window_days
  kms_service_policy_statements_json = jsonencode(module.identity[0].kms_key_policy_statements)

  depends_on = [terraform_data.account_guard]
}
