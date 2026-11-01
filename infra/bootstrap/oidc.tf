# Account-wide singleton belongs to bootstrap, never an environment foundation.
# Import an existing provider before setting this true; do not create a duplicate.
variable "github_oidc_enabled" {
  description = "Enable only after inspecting the account and protecting GitHub environments."
  type        = bool
  default     = false
}

resource "aws_iam_openid_connect_provider" "github" {
  count          = var.github_oidc_enabled ? 1 : 0
  url            = "https://token.actions.githubusercontent.com"
  client_id_list = ["sts.amazonaws.com"]
  # IAM validates this provider through its trusted CA library; no stale thumbprint pin.
  lifecycle { prevent_destroy = true }
}

output "github_oidc_provider_arn" {
  value = one(aws_iam_openid_connect_provider.github[*].arn)
}
