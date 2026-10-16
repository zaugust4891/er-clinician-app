variable "name" {
  description = "Environment-qualified prefix, e.g. ernote-staging. Pool, client, key and secret names derive from it."
  type        = string

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{2,24}$", var.name))
    error_message = "name must be lowercase letters, digits and hyphens, starting with a letter."
  }
}

variable "environment" {
  description = "staging or prod. Only staging may carry the synthetic smoke exception."
  type        = string

  validation {
    condition     = contains(["staging", "prod"], var.environment)
    error_message = "environment must be staging or prod."
  }
}

variable "synthetic_smoke_exception" {
  description = <<-EOT
    Synthetic-staging-only exception: MFA is optional in this pool and the client
    allows USER_PASSWORD_AUTH, so automated smoke users can sign in without a second
    factor. The pool must then hold synthetic users only and never patient data.
    Production must not set this; it requires its own reviewed MFA/session policy.
  EOT
  type        = bool
  default     = false

  validation {
    condition     = !var.synthetic_smoke_exception || var.environment == "staging"
    error_message = "The synthetic smoke exception is staging-only; production keeps mandatory MFA and SRP."
  }
}

variable "access_token_minutes" {
  description = "Access-token lifetime. It bounds how long a disabled user's issued token keeps working."
  type        = number
  default     = 60

  validation {
    condition     = var.access_token_minutes >= 5 && var.access_token_minutes <= 60 && floor(var.access_token_minutes) == var.access_token_minutes
    error_message = "access_token_minutes must be a whole number from 5 to 60."
  }
}

variable "refresh_token_hours" {
  description = "Refresh-token lifetime. Revocation is enabled, so a revoked refresh token stops early."
  type        = number
  default     = 12

  validation {
    condition     = var.refresh_token_hours >= 1 && var.refresh_token_hours <= 24 && floor(var.refresh_token_hours) == var.refresh_token_hours
    error_message = "refresh_token_hours must be a whole number from 1 to 24."
  }
}

variable "callback_urls" {
  description = <<-EOT
    Exact hosted-login redirect URLs. Empty disables OAuth on the client entirely
    (CLI/native API sign-in only). Phase 5 adds its reviewed native redirect here
    without replacing the pool. Wildcards and plain http are refused.
  EOT
  type        = list(string)
  default     = []

  validation {
    condition = alltrue([
      for url in var.callback_urls :
      !strcontains(url, "*") && (startswith(url, "https://") || can(regex("^[a-z][a-z0-9.+-]*\\.[a-z0-9.+-]+:/", url)))
    ])
    error_message = "callback_urls must be exact https:// URLs or reverse-DNS native schemes, with no wildcard."
  }
}

variable "logout_urls" {
  description = "Exact sign-out URLs, under the same rules as callback_urls. Requires callback_urls."
  type        = list(string)
  default     = []

  validation {
    condition = alltrue([
      for url in var.logout_urls :
      !strcontains(url, "*") && (startswith(url, "https://") || can(regex("^[a-z][a-z0-9.+-]*\\.[a-z0-9.+-]+:/", url)))
    ])
    error_message = "logout_urls must be exact https:// URLs or reverse-DNS native schemes, with no wildcard."
  }

  validation {
    condition     = length(var.logout_urls) == 0 || length(var.callback_urls) > 0
    error_message = "logout_urls require a hosted-login flow, i.e. callback_urls."
  }
}

variable "kms_deletion_window_days" {
  description = "Days before a scheduled deletion of the synthetic-credential key takes effect."
  type        = number
  default     = 30

  validation {
    condition     = var.kms_deletion_window_days >= 7 && var.kms_deletion_window_days <= 30 && floor(var.kms_deletion_window_days) == var.kms_deletion_window_days
    error_message = "kms_deletion_window_days must be a whole number between 7 and 30."
  }
}

variable "secret_recovery_window_days" {
  description = "Days a deleted synthetic-credential secret remains recoverable. Immediate deletion is refused."
  type        = number
  default     = 7

  validation {
    condition     = var.secret_recovery_window_days >= 7 && var.secret_recovery_window_days <= 30 && floor(var.secret_recovery_window_days) == var.secret_recovery_window_days
    error_message = "secret_recovery_window_days must be a whole number between 7 and 30."
  }
}
