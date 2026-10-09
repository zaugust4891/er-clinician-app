output "kms_key_arn" {
  description = "Application-data key. RDS storage and snapshots, audio objects and queue payloads are encrypted under it; S6 grants use of it."
  value       = aws_kms_key.data.arn
}

output "kms_key_alias" {
  description = "Human-readable alias for the same key."
  value       = aws_kms_alias.data.name
}

output "audio_bucket_name" {
  description = "Value of the application's AUDIO_BUCKET."
  value       = aws_s3_bucket.audio.bucket
}

output "audio_bucket_arn" {
  description = "Bucket ARN. The calling root passes the identical string to the network module's S3 gateway endpoint policy."
  value       = aws_s3_bucket.audio.arn
}

output "stt_queue_name" {
  description = "Value of the application's STT_QUEUE. The worker and API resolve it, and its `-dlq` sibling, with GetQueueUrl."
  value       = aws_sqs_queue.jobs.name
}

output "stt_queue_url" {
  description = "Main job queue URL, for operators and for S9's depth and age alarms."
  value       = aws_sqs_queue.jobs.url
}

output "stt_queue_arn" {
  description = "Main job queue ARN, for S6's producer and consumer policies."
  value       = aws_sqs_queue.jobs.arn
}

output "stt_dlq_name" {
  description = "Dead-letter queue name. It must remain the main queue's name plus `-dlq`; Config::stt_dlq derives it that way."
  value       = aws_sqs_queue.dlq.name
}

output "stt_dlq_url" {
  description = "Dead-letter queue URL, for operator inspection and S9's failure alarm."
  value       = aws_sqs_queue.dlq.url
}

output "stt_dlq_arn" {
  description = "Dead-letter queue ARN, for S6's lookup grant and the operator redrive grant."
  value       = aws_sqs_queue.dlq.arn
}

output "db_host" {
  description = "Private database hostname, without a port. Clients verify this name against the server certificate."
  value       = aws_db_instance.this.address
}

output "db_port" {
  description = "Database port."
  value       = aws_db_instance.this.port
}

output "db_name" {
  description = "Initial database name."
  value       = aws_db_instance.this.db_name
}

output "db_identifier" {
  description = "RDS instance identifier, for describe/backup inspection."
  value       = aws_db_instance.this.identifier
}

output "db_arn" {
  description = "RDS instance ARN, for S6 policies and S9 alarms."
  value       = aws_db_instance.this.arn
}

output "db_parameter_group_name" {
  description = "Parameter group carrying rds.force_ssl = 1; inspect it when diagnosing a TLS failure."
  value       = aws_db_parameter_group.postgres.name
}

output "db_backup_retention_days" {
  description = "Automated-backup retention actually applied, so an operator can compare it with the contract."
  value       = aws_db_instance.this.backup_retention_period
}

output "db_master_user_secret_arn" {
  description = <<-EOT
    Secrets Manager ARN of the RDS-managed master credential. A pointer, not a value, and
    deliberately **not** published in the foundation manifest: S6's one-shot bootstrap task
    uses it to create the runtime logins, and no running service is ever granted it.
  EOT
  value       = try(aws_db_instance.this.master_user_secret[0].secret_arn, null)
}
