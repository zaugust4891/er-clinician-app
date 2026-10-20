locals {
  components   = toset(["api", "worker", "migration", "db-bootstrap", "onboarding"])
  log_names    = { for p in local.components : p => "/ecs/${var.name}/${p}" }
  log_arns     = [for name in values(local.log_names) : "arn:aws:logs:${var.aws_region}:${var.aws_account_id}:log-group:${name}"]
  namespace    = "ERNote/${var.name}"
  trail_arn    = "arn:aws:cloudtrail:${var.aws_region}:${var.aws_account_id}:trail/${var.name}"
  topic_arn    = "arn:aws:sns:${var.aws_region}:${var.aws_account_id}:${var.name}-alarms"
  audit_bucket = "${var.name}-audit-${var.aws_account_id}-${var.aws_region}"
  operator_arn = "arn:aws:iam::${var.aws_account_id}:role/${var.name}-telemetry-operator"
}
resource "terraform_data" "activation" {
  lifecycle {
    precondition {
      condition     = !var.alarms_enabled || (length(var.alarm_emails) > 0 && length(var.operator_principal_arns) > 0)
      error_message = "Runtime notifications require reviewed email destinations and operator access. Confirm live delivery before traffic."
    }
  }
}
resource "aws_kms_key" "telemetry" {
  description             = "${var.name}: application logs, restricted audit and alarm delivery"
  enable_key_rotation     = true
  deletion_window_in_days = 30
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      { Sid = "AccountAdministration", Effect = "Allow", Principal = { AWS = "arn:aws:iam::${var.aws_account_id}:root" }, Action = "kms:*", Resource = "*" },
      {
        Sid       = "EncryptOwnLogs", Effect = "Allow", Principal = { Service = "logs.${var.aws_region}.amazonaws.com" },
        Action    = ["kms:Encrypt", "kms:Decrypt", "kms:ReEncrypt*", "kms:GenerateDataKey*", "kms:DescribeKey"], Resource = "*",
        Condition = { ArnEquals = { "kms:EncryptionContext:aws:logs:arn" = local.log_arns } }
      },
      {
        Sid       = "TrailEncryption", Effect = "Allow", Principal = { Service = "cloudtrail.amazonaws.com" },
        Action    = ["kms:GenerateDataKey*"], Resource = "*",
        Condition = { StringEquals = { "aws:SourceArn" = local.trail_arn }, StringLike = { "kms:EncryptionContext:aws:cloudtrail:arn" = "arn:aws:cloudtrail:*:${var.aws_account_id}:trail/${var.name}" } }
      },
      {
        Sid    = "TrailKeyDescription", Effect = "Allow", Principal = { Service = "cloudtrail.amazonaws.com" },
        Action = "kms:DescribeKey", Resource = "*", Condition = { StringEquals = { "aws:SourceArn" = local.trail_arn } }
      },
      {
        Sid       = "AlarmEncryption", Effect = "Allow", Principal = { Service = "cloudwatch.amazonaws.com" },
        Action    = ["kms:GenerateDataKey*", "kms:Decrypt"], Resource = "*",
        Condition = { StringEquals = { "aws:SourceAccount" = var.aws_account_id }, ArnLike = { "aws:SourceArn" = "arn:aws:cloudwatch:${var.aws_region}:${var.aws_account_id}:alarm:${var.name}-*" } }
      },
      {
        Sid       = "BudgetEncryption", Effect = "Allow", Principal = { Service = "budgets.amazonaws.com" },
        Action    = ["kms:GenerateDataKey*", "kms:Decrypt"], Resource = "*",
        Condition = { StringEquals = { "aws:SourceAccount" = var.aws_account_id }, ArnLike = { "aws:SourceArn" = "arn:aws:budgets::${var.aws_account_id}:*" } }
      }
    ]
  })
  lifecycle { prevent_destroy = true }
}
resource "aws_cloudwatch_log_group" "app" {
  for_each          = local.log_names
  name              = each.value
  retention_in_days = var.log_retention_days
  kms_key_id        = aws_kms_key.telemetry.arn
  # Execution policies already allow only their own streams. Groups must exist
  # before tasks start; tasks cannot create groups or change retention/encryption.
}
resource "aws_iam_role" "operator" {
  count = length(var.operator_principal_arns) > 0 ? 1 : 0
  name  = "${var.name}-telemetry-operator"
  assume_role_policy = jsonencode({ Version = "2012-10-17", Statement = [{
    Effect = "Allow", Principal = { AWS = sort(tolist(var.operator_principal_arns)) }, Action = "sts:AssumeRole"
  }] })
}
resource "aws_iam_role_policy" "operator" {
  count = length(var.operator_principal_arns) > 0 ? 1 : 0
  role  = aws_iam_role.operator[0].id
  name  = "restricted-telemetry-read"
  policy = jsonencode({ Version = "2012-10-17", Statement = [
    { Effect = "Allow", Action = ["logs:DescribeLogStreams", "logs:GetLogEvents", "logs:FilterLogEvents", "logs:StartQuery", "logs:StartLiveTail"], Resource = concat(local.log_arns, [for arn in local.log_arns : "${arn}:*"]) },
    { Effect = "Allow", Action = ["kms:Decrypt"], Resource = aws_kms_key.telemetry.arn, Condition = { StringEquals = { "kms:ViaService" = "logs.${var.aws_region}.amazonaws.com" }, ArnEquals = { "kms:EncryptionContext:aws:logs:arn" = local.log_arns } } },
    # These result/stop APIs cannot be scoped to log groups. Query creation is scoped above.
    { Effect = "Allow", Action = ["logs:GetQueryResults", "logs:StopQuery", "logs:StopLiveTail"], Resource = "*" },
    { Effect = "Allow", Action = ["s3:ListBucket"], Resource = "arn:aws:s3:::${local.audit_bucket}" },
    { Effect = "Allow", Action = ["s3:GetObject", "s3:GetObjectVersion"], Resource = "arn:aws:s3:::${local.audit_bucket}/*" },
    { Effect = "Allow", Action = ["kms:Decrypt"], Resource = aws_kms_key.telemetry.arn, Condition = { StringLike = { "kms:EncryptionContext:aws:cloudtrail:arn" = "arn:aws:cloudtrail:*:${var.aws_account_id}:trail/${var.name}" } } },
    { Effect = "Deny", Action = ["logs:CreateExportTask", "logs:PutSubscriptionFilter", "logs:PutDestination"], Resource = "*" }
  ] })
}
# KMS caller permissions are separate from the regional Logs service grant.
# No direct log read/decrypt or key administration is granted to execution roles.
resource "aws_iam_role_policy" "log_encryption" {
  for_each = local.log_names
  role     = "${var.name}-${each.key}-execution"
  name     = "encrypted-log-delivery"
  policy = jsonencode({ Version = "2012-10-17", Statement = [{
    Effect    = "Allow", Action = ["kms:Encrypt", "kms:Decrypt", "kms:ReEncrypt*", "kms:GenerateDataKey*", "kms:DescribeKey"], Resource = aws_kms_key.telemetry.arn,
    Condition = { StringEquals = { "kms:ViaService" = "logs.${var.aws_region}.amazonaws.com", "kms:EncryptionContext:aws:logs:arn" = "arn:aws:logs:${var.aws_region}:${var.aws_account_id}:log-group:${each.value}" } }
  }] })
}
