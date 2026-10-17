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
  description = "`deploy_artifacts_bucket` output from infra/bootstrap. This root reads its environment's manifest from there."
  type        = string
}

variable "manifest_version_id" {
  description = <<-EOT
    Pin the release to one exact published version of the foundation manifest. Leave
    null to read the current version, which is fine by hand and wrong in CI: a release
    that was reviewed against one manifest must not apply against another.
  EOT
  type        = string
  default     = null
}

variable "image_digest" {
  description = <<-EOT
    The immutable image to run, as `sha256:<64 hex characters>`. A tag names a release;
    only a digest fixes its bytes, and ECS task definitions here reference
    `<repository>@<digest>`. Supplied by the deploy workflow after it pushes and
    verifies the image. Set it to render templates before enabling services.
  EOT
  type        = string
  default     = null

  validation {
    condition     = var.image_digest == null || can(regex("^sha256:[0-9a-f]{64}$", var.image_digest))
    error_message = "image_digest must be sha256: followed by 64 lowercase hex characters."
  }
}

variable "enabled" {
  description = <<-EOT
    Production services are authored but not provisioned. Everything in this root is
    gated on this flag. Flipping it to true is a reviewed decision gated on the
    production contract and the pre-PHI conditions in S0.
  EOT
  type        = bool
  default     = false
}

variable "services_enabled" {
  description = "S11 activation after the exact-digest migration succeeds and S9 telemetry is installed. False renders templates without deploying services."
  type        = bool
  default     = false
}
variable "db_connection_budget" {
  description = "Application connections available after database/system reserves; initial required budget is 35."
  type        = number
  default     = 35
}
