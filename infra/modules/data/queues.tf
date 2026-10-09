# ---------------------------------------------------------------------------
# Job queue and dead-letter queue
# ---------------------------------------------------------------------------
# One **standard** queue carries both job kinds the application sends — the
# `TranscribeChunk` the API enqueues at upload completion and the `GenerateMdm` it
# enqueues on request (`queue.rs`). A second queue would be a measured latency decision,
# not a requirement of the code that exists.
#
# The names are load-bearing. The application is configured with `STT_QUEUE=<name>` and
# derives its dead-letter queue as `<name>-dlq` (`Config::stt_dlq`), then resolves both
# with `GetQueueUrl` rather than creating anything. Renaming either queue without
# renaming the other breaks worker startup.
#
# Message bodies are opaque identifiers by construction (§C5): audio lives in S3,
# transcripts live in Postgres, and neither passes through a message body, an attribute,
# a queue name or an alarm.

resource "aws_sqs_queue" "dlq" {
  name = local.dlq_name

  # Failed work must stay inspectable for longer than the main queue holds live work.
  message_retention_seconds = var.dlq_retention_days * 24 * 60 * 60

  # Naming a customer-managed key replaces SQS-managed encryption; the provider rejects
  # setting both, so `sqs_managed_sse_enabled` is deliberately absent rather than false.
  # Producers and consumers therefore need KMS permissions of their own, which S6 grants.
  kms_master_key_id                 = aws_kms_key.data.arn
  kms_data_key_reuse_period_seconds = 300

  # Only the main queue may deposit here. This is about the *source queue*, not about
  # who may move messages back: that is an IAM grant on `sqs:StartMessageMoveTask`,
  # optionally mirrored by the operator statement in the queue policy below.
  redrive_allow_policy = jsonencode({
    redrivePermission = "byQueue"
    sourceQueueArns   = [local.queue_arn]
  })
}

resource "aws_sqs_queue" "jobs" {
  name = local.queue_name

  # 300 seconds matches the worker's lease exactly (S1 `WorkerLimits`). The worker also
  # heartbeats visibility while it holds a message; a shorter queue-side timeout would
  # hand the same job to a second worker while the first is still transcribing.
  visibility_timeout_seconds = var.queue_visibility_timeout_seconds
  receive_wait_time_seconds  = var.queue_receive_wait_seconds
  message_retention_seconds  = var.queue_retention_days * 24 * 60 * 60

  kms_master_key_id                 = aws_kms_key.data.arn
  kms_data_key_reuse_period_seconds = 300

  redrive_policy = jsonencode({
    deadLetterTargetArn = local.dlq_arn
    maxReceiveCount     = var.queue_max_receive_count
  })

  # The ARN is composed from inputs above rather than read from the DLQ resource, but
  # the DLQ must still exist before a job can be redriven to it.
  depends_on = [aws_sqs_queue.dlq]
}

# ---------------------------------------------------------------------------
# Queue policies
# ---------------------------------------------------------------------------
# Resource policies here are a floor, like the bucket policy. The producing and
# consuming grants themselves are IAM policies written in S6, scoped to these ARNs.

locals {
  insecure_transport_statement = {
    for key, arn in { jobs = local.queue_arn, dlq = local.dlq_arn } :
    key => {
      Sid       = "DenyInsecureTransport"
      Effect    = "Deny"
      Principal = "*"
      Action    = "sqs:*"
      Resource  = arn
      Condition = { Bool = { "aws:SecureTransport" = "false" } }
    }
  }

  # Zero or one element, for the same reason as the bucket policy's optional statement.
  dlq_operator_lists = length(var.dlq_operator_principal_arns) == 0 ? [] : [sort(tolist(var.dlq_operator_principal_arns))]

  dlq_operator_statements = [
    for principals in local.dlq_operator_lists : {
      Sid       = "AllowOperatorRedrive"
      Effect    = "Allow"
      Principal = { AWS = principals }
      Action = [
        "sqs:GetQueueAttributes",
        "sqs:GetQueueUrl",
        "sqs:ReceiveMessage",
        "sqs:DeleteMessage",
        "sqs:StartMessageMoveTask",
        "sqs:ListMessageMoveTasks",
        "sqs:CancelMessageMoveTask",
      ]
      Resource  = local.dlq_arn
      Condition = { Bool = { "aws:SecureTransport" = "true" } }
    }
  ]
}

resource "aws_sqs_queue_policy" "jobs" {
  queue_url = aws_sqs_queue.jobs.url
  policy = jsonencode({
    Version   = "2012-10-17"
    Statement = [local.insecure_transport_statement["jobs"]]
  })
}

resource "aws_sqs_queue_policy" "dlq" {
  queue_url = aws_sqs_queue.dlq.url
  policy = jsonencode({
    Version   = "2012-10-17"
    Statement = concat([local.insecure_transport_statement["dlq"]], local.dlq_operator_statements)
  })
}
