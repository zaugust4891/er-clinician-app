variable "name" {
  description = "`<project>-<environment>`. Also the ECR repository name, which S6's execution roles already scope their pull permission to."
  type        = string

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{2,40}$", var.name))
    error_message = "name must be lowercase letters, digits or dashes."
  }
}

variable "untagged_expiry_days" {
  description = "Days to keep untagged images. Tagged images are never expired; with immutable tags and single-platform pushes, untagged images are only interrupted pushes."
  type        = number
  default     = 7

  validation {
    condition     = var.untagged_expiry_days >= 1 && var.untagged_expiry_days <= 90
    error_message = "untagged_expiry_days must be between 1 and 90."
  }
}

variable "container_insights" {
  description = "ECS Container Insights. S9 owns telemetry cost decisions; off until then."
  type        = string
  default     = "disabled"

  validation {
    condition     = contains(["disabled", "enabled", "enhanced"], var.container_insights)
    error_message = "container_insights must be disabled, enabled or enhanced."
  }
}
