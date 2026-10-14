# Plan only: the protected bootstrap resources must never be mock-applied and
# destroyed for teardown. Request-case behavior is tested in the shared pure module.
mock_provider "aws" {
  mock_data "aws_caller_identity" {
    defaults = { account_id = "000000000000" }
  }
  mock_data "aws_iam_policy_document" {
    defaults = { json = "{}" }
  }
}

variables {
  aws_account_id = "000000000000"
}

run "both_bootstrap_buckets_use_encryption_guards" {
  command = plan
  assert {
    condition = alltrue([
      for statements in values(local.bucket_guard_statements) :
      length(statements) == 6 &&
      alltrue([for statement in statements : statement.Effect == "Deny"]) &&
      contains([for statement in statements : statement.Sid], "DenyKmsWithoutKey") &&
      contains([for statement in statements : statement.Sid], "DenyCustomerProvidedEncryption")
    ])
    error_message = "Both bootstrap buckets must include the shared encryption guards and transport denies."
  }
}
