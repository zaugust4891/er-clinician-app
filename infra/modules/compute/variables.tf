variable "name" { type = string }
variable "environment" {
  type = string
  validation {
    condition     = contains(["staging", "prod"], var.environment)
    error_message = "Services are cloud-only."
  }
}
variable "aws_account_id" { type = string }
variable "aws_region" { type = string }
variable "resources" { type = map(string) }
variable "secret_refs" { type = map(string) }
variable "image_digest" {
  type = string
  validation {
    condition     = can(regex("^sha256:[0-9a-f]{64}$", var.image_digest))
    error_message = "A release requires an immutable image digest."
  }
}
variable "services_enabled" {
  description = "Render templates first. S11 enables services only after migration, telemetry, and release review."
  type        = bool
  default     = false
}
variable "db_connection_budget" {
  description = "Connections available to this app after RDS/system reserves. Confirm against the actual RDS parameter before activation."
  type        = number
  default     = 35
  validation {
    condition     = var.db_connection_budget >= 35 && floor(var.db_connection_budget) == var.db_connection_budget
    error_message = "Reserve at least 35 connections: four rolling API tasks * 5, one worker * 5, and 10 for operators/one-shots."
  }
}
