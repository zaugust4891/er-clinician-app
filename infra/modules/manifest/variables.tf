variable "enabled" {
  description = "Set false to define the manifest without provisioning it (production before it is enabled)."
  type        = bool
  default     = true
}

variable "bucket" {
  description = "Deployment-artifact bucket created by infra/bootstrap."
  type        = string
}

variable "key" {
  description = "Object key, e.g. envs/staging/foundation-manifest.json."
  type        = string
}

variable "kms_key_arn" {
  description = "KMS key the artifact bucket requires for uploads."
  type        = string
}

variable "manifest" {
  description = <<-EOT
    The non-secret contract between a foundation stack and its release pipeline.

    `resources` maps a logical name to an identifier: an ARN, a queue URL, a bucket
    name, a subnet id. `secret_refs` maps a logical name to a Secrets Manager ARN —
    a pointer to where a value lives, never the value. Nothing in this object is
    allowed to be a password, token, key, or connection string with credentials in it.
  EOT

  type = object({
    schema_version = string
    project        = string
    environment    = string
    account_id     = string
    region         = string
    generated_by   = string
    resources      = map(string)
    secret_refs    = map(string)
  })

  validation {
    condition     = contains(["staging", "prod"], var.manifest.environment)
    error_message = "manifest.environment must be staging or prod."
  }

  validation {
    condition     = can(regex("^[0-9]{12}$", var.manifest.account_id))
    error_message = "manifest.account_id must be 12 digits."
  }

  # The load-bearing check. A Secrets Manager ARN is safe to publish; the secret it
  # points at is not. Requiring the ARN shape makes it impossible to pass a value
  # through this field by accident.
  validation {
    condition = alltrue([
      for value in values(var.manifest.secret_refs) :
      can(regex("^arn:aws[a-z-]*:secretsmanager:[a-z0-9-]+:[0-9]{12}:secret:", value))
    ])
    error_message = "manifest.secret_refs must contain Secrets Manager ARNs only, never secret values."
  }

  validation {
    condition = !anytrue([
      for name in keys(var.manifest.resources) :
      can(regex("(?i)(password|passphrase|secret_value|api_key|private_key|key_material|credential|token)", name))
    ])
    error_message = "manifest.resources must not name credential material. Publish a Secrets Manager ARN under secret_refs instead."
  }

  # A short identifier is a name; a long opaque blob is probably a credential that
  # slipped in. 512 characters comfortably fits every ARN and queue URL in this system.
  validation {
    condition     = alltrue([for value in values(var.manifest.resources) : length(value) > 0 && length(value) <= 512])
    error_message = "Each manifest.resources value must be between 1 and 512 characters."
  }

  validation {
    condition     = can(regex("^[0-9]+\\.[0-9]+$", var.manifest.schema_version))
    error_message = "manifest.schema_version must look like 1.0."
  }
}
