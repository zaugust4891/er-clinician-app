output "log_group_names" { value = { for p, group in aws_cloudwatch_log_group.app : p => group.name } }
output "alarm_topic_arn" { value = aws_sns_topic.alarms.arn }
output "alarm_names" { value = { for k, alarm in aws_cloudwatch_metric_alarm.runtime : k => alarm.alarm_name } }
output "audit_bucket" { value = aws_s3_bucket.audit.id }
output "trail_arn" { value = aws_cloudtrail.management.arn }
output "operator_role_arn" { value = one(aws_iam_role.operator[*].arn) }
output "kms_key_arn" { value = aws_kms_key.telemetry.arn }
