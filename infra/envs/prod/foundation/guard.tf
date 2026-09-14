# Two guards, the same pair every root uses.
#
#  1. `allowed_account_ids` on the provider (providers.tf) stops the run at the
#     provider level.
#  2. The precondition below asks AWS who the credentials actually belong to and
#     compares it with what the operator declared. Preconditions are checked during
#     `plan`, so a mismatch fails before a single resource is created.

data "aws_caller_identity" "current" {}

resource "terraform_data" "account_guard" {
  input = {
    expected_account_id = var.aws_account_id
    actual_account_id   = data.aws_caller_identity.current.account_id
    environment         = local.environment
    region              = var.aws_region
  }

  lifecycle {
    precondition {
      condition     = data.aws_caller_identity.current.account_id == var.aws_account_id
      error_message = "Wrong AWS account for the prod foundation root: expected ${var.aws_account_id}, got ${data.aws_caller_identity.current.account_id}."
    }
  }
}
