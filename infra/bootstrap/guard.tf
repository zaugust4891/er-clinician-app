# The second guard, and the one that produces a readable error.
#
# `allowed_account_ids` on the provider fails with a provider-level message. This
# resource asks AWS who we actually are and compares it with what the operator said
# they expected. A precondition is evaluated during `plan`, so a mismatch stops the
# run before anything is created.
#
# `terraform_data` is a built-in resource that stores whatever you put in `input`
# and creates nothing in the cloud. It exists here purely to hang the check on.

data "aws_caller_identity" "current" {}

data "aws_partition" "current" {}

resource "terraform_data" "account_guard" {
  input = {
    expected_account_id = var.aws_account_id
    actual_account_id   = data.aws_caller_identity.current.account_id
    region              = var.aws_region
  }

  lifecycle {
    precondition {
      condition     = data.aws_caller_identity.current.account_id == var.aws_account_id
      error_message = "Wrong AWS account. This root expects ${var.aws_account_id} but the current credentials belong to ${data.aws_caller_identity.current.account_id}. Re-run `aws sso login --profile <the right profile>`."
    }
  }
}





