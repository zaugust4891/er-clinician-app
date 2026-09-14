variable "aws_account_id" {
  description = <<-EOT
    The 12-digit AWS account this root is allowed to touch. There is no default on
    purpose: supplying it is what turns `allowed_account_ids` into a real guard.
    Verify the value with `aws sts get-caller-identity` before the first apply.
  EOT
  type        = string

  validation {
    condition     = can(regex("^[0-9]{12}$", var.aws_account_id))
    error_message = "aws_account_id must be exactly 12 digits, with no dashes."
  }
}

variable "aws_region" {
  description = "Region that holds the state and deployment-artifact buckets."
  type        = string
  default     = "us-east-2"

  validation {
    condition     = contains(["us-east-1", "us-east-2", "us-west-2"], var.aws_region)
    error_message = "aws_region must be one of the US regions recorded in the deployment contract."
  }
}

variable "project" {
  description = "Short, lowercase name used as the prefix for every resource created here."
  type        = string
  default     = "ernote"

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{2,11}$", var.project))
    error_message = "project must be 3-12 characters of lowercase letters, digits, or dashes, starting with a letter."
  }
}

variable "state_operator_principals" {
  description = <<-EOT
    IAM principal ARNs allowed to assume the per-scope state roles. Leave empty to
    delegate to the account itself, which lets an Identity Center administrator assume
    them; set it to the exact SSO permission-set role ARNs once they are stable, which
    is narrower. These are the humans and pipelines that touch Terraform state, not the
    application's runtime roles (those are Step 8 S6).
  EOT
  type        = list(string)
  default     = []

  validation {
    condition     = alltrue([for arn in var.state_operator_principals : can(regex("^arn:aws[a-z-]*:iam::[0-9]{12}:(role|user)/", arn))])
    error_message = "state_operator_principals must be IAM role or user ARNs."
  }
}

variable "state_noncurrent_version_retention_days" {
  description = "How long a superseded copy of a state object stays recoverable."
  type        = number
  default     = 365

  validation {
    condition     = var.state_noncurrent_version_retention_days >= 90
    error_message = "Keep at least 90 days of state history so a bad apply stays recoverable."
  }
}
