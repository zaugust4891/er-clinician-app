variable "name" { type = string }
variable "aws_account_id" { type = string }
variable "aws_region" { type = string }
variable "audio_bucket_arn" { type = string }
variable "queue_name" { type = string }
variable "dlq_name" { type = string }
variable "db_identifier" { type = string }
variable "edge" {
  type    = object({ alb = string, target_group = string })
  default = null
}
variable "alarm_emails" {
  type    = set(string)
  default = []
  validation {
    condition     = alltrue([for email in var.alarm_emails : can(regex("^[^@\\s]+@[^@\\s]+\\.[^@\\s]+$", email))])
    error_message = "Supply reviewed operator email destinations. Subscription confirmation is a separate live gate."
  }
}
variable "alarms_enabled" {
  type        = bool
  default     = false
  description = "Enable runtime notifications before services/ingress. Confirm subscription delivery first."
}
variable "operator_principal_arns" {
  type    = set(string)
  default = []
  validation {
    condition     = alltrue([for arn in var.operator_principal_arns : can(regex("^arn:aws:iam::[0-9]{12}:role/", arn))])
    error_message = "Use exact reviewed IAM operator role ARNs, not account-wide trust."
  }
}
variable "log_retention_days" {
  type    = number
  default = 14
  validation {
    condition     = contains([7, 14, 30], var.log_retention_days)
    error_message = "Application log retention must be 7, 14 or 30 days."
  }
}
variable "audit_retention_days" {
  type    = number
  default = 365
  validation {
    condition     = var.audit_retention_days >= 90 && var.audit_retention_days <= 2555 && floor(var.audit_retention_days) == var.audit_retention_days
    error_message = "Choose 90–2555 whole days of restricted audit retention."
  }
}
variable "audio_data_events" {
  type        = bool
  default     = false
  description = "Explicitly opt into restricted audio object metadata after estimating volume and reviewing canaries."
}
variable "monthly_budget_usd" {
  type    = number
  default = 50
  validation {
    condition     = var.monthly_budget_usd > 0
    error_message = "A positive reviewed monthly budget is required. This is a notification threshold, not spending authorization."
  }
}
variable "db_connection_threshold" {
  type    = number
  default = 35
  validation {
    condition     = var.db_connection_threshold >= 25
    error_message = "Reserve at least the 25 connections possible during normal service rollout; review against the live RDS limit."
  }
}

variable "db_allocated_storage_gib" {
  type    = number
  default = 20
  validation {
    condition     = var.db_allocated_storage_gib > 0
    error_message = "Storage alarm needs a positive allocated capacity. Review after RDS autoscaling."
  }
}
variable "ecs_services" {
  description = "Desired counts, keyed by service name. Requires Container Insights on the named cluster."
  type        = map(number)
  default     = {}
  validation {
    condition     = alltrue([for count in values(var.ecs_services) : count >= 1 && floor(count) == count])
    error_message = "Running services require a positive whole desired count."
  }
}
