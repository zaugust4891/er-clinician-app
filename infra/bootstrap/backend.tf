# The bootstrap root is the one place with a chicken-and-egg problem: it creates the
# bucket that every other root stores its state in, so its own first apply has nowhere
# remote to write. That is why the backend block below starts commented out.
#
# Phase 1 — create the state bucket with a local state file:
#     terraform -chdir=infra/bootstrap init
#     terraform -chdir=infra/bootstrap apply
#
# Phase 2 — uncomment the block below, then move the local state into the bucket it
# just created. `-migrate-state` copies terraform.tfstate up and rewires the backend:
#     terraform -chdir=infra/bootstrap init -migrate-state \
#       -backend-config="bucket=<state bucket from outputs>" \
#       -backend-config="kms_key_id=<state KMS key ARN from outputs>"
#
# `bucket` and `kms_key_id` are deliberately not written into this file. They are
# account-specific but not secret, so they are passed as `-backend-config` values at
# init time and recorded in docs/ops/deployment-contract.md. Credentials are never
# passed here: Terraform picks them up from the operator's AWS SSO session or, in CI,
# from a short-lived OIDC role.
#
# terraform {
#   backend "s3" {
#     key          = "bootstrap/terraform.tfstate"
#     region       = "us-east-2"
#     encrypt      = true
#     use_lockfile = true
#   }
# }
