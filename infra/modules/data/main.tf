locals {
  # The calling roots restrict aws_region to US commercial regions, which is the same
  # assumption the network module's S3 gateway policy already makes.
  partition = "aws"

  db_identifier   = "${var.name}-pg"
  postgres_family = "postgres${split(".", var.postgres_version)[0]}"

  # The main queue's redrive policy names the DLQ and the DLQ's redrive_allow_policy
  # names the main queue. Composing both ARNs from inputs breaks what would otherwise
  # be a resource dependency cycle, and keeps the names readable in review.
  queue_name = "${var.name}-jobs"
  dlq_name   = "${var.name}-jobs-dlq"
  queue_arn  = "arn:${local.partition}:sqs:${var.aws_region}:${var.aws_account_id}:${var.name}-jobs"
  dlq_arn    = "arn:${local.partition}:sqs:${var.aws_region}:${var.aws_account_id}:${var.name}-jobs-dlq"

  audio_bucket_arn = "arn:${local.partition}:s3:::${var.audio_bucket_name}"
}

# ---------------------------------------------------------------------------
# Application-data key
# ---------------------------------------------------------------------------
# One customer-managed key for RDS storage and snapshots, audio objects, and queue
# payloads. It is deliberately **not** the bootstrap state/artifact key: state and
# security evidence live on a different key boundary from clinical application data,
# so a grant that lets a task decrypt audio can never also decrypt Terraform state.
#
# Per-tenant keys are not used. They multiply grants, key policies and rotation work
# for every organization, and tenancy in this system is enforced by authenticated
# repository scoping and server-derived object keys, not by cryptographic separation.
# Revisit only when a contract or isolation requirement actually asks for it.
#
# Disabling, scheduling deletion of, or losing access to this key makes every RDS
# snapshot, audio object and queued message encrypted under it unreadable — including
# backups taken while it was healthy. That is why the deletion window must cover the
# backup retention period and why no ordinary runtime role may touch the key's policy.

resource "aws_kms_key" "data" {
  description             = "${var.name} application data: RDS storage and backups, audio objects, job queue"
  deletion_window_in_days = var.kms_deletion_window_days
  enable_key_rotation     = true

  # "Enable IAM policies": the account may delegate use of this key through IAM. It
  # grants nothing by itself. Every service grant is an IAM policy, written in S6.
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      {
        Sid       = "EnableIAMPolicies"
        Effect    = "Allow"
        Principal = { AWS = "arn:${local.partition}:iam::${var.aws_account_id}:root" }
        Action    = "kms:*"
        Resource  = "*"
      }
    ]
  })
}

resource "aws_kms_alias" "data" {
  name          = "alias/${var.name}-data"
  target_key_id = aws_kms_key.data.key_id
}
