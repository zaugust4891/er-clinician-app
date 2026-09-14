# State for this root lives at its own key in the shared state bucket. The key is
# written here because it is reviewed configuration; the bucket name and KMS key are
# account-specific and are supplied at init time:
#
#   terraform -chdir=infra/envs/prod/release init \
#     -backend-config="bucket=$STATE_BUCKET" \
#     -backend-config="kms_key_id=$STATE_KMS_KEY_ARN"
#
# `use_lockfile = true` makes Terraform take its lock by writing a small
# `terraform.tfstate.tflock` object next to the state, using S3's conditional-write
# support. No DynamoDB table is involved. A second operator running the same root at
# the same time is refused the lock rather than allowed to race.
#
# No credentials belong in this file or in a backend.hcl. Terraform picks them up from
# the operator's AWS SSO session or, in CI, from a short-lived OIDC role.
terraform {
  backend "s3" {
    key          = "prod/release/terraform.tfstate"
    region       = "us-east-2"
    encrypt      = true
    use_lockfile = true
  }
}
