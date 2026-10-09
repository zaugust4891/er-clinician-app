variable "name" {
  description = "Environment-qualified resource prefix, e.g. ernote-staging."
  type        = string

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{2,24}$", var.name))
    error_message = "name must be lowercase letters, digits and hyphens, starting with a letter."
  }
}

variable "aws_region" {
  description = "Region every resource here is created in. Must match the calling root's provider."
  type        = string
}

variable "aws_account_id" {
  description = <<-EOT
    Account that owns these resources. Queue ARNs are composed from it rather than read
    from the queue resources, because the main queue names its DLQ and the DLQ names its
    one permitted source queue: reading both attributes would be a dependency cycle.
  EOT
  type        = string

  validation {
    condition     = can(regex("^[0-9]{12}$", var.aws_account_id))
    error_message = "aws_account_id must be exactly 12 digits, with no dashes."
  }
}

variable "audio_bucket_name" {
  description = <<-EOT
    Globally unique audio bucket name. The calling root composes it so the identical
    string can also scope S4's S3 gateway endpoint policy without making the network and
    data modules depend on each other in both directions.
  EOT
  type        = string

  validation {
    condition     = can(regex("^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$", var.audio_bucket_name))
    error_message = "Use 3-63 lowercase letters, digits and hyphens. Dots break TLS hostname verification on virtual-hosted presigned URLs."
  }
}

# ---------------------------------------------------------------------------
# Placement supplied by S4
# ---------------------------------------------------------------------------

variable "db_subnet_group_name" {
  description = "S4's two-AZ isolated database subnet group. Two-AZ placement is not Multi-AZ; see db_multi_az."
  type        = string
}

variable "db_security_group_ids" {
  description = "S4's database security group. PostgreSQL 5432 is reachable only from the four approved task security groups."
  type        = list(string)

  validation {
    condition     = length(var.db_security_group_ids) > 0
    error_message = "The database needs at least one security group; it must never be left with the VPC default."
  }
}

# ---------------------------------------------------------------------------
# Encryption
# ---------------------------------------------------------------------------

variable "kms_deletion_window_days" {
  description = <<-EOT
    Waiting period before a scheduled deletion of the application-data key completes.
    The key must outlive every backup and object version encrypted under it: deleting or
    disabling it makes RDS snapshots, audio objects and queued messages unrecoverable.
  EOT
  type        = number
  default     = 30

  validation {
    condition     = var.kms_deletion_window_days >= 7 && var.kms_deletion_window_days <= 30 && floor(var.kms_deletion_window_days) == var.kms_deletion_window_days
    error_message = "kms_deletion_window_days must be a whole number between 7 and 30."
  }

  # AWS caps the window at 30 days, so a retention period longer than that cannot be
  # fully covered: scheduling deletion then makes the oldest backups unreadable before
  # they expire. The rule asks for the longest window AWS allows in that case, and for a
  # window at least as long as the retention period whenever that is achievable.
  validation {
    condition     = var.kms_deletion_window_days >= min(30, var.db_backup_retention_days)
    error_message = "The key's deletion window must cover the automated-backup retention it protects, up to the 30-day maximum AWS allows."
  }
}

# ---------------------------------------------------------------------------
# Database
# ---------------------------------------------------------------------------

variable "postgres_version" {
  description = <<-EOT
    Engine version. A bare major version lets RDS pick a currently supported minor at
    creation; pin an exact verified minor once `describe-db-engine-versions` has been run
    for the chosen region and instance class.
  EOT
  type        = string
  default     = "16"

  validation {
    condition     = can(regex("^16(\\.[0-9]+)?$", var.postgres_version))
    error_message = "This module provisions PostgreSQL 16. Supply 16 or 16.<minor>."
  }
}

variable "db_instance_class" {
  description = "Verify the class/storage combination is offered in the region before applying."
  type        = string
  default     = "db.t4g.small"

  validation {
    condition     = can(regex("^db\\.[a-z0-9]+\\.[a-z0-9]+$", var.db_instance_class))
    error_message = "db_instance_class must look like db.<family>.<size>."
  }
}

variable "db_allocated_storage" {
  description = "Initial gp3 storage in GiB."
  type        = number
  default     = 50

  validation {
    condition     = var.db_allocated_storage >= 20 && var.db_allocated_storage <= 400
    error_message = "Use 20-400 GiB: gp3 below 400 GiB takes baseline IOPS and throughput, which this module deliberately does not override."
  }
}

variable "db_max_allocated_storage" {
  description = "Hard cap on storage autoscaling. Equal to db_allocated_storage disables autoscaling entirely."
  type        = number
  default     = 100

  validation {
    condition     = var.db_max_allocated_storage >= var.db_allocated_storage
    error_message = "db_max_allocated_storage may not be smaller than db_allocated_storage."
  }
}

variable "db_multi_az" {
  description = "Synthetic staging runs single-AZ. Production availability is a separate recovery-objective decision."
  type        = bool
  default     = false
}

variable "db_backup_retention_days" {
  description = <<-EOT
    Automated-backup retention. S0 adopts 7 days for synthetic staging; production
    retention follows production recovery objectives. Zero would disable backups and is
    rejected here, because an unavailable backup is already a Step 8 failure.
  EOT
  type        = number
  default     = 7

  validation {
    condition     = var.db_backup_retention_days >= 1 && var.db_backup_retention_days <= 35 && floor(var.db_backup_retention_days) == var.db_backup_retention_days
    error_message = "Automated backups may not be disabled; choose a whole number of days between 1 and 35."
  }
}

variable "db_backup_window" {
  description = "UTC automated-backup window, hh:mm-hh:mm, at least 30 minutes and not overlapping db_maintenance_window."
  type        = string
  default     = "07:00-07:30"

  validation {
    condition     = can(regex("^([01][0-9]|2[0-3]):[0-5][0-9]-([01][0-9]|2[0-3]):[0-5][0-9]$", var.db_backup_window))
    error_message = "db_backup_window must look like 07:00-07:30 in UTC."
  }
}

variable "db_maintenance_window" {
  description = "UTC weekly maintenance window, ddd:hh:mm-ddd:hh:mm. Minor-version upgrades land here."
  type        = string
  default     = "sun:08:00-sun:09:00"

  validation {
    condition     = can(regex("^(mon|tue|wed|thu|fri|sat|sun):([01][0-9]|2[0-3]):[0-5][0-9]-(mon|tue|wed|thu|fri|sat|sun):([01][0-9]|2[0-3]):[0-5][0-9]$", var.db_maintenance_window))
    error_message = "db_maintenance_window must look like sun:08:00-sun:09:00 in UTC."
  }
}

variable "db_name" {
  description = "Initial database name. Published in the manifest; the login and password are not."
  type        = string
  default     = "ernote"

  validation {
    condition     = can(regex("^[a-z][a-z0-9_]{0,62}$", var.db_name))
    error_message = "db_name must be lowercase letters, digits and underscores, starting with a letter."
  }
}

variable "db_master_username" {
  description = <<-EOT
    Master login, created with an RDS-managed Secrets Manager password. It bootstraps the
    schema-owner and runtime logins in S6 and must never be injected into a running task.
  EOT
  type        = string
  default     = "ernote_admin"

  validation {
    condition     = can(regex("^[a-z][a-z0-9_]{2,62}$", var.db_master_username)) && var.db_master_username != "rdsadmin"
    error_message = "db_master_username must be lowercase letters, digits and underscores, and cannot be the reserved rdsadmin."
  }
}

variable "db_ca_cert_identifier" {
  description = <<-EOT
    Optional explicit server certificate authority, e.g. rds-ca-rsa2048-g1. Null keeps the
    regional default. Clients verify the server against the CA bundle baked into the image,
    so rotating either side is an operations procedure, not a silent change.
  EOT
  type        = string
  default     = null

  validation {
    condition     = var.db_ca_cert_identifier == null || can(regex("^rds-ca-[a-z0-9-]+$", var.db_ca_cert_identifier))
    error_message = "db_ca_cert_identifier must be an rds-ca-* identifier, or null for the regional default."
  }
}

variable "db_deletion_protection" {
  description = "Enabled for persistent staging as well as production. Deliberate teardown is a separate reviewed apply."
  type        = bool
  default     = true
}

variable "db_final_snapshot_suffix" {
  description = "Distinguishes the required final snapshot from any earlier teardown's snapshot, whose name AWS still holds."
  type        = string
  default     = "v1"

  validation {
    condition     = can(regex("^[a-z0-9][a-z0-9-]{0,14}$", var.db_final_snapshot_suffix))
    error_message = "db_final_snapshot_suffix must be 1-15 lowercase letters, digits or hyphens."
  }
}

variable "db_apply_immediately" {
  description = "False applies modifications in the maintenance window. True is an operator decision with a restart risk."
  type        = bool
  default     = false
}

# ---------------------------------------------------------------------------
# Audio object storage
# ---------------------------------------------------------------------------

variable "audio_retention_days" {
  description = <<-EOT
    Expiry for current audio objects. S0 adopts 7 days for synthetic staging. Expiry is
    asynchronous: S3 removes objects after this age, it does not promise deletion at an
    exact deadline, and it does not erase the transcript, event or backup copies.
  EOT
  type        = number
  default     = 7

  validation {
    condition     = var.audio_retention_days >= 1 && floor(var.audio_retention_days) == var.audio_retention_days
    error_message = "audio_retention_days must be a whole number of days, at least 1."
  }
}

variable "audio_noncurrent_version_retention_days" {
  description = <<-EOT
    Expiry for superseded versions, counted separately from current objects. Versioning is
    what makes an overwritten upload recoverable, and every retained version is another
    copy a future deletion workflow has to account for.
  EOT
  type        = number
  default     = 7

  validation {
    condition     = var.audio_noncurrent_version_retention_days >= 1 && floor(var.audio_noncurrent_version_retention_days) == var.audio_noncurrent_version_retention_days
    error_message = "audio_noncurrent_version_retention_days must be a whole number of days, at least 1."
  }
}

variable "audio_abort_incomplete_multipart_days" {
  description = "Abandoned multipart uploads are billable storage that no listing shows; clean them up on this schedule."
  type        = number
  default     = 7

  validation {
    condition     = var.audio_abort_incomplete_multipart_days >= 1 && floor(var.audio_abort_incomplete_multipart_days) == var.audio_abort_incomplete_multipart_days
    error_message = "audio_abort_incomplete_multipart_days must be a whole number of days, at least 1."
  }
}

variable "bucket_administration_principal_arns" {
  description = <<-EOT
    Principals allowed to change the bucket's own protections — versioning, lifecycle,
    encryption, policy, ownership and public-access settings. Empty adds no such statement
    and leaves S6's scoped roles as the only control. When non-empty, include the Terraform
    role that applies this module: a bucket policy binds every caller, including this one.
  EOT
  type        = set(string)
  default     = []

  validation {
    condition     = alltrue([for arn in var.bucket_administration_principal_arns : can(regex("^arn:aws[a-z-]*:iam::[0-9]{12}:(role|user|root)", arn))])
    error_message = "bucket_administration_principal_arns must be IAM role, user or account-root ARNs."
  }
}

# ---------------------------------------------------------------------------
# Job queue
# ---------------------------------------------------------------------------

variable "queue_visibility_timeout_seconds" {
  description = <<-EOT
    Must match the worker's 300-second lease (S1's WorkerLimits: a 270-second job budget
    plus 10 seconds of terminal bookkeeping). A shorter timeout redelivers work that is
    still running.
  EOT
  type        = number
  default     = 300

  validation {
    condition     = var.queue_visibility_timeout_seconds >= 300 && var.queue_visibility_timeout_seconds <= 43200
    error_message = "Visibility must be at least the worker's 300-second lease and at most SQS's 12-hour maximum."
  }
}

variable "queue_receive_wait_seconds" {
  description = "Long-poll wait. Zero would short-poll and bill empty receives; the worker expects long polling."
  type        = number
  default     = 20

  validation {
    condition     = var.queue_receive_wait_seconds >= 1 && var.queue_receive_wait_seconds <= 20
    error_message = "queue_receive_wait_seconds must be between 1 and 20."
  }
}

variable "queue_max_receive_count" {
  description = "Deliveries before a job moves to the DLQ. Matches the local emulator's redrive policy in aws.rs."
  type        = number
  default     = 3

  validation {
    condition     = var.queue_max_receive_count >= 1 && var.queue_max_receive_count <= 10
    error_message = "queue_max_receive_count must be between 1 and 10."
  }
}

variable "queue_retention_days" {
  description = "How long an unconsumed job survives on the main queue."
  type        = number
  default     = 4

  validation {
    condition     = var.queue_retention_days >= 1 && var.queue_retention_days <= 14
    error_message = "queue_retention_days must be between 1 and 14."
  }
}

variable "dlq_retention_days" {
  description = "Failed jobs must outlive the main queue's retention so an operator can still investigate them."
  type        = number
  default     = 14

  validation {
    condition     = var.dlq_retention_days >= 1 && var.dlq_retention_days <= 14
    error_message = "dlq_retention_days must be between 1 and 14."
  }

  validation {
    condition     = var.dlq_retention_days >= var.queue_retention_days
    error_message = "dlq_retention_days may not be shorter than queue_retention_days."
  }
}

variable "dlq_operator_principal_arns" {
  description = <<-EOT
    Operator principals allowed to inspect and redrive the dead-letter queue. Empty adds no
    statement and leaves the grant to S6. This is separate from redrive_allow_policy, which
    controls which *source queue* may use this queue as its DLQ, not which human may move
    messages back.
  EOT
  type        = set(string)
  default     = []

  validation {
    condition     = alltrue([for arn in var.dlq_operator_principal_arns : can(regex("^arn:aws[a-z-]*:iam::[0-9]{12}:(role|user)/", arn))])
    error_message = "dlq_operator_principal_arns must be IAM role or user ARNs."
  }
}
