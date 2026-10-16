# S7 Cognito pool and native client. Off until the read-only inventory of the Step 4
# sandbox pool decides between importing it and creating this separate synthetic
# pool (docs/ops/staging-deploy.md). Enabling it creates a new pool: every user then
# has a new `sub`, and clinician rows must be onboarded against those subjects.
module "auth" {
  source = "../../../modules/auth"
  count  = var.auth.enabled ? 1 : 0

  name                      = "${var.project}-${local.environment}"
  environment               = local.environment
  synthetic_smoke_exception = var.auth.synthetic_smoke_exception
  callback_urls             = var.auth.callback_urls
  logout_urls               = var.auth.logout_urls

  depends_on = [terraform_data.account_guard]
}
