variable "name" {
  description = "Environment-qualified prefix, e.g. ernote-staging. Role, secret and log-group names derive from it."
  type        = string

  validation {
    condition     = can(regex("^[a-z][a-z0-9-]{2,24}$", var.name))
    error_message = "name must be lowercase letters, digits and hyphens, starting with a letter."
  }
}

variable "aws_region" {
  description = "Region the tasks run in, which is also the Bedrock source Region."
  type        = string

  validation {
    condition     = can(regex("^us-[a-z]+-[0-9]$", var.aws_region))
    error_message = "aws_region must be a US commercial Region such as us-east-2."
  }
}

variable "aws_account_id" {
  description = "Account that owns the roles. ECS may assume them only for tasks in this account."
  type        = string

  validation {
    condition     = can(regex("^[0-9]{12}$", var.aws_account_id))
    error_message = "aws_account_id must be exactly 12 digits, with no dashes."
  }
}

variable "kms_key_arn" {
  description = "S5 application-data key. Audio, queue payloads and every secret created here are encrypted under it."
  type        = string

  validation {
    condition     = can(regex("^arn:aws:kms:[a-z0-9-]+:[0-9]{12}:key/[a-f0-9-]+$", var.kms_key_arn))
    error_message = "kms_key_arn must be a key ARN. An alias ARN does not match key-scoped grants."
  }
}

variable "audio_bucket_arn" {
  description = "S5 audio bucket ARN, composed by the calling root."
  type        = string

  validation {
    condition     = can(regex("^arn:aws:s3:::[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$", var.audio_bucket_arn))
    error_message = "audio_bucket_arn must be a bucket ARN, not an object ARN."
  }
}

variable "stt_queue_arn" {
  description = <<-EOT
    Main job queue ARN, composed by the calling root from the queue name. The queue is
    encrypted under the data key, whose policy includes this module's statements, so
    reading the queue resource's ARN here would be a dependency cycle.
  EOT
  type        = string

  validation {
    condition     = can(regex("^arn:aws:sqs:[a-z0-9-]+:[0-9]{12}:[A-Za-z0-9_-]{1,76}$", var.stt_queue_arn))
    error_message = "stt_queue_arn must be a standard SQS queue ARN."
  }
}

variable "stt_dlq_arn" {
  description = "Dead-letter queue ARN. The application derives its name as STT_QUEUE plus -dlq."
  type        = string

  validation {
    condition     = var.stt_dlq_arn == "${var.stt_queue_arn}-dlq"
    error_message = "stt_dlq_arn must be the main queue's ARN plus -dlq, the name Config::stt_dlq resolves."
  }
}

variable "db_master_user_secret_arn" {
  description = "RDS-managed master secret. Only the one-shot DB bootstrap's execution role may read it."
  type        = string

  validation {
    condition     = can(regex("^arn:aws:secretsmanager:[a-z0-9-]+:[0-9]{12}:secret:", var.db_master_user_secret_arn))
    error_message = "db_master_user_secret_arn must be a Secrets Manager secret ARN."
  }
}

variable "bedrock_inference_profile_id" {
  description = <<-EOT
    The worker's approved Bedrock route, the value of BEDROCK_SUMMARY_MODEL_ID and
    BEDROCK_MDM_MODEL_ID. Only a US geographic profile is accepted, never global.
  EOT
  type        = string
  default     = "us.anthropic.claude-sonnet-4-6"

  validation {
    condition     = can(regex("^us\\.anthropic\\.[a-z0-9.-]+$", var.bedrock_inference_profile_id))
    error_message = "bedrock_inference_profile_id must be a us.anthropic.* geographic inference profile."
  }
}

variable "bedrock_destination_regions" {
  description = "Regions the profile routes to. The worker may invoke the model there only through the profile."
  type        = set(string)
  default     = ["us-east-1", "us-east-2", "us-west-2"]

  validation {
    condition     = length(var.bedrock_destination_regions) > 0 && alltrue([for region in var.bedrock_destination_regions : can(regex("^us-[a-z]+-[0-9]$", region))])
    error_message = "bedrock_destination_regions must list the profile's US destination Regions."
  }
}

variable "ecr_repository_name" {
  description = "S8's application image repository; null uses name. Execution roles may pull from it only."
  type        = string
  default     = null
}

variable "cognito_user_pool_arn" {
  description = "S7's user pool. While null, the onboarding task role has no Cognito permission."
  type        = string
  default     = null

  validation {
    condition     = var.cognito_user_pool_arn == null || can(regex("^arn:aws:cognito-idp:[a-z0-9-]+:[0-9]{12}:userpool/[a-z0-9-]+_[A-Za-z0-9]+$", var.cognito_user_pool_arn))
    error_message = "cognito_user_pool_arn must be a Cognito user pool ARN."
  }
}

variable "secret_recovery_window_days" {
  description = "Days a deleted secret remains recoverable. Immediate deletion is refused."
  type        = number
  default     = 7

  validation {
    condition     = var.secret_recovery_window_days >= 7 && var.secret_recovery_window_days <= 30 && floor(var.secret_recovery_window_days) == var.secret_recovery_window_days
    error_message = "secret_recovery_window_days must be a whole number between 7 and 30."
  }
}
