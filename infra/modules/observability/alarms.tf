resource "aws_sns_topic" "alarms" {
  name              = "${var.name}-alarms"
  kms_master_key_id = aws_kms_key.telemetry.arn
}
resource "aws_sns_topic_policy" "alarms" {
  arn = aws_sns_topic.alarms.arn
  policy = jsonencode({ Version = "2012-10-17", Statement = [
    { Sid = "CloudWatchPublish", Effect = "Allow", Principal = { Service = "cloudwatch.amazonaws.com" }, Action = "sns:Publish", Resource = local.topic_arn, Condition = { StringEquals = { "aws:SourceAccount" = var.aws_account_id }, ArnLike = { "aws:SourceArn" = "arn:aws:cloudwatch:${var.aws_region}:${var.aws_account_id}:alarm:${var.name}-*" } } },
    { Sid = "BudgetPublish", Effect = "Allow", Principal = { Service = "budgets.amazonaws.com" }, Action = "sns:Publish", Resource = local.topic_arn, Condition = { StringEquals = { "aws:SourceAccount" = var.aws_account_id }, ArnLike = { "aws:SourceArn" = "arn:aws:budgets::${var.aws_account_id}:*" } } },
    { Sid = "DenyInsecureTransport", Effect = "Deny", Principal = "*", Action = "sns:Publish", Resource = local.topic_arn, Condition = { Bool = { "aws:SecureTransport" = "false" } } }
  ] })
}
resource "aws_sns_topic_subscription" "email" {
  for_each  = var.alarm_emails
  topic_arn = aws_sns_topic.alarms.arn
  protocol  = "email"
  endpoint  = each.value
}
# Two low-cardinality custom metrics only. Rich counters/timing/token fields stay
# queryable in bounded aggregate log lines, avoiding dozens of billable series.
resource "aws_cloudwatch_log_metric_filter" "worker" {
  for_each       = toset(["worker_progress", "usage_write_failed"])
  name           = each.value
  log_group_name = aws_cloudwatch_log_group.app["worker"].name
  pattern        = "{ $.metric_name = \"${each.value}\" }"
  metric_transformation {
    name      = each.value
    namespace = local.namespace
    value     = "$.value"
    unit      = "Count"
    # No default value: missing progress must never be turned into a healthy sample.
  }
}
locals {
  alarms = merge({
    worker_progress    = { namespace = local.namespace, metric = "worker_progress", dimensions = {}, statistic = "Sum", threshold = 1, comparison = "LessThanThreshold", periods = 6, points = 6, missing = "breaching" },
    usage_write_failed = { namespace = local.namespace, metric = "usage_write_failed", dimensions = {}, statistic = "Sum", threshold = 0, comparison = "GreaterThanThreshold", periods = 1, points = 1, missing = "notBreaching" },
    queue_age          = { namespace = "AWS/SQS", metric = "ApproximateAgeOfOldestMessage", dimensions = { QueueName = var.queue_name }, statistic = "Maximum", threshold = 600, comparison = "GreaterThanThreshold", periods = 3, points = 3, missing = "notBreaching" },
    dlq_visible        = { namespace = "AWS/SQS", metric = "ApproximateNumberOfMessagesVisible", dimensions = { QueueName = var.dlq_name }, statistic = "Maximum", threshold = 0, comparison = "GreaterThanThreshold", periods = 1, points = 1, missing = "notBreaching" },
    db_storage         = { namespace = "AWS/RDS", metric = "FreeStorageSpace", dimensions = { DBInstanceIdentifier = var.db_identifier }, statistic = "Minimum", threshold = 5368709120, comparison = "LessThanThreshold", periods = 3, points = 3, missing = "breaching" },
    db_connections     = { namespace = "AWS/RDS", metric = "DatabaseConnections", dimensions = { DBInstanceIdentifier = var.db_identifier }, statistic = "Maximum", threshold = var.db_connection_threshold, comparison = "GreaterThanOrEqualToThreshold", periods = 3, points = 3, missing = "breaching" }
    }, var.edge == null ? {} : {
    api_healthy = { namespace = "AWS/ApplicationELB", metric = "HealthyHostCount", dimensions = { LoadBalancer = var.edge.alb, TargetGroup = var.edge.target_group }, statistic = "Minimum", threshold = 1, comparison = "LessThanThreshold", periods = 3, points = 3, missing = "breaching" },
    api_5xx     = { namespace = "AWS/ApplicationELB", metric = "HTTPCode_Target_5XX_Count", dimensions = { LoadBalancer = var.edge.alb }, statistic = "Sum", threshold = 5, comparison = "GreaterThanOrEqualToThreshold", periods = 5, points = 3, missing = "notBreaching" },
    alb_5xx     = { namespace = "AWS/ApplicationELB", metric = "HTTPCode_ELB_5XX_Count", dimensions = { LoadBalancer = var.edge.alb }, statistic = "Sum", threshold = 5, comparison = "GreaterThanOrEqualToThreshold", periods = 5, points = 3, missing = "notBreaching" }
  })
}
resource "aws_cloudwatch_metric_alarm" "runtime" {
  for_each            = local.alarms
  alarm_name          = "${var.name}-${each.key}"
  alarm_description   = "${var.name}: ${each.key}; see docs/ops/telemetry-verification.md"
  namespace           = each.value.namespace
  metric_name         = each.value.metric
  dimensions          = each.value.dimensions
  statistic           = each.value.statistic
  threshold           = each.value.threshold
  comparison_operator = each.value.comparison
  period              = 60
  evaluation_periods  = each.value.periods
  datapoints_to_alarm = each.value.points
  treat_missing_data  = each.value.missing
  actions_enabled     = var.alarms_enabled
  alarm_actions       = [aws_sns_topic.alarms.arn]
  ok_actions          = [aws_sns_topic.alarms.arn]
  depends_on          = [aws_sns_topic_policy.alarms, aws_cloudwatch_log_metric_filter.worker, terraform_data.activation]
}
# Account-wide spend catches untagged/shared infrastructure. This intentionally
# does not replace/adopt the existing manually configured account budget.
resource "aws_budgets_budget" "monthly" {
  name         = "${var.name}-deployment-monthly"
  account_id   = var.aws_account_id
  budget_type  = "COST"
  limit_amount = tostring(var.monthly_budget_usd)
  limit_unit   = "USD"
  time_unit    = "MONTHLY"
  dynamic "notification" {
    for_each = { ACTUAL = 80, FORECASTED = 100 }
    content {
      comparison_operator       = "GREATER_THAN"
      threshold                 = notification.value
      threshold_type            = "PERCENTAGE"
      notification_type         = notification.key
      subscriber_sns_topic_arns = [aws_sns_topic.alarms.arn]
    }
  }
  depends_on = [aws_sns_topic_policy.alarms]
}
