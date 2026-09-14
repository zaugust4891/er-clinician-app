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

variable "network" {
  description = "S4 placement and egress. Closed ingress, one NAT, S3 gateway only by default. S5-S9 supply actual resource ARNs before endpoints are enabled."
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
