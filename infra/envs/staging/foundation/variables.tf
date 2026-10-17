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


variable "data" {
  description = <<-EOT
    S5 durable resources. The defaults are S0's synthetic-staging decisions: a single-AZ
    db.t4g.small on 50 GiB gp3, 7-day automated backups, 7-day audio expiry, and a
    4-day/14-day queue and dead-letter pair. Verify the class and storage combination in
    the region before the first apply. `db_multi_az` and `db_backup_retention_days` are
    null here and defaulted by the root, so the production root can require an explicit
    choice for both.
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

    audio_retention_days                    = optional(number, 7)
    audio_noncurrent_version_retention_days = optional(number, 7)
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
}

variable "network" {
  description = "S4 placement and egress. Closed ingress, one NAT, S3 gateway only by default. S5-S9 supply actual resource ARNs before endpoints are enabled."
  type = object({
    availability_zones    = optional(list(string))
    vpc_cidr              = optional(string, "10.40.0.0/16")
    nat_per_az            = optional(bool, false)
    client_ipv4_cidrs     = optional(set(string), [])
    http_redirect_enabled = optional(bool, false)
    api_https_port        = optional(number, 8443)
    s3_bucket_arns        = optional(set(string), [])
    interface_endpoints   = optional(map(object({ resource_arns = set(string) })), {})
  })
  default = {}
}

variable "auth" {
  description = <<-EOT
    S7 Cognito. Leave disabled until the sandbox-pool inventory is recorded. Staging
    defaults to the labeled synthetic smoke exception (optional MFA and password
    sign-in for synthetic users only); hosted login stays off without exact URLs.
  EOT
  type = object({
    enabled                   = optional(bool, false)
    synthetic_smoke_exception = optional(bool, true)
    callback_urls             = optional(list(string), [])
    logout_urls               = optional(list(string), [])
  })
  default = {}
}

variable "cluster" {
  description = "S8 ECR untagged-image expiry and cluster settings. Tagged images are never expired; see infra/modules/cluster."
  type = object({
    untagged_expiry_days = optional(number, 7)
    container_insights   = optional(string, "disabled")
  })
  default = {}
}

variable "ci" {
  description = "S11 opt-in role templates; leave null until GitHub environments and OIDC are reviewed."
  type = object({
    repository        = string
    environment       = string
    smoke_secret_arn  = string
    smoke_kms_key_arn = string
  })
  default = null
}

variable "edge" {
  description = "S8 opt-in ALB/ACM/DNS/WAF. Null creates no edge; an edge starts closed until explicit synthetic validation. Use an existing public Route53 zone."
  type = object({
    hostname        = string
    hosted_zone_id  = string
    ingress_enabled = optional(bool, false)
    rate_limit      = optional(number, 2000)
  })
  default = null
}
variable "operator_image_digest" {
  description = "Optional reviewed digest for private bootstrap/onboarding JSON templates. Produces no registered/running tasks; S9 must create their log groups first."
  type        = string
  default     = null
  validation {
    condition     = var.operator_image_digest == null || can(regex("^sha256:[0-9a-f]{64}$", var.operator_image_digest))
    error_message = "operator_image_digest must be sha256 followed by 64 lowercase hex characters."
  }
}
