resource "aws_cloudwatch_metric_alarm" "latency" {
  count                                 = var.edge == null ? 0 : 1
  alarm_name                            = "${var.name}-api-p95"
  namespace                             = "AWS/ApplicationELB"
  metric_name                           = "TargetResponseTime"
  dimensions                            = { LoadBalancer = var.edge.alb }
  extended_statistic                    = "p95"
  threshold                             = 0.5
  comparison_operator                   = "GreaterThanThreshold"
  period                                = 60
  evaluation_periods                    = 5
  datapoints_to_alarm                   = 3
  treat_missing_data                    = "notBreaching"
  evaluate_low_sample_count_percentiles = "ignore"
  actions_enabled                       = var.alarms_enabled
  alarm_actions                         = [aws_sns_topic.alarms.arn]
  ok_actions                            = [aws_sns_topic.alarms.arn]
}
resource "aws_cloudwatch_metric_alarm" "tasks" {
  for_each            = var.ecs_services
  alarm_name          = "${var.name}-${each.key}-running"
  namespace           = "ECS/ContainerInsights"
  metric_name         = "RunningTaskCount"
  dimensions          = { ClusterName = var.name, ServiceName = each.key }
  statistic           = "Minimum"
  threshold           = each.value
  comparison_operator = "LessThanThreshold"
  period              = 60
  evaluation_periods  = 3
  datapoints_to_alarm = 3
  treat_missing_data  = "breaching"
  actions_enabled     = var.alarms_enabled
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]
}
resource "aws_cloudwatch_log_metric_filter" "duration" {
  for_each       = { stt_seconds = "Seconds", mdm_duration_ms = "Milliseconds" }
  name           = each.key
  log_group_name = aws_cloudwatch_log_group.app["worker"].name
  pattern        = "{ $.${each.key} = * }"
  metric_transformation {
    name      = each.key
    namespace = local.namespace
    value     = "$.${each.key}"
    unit      = each.value
  }
}
resource "aws_cloudwatch_dashboard" "operations" {
  dashboard_name = "${var.name}-operations"
  dashboard_body = jsonencode({ widgets = concat(
    [for widget in [
      { type = "metric", x = 0, y = 0, width = 12, height = 6, properties = {
        title   = "Requests and 5xx / minute", region = var.aws_region, period = 60, stat = "Sum",
        metrics = [for metric in ["RequestCount", "HTTPCode_Target_5XX_Count", "HTTPCode_ELB_5XX_Count"] : ["AWS/ApplicationELB", metric, "LoadBalancer", try(var.edge.alb, "")]]
      } },
      { type = "metric", x = 12, y = 0, width = 12, height = 6, properties = {
        title   = "API latency (seconds)", region = var.aws_region, period = 60,
        metrics = [for stat in ["p50", "p95"] : ["AWS/ApplicationELB", "TargetResponseTime", "LoadBalancer", try(var.edge.alb, ""), { stat = stat }]]
      } }
      ] : widget if var.edge != null], [
      { type = "metric", x = 0, y = 6, width = 12, height = 6, properties = {
        title = "Queue depth, age and DLQ", region = var.aws_region, period = 60, stat = "Maximum",
        metrics = [
          ["AWS/SQS", "ApproximateNumberOfMessagesVisible", "QueueName", var.queue_name],
          ["AWS/SQS", "ApproximateAgeOfOldestMessage", "QueueName", var.queue_name],
          ["AWS/SQS", "ApproximateNumberOfMessagesVisible", "QueueName", var.dlq_name]
        ]
      } },
      { type = "metric", x = 12, y = 6, width = 12, height = 6, properties = {
        title   = "Database capacity", region = var.aws_region, period = 60, stat = "Average",
        metrics = [for metric in ["CPUUtilization", "DatabaseConnections", "FreeStorageSpace"] : ["AWS/RDS", metric, "DBInstanceIdentifier", var.db_identifier]]
      } },
      { type = "metric", x = 0, y = 12, width = 12, height = 6, properties = {
        title   = "STT audio seconds / MDM job milliseconds", region = var.aws_region, period = 60, stat = "Average",
        metrics = [[local.namespace, "stt_seconds"], [local.namespace, "mdm_duration_ms"]]
      } },
      { type = "metric", x = 12, y = 12, width = 12, height = 6, properties = {
        title   = "Worker progress and metering failures", region = var.aws_region, period = 60, stat = "Sum",
        metrics = [[local.namespace, "worker_progress"], [local.namespace, "usage_write_failed"]]
      } }
      ], [for index, service in sort(keys(var.ecs_services)) : {
        type = "metric", x = index * 12, y = 18, width = 12, height = 6, properties = {
          title   = "${service} running tasks", region = var.aws_region, period = 60, stat = "Minimum",
          metrics = [["ECS/ContainerInsights", "RunningTaskCount", "ClusterName", var.name, "ServiceName", service]]
        }
    }]
  ) })
}
resource "aws_cloudwatch_log_metric_filter" "outbox_failure" {
  name           = "outbox_dispatch_failed"
  log_group_name = aws_cloudwatch_log_group.app["worker"].name
  pattern        = "{ $.error_kind = \"outbox_dispatch_failed\" }"
  metric_transformation {
    name      = "outbox_dispatch_failed"
    namespace = local.namespace
    value     = "1"
    unit      = "Count"
  }
}
resource "aws_cloudwatch_metric_alarm" "outbox_failure" {
  alarm_name          = "${var.name}-outbox-dispatch-failed"
  namespace           = local.namespace
  metric_name         = "outbox_dispatch_failed"
  statistic           = "Sum"
  threshold           = 0
  comparison_operator = "GreaterThanThreshold"
  period              = 60
  evaluation_periods  = 3
  datapoints_to_alarm = 3
  treat_missing_data  = "notBreaching"
  actions_enabled     = var.alarms_enabled
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]
}
