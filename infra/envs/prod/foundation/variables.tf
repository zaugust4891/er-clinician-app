variable "aws_account_id" {
  description = "The 12-digit AWS account this root may touch. Verify with `aws sts get-caller-identity` first."
  type        = string

  validation {
    condition     = can(regex("^[0-9]{12}$", var.aws_account_id))
    error_message = "aws_account_id must be exactly 12 digits, with no dashes."
  }
}

variable "aws_region" {
  description = "Region this environment runs in."
  type        = string
  default     = "us-east-2"

  validation {
    condition     = contains(["us-east-1", "us-east-2", "us-west-2"], var.aws_region)
    error_message = "aws_region must be one of the US regions recorded in the deployment contract."
  }
}

variable "project" {
  description = "Short, lowercase resource-name prefix. Must match the bootstrap root."
  type        = string
  default     = "ernote"

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{2,11}$", var.project))
    error_message = "project must be 3-12 characters of lowercase letters, digits, or dashes, starting with a letter."
  }
}

variable "deploy_artifacts_bucket" {
  description = "`deploy_artifacts_bucket` output from infra/bootstrap. This root publishes its manifest there."
  type        = string
}

variable "deploy_kms_key_arn" {
  description = "`state_kms_key_arn` output from infra/bootstrap. The artifact bucket requires it for uploads."
  type        = string
}

variable "generated_by" {
  description = <<-EOT
    Who or what produced this manifest: a git commit SHA in CI, or something like
    `local:<operator>` by hand. It is recorded in the manifest so a release can be
    traced back to the foundation run that described it.
  EOT
  type        = string
  default     = "local"

  validation {
    condition     = can(regex("^[A-Za-z0-9._:@/+-]{1,120}$", var.generated_by))
    error_message = "generated_by must be a short plain identifier."
  }
}

variable "enabled" {
  description = <<-EOT
    Production is authored but not provisioned. Everything in this root is gated on
    this flag, so a plan run before the production contract and pre-PHI conditions are
    met shows no resources to create. Flipping it to true is a reviewed decision, not
    a convenience.
  EOT
  type        = bool
  default     = false
}

variable "data" {
  description = <<-EOT
    S5 durable resources. The staging defaults are deliberately *not* inherited for the
    two settings that decide how much data an incident can lose: an enabled production
    plan must state `db_multi_az` and `db_backup_retention_days` itself, from production
    recovery objectives rather than from a synthetic-staging convenience.
  EOT
  type = object({
    postgres_version         = optional(string, "16")
    db_instance_class        = optional(string, "db.t4g.small")
    db_allocated_storage     = optional(number, 50)
    db_max_allocated_storage = optional(number, 100)
    db_multi_az              = optional(bool)
    db_backup_retention_days = optional(number)
    db_backup_window         = optional(string, "07:00-07:30")
    db_maintenance_window    = optional(string, "sun:08:00-sun:09:00")
    db_name                  = optional(string, "ernote")
    db_master_username       = optional(string, "ernote_admin")
    db_ca_cert_identifier    = optional(string)
    db_deletion_protection   = optional(bool, true)
    db_final_snapshot_suffix = optional(string, "v1")
    db_apply_immediately     = optional(bool, false)

    audio_retention_days                    = optional(number)
    audio_noncurrent_version_retention_days = optional(number)
    audio_abort_incomplete_multipart_days   = optional(number, 7)
    bucket_administration_principal_arns    = optional(set(string), [])

    queue_visibility_timeout_seconds = optional(number, 300)
    queue_receive_wait_seconds       = optional(number, 20)
    queue_max_receive_count          = optional(number, 3)
    queue_retention_days             = optional(number, 4)
    dlq_retention_days               = optional(number, 14)
    dlq_operator_principal_arns      = optional(set(string), [])

    kms_deletion_window_days = optional(number, 30)
  })
  default = {}

  validation {
    condition     = !var.enabled || var.data.db_backup_retention_days != null
    error_message = "Set data.db_backup_retention_days explicitly before enabling production; staging's synthetic retention is not a production decision."
  }
  validation {
    condition     = !var.enabled || var.data.db_multi_az != null
    error_message = "Set data.db_multi_az explicitly before enabling this root; superseded audio versions ened their own retention decision."
  }
  # Production audio retention and legal holds are an expert-review decision recorded in
  # the deployment contract, not something this file can settle with a number.
}

variable "network" {
  description = <<-EOT
    S5 durable resources. Enabled production requires explicit db_multi_az,
    db_backup_retention_days, audio_retention_days, and
    audio_noncurrent_version_retention_days values. Production recovery and
    retention decisions must be recorded in the deployment contract.
  EOT
  type = object({
    availability_zones    = optional(list(string))
    vpc_cidr              = optional(string, "10.50.0.0/16")
    nat_per_az            = optional(bool, false)
    client_ipv4_cidrs     = optional(set(string), [])
    http_redirect_enabled = optional(bool, false)
    api_https_port        = optional(number, 8443)
    s3_bucket_arns        = optional(set(string), [])
    interface_endpoints   = optional(map(object({ resource_arns = set(string) })), {})
  })
  default = {}
}
