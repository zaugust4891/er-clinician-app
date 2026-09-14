provider "aws" {
  region = var.aws_region

  # Refuses to plan anything if the ambient credentials belong to another account.
  allowed_account_ids = [var.aws_account_id]

  default_tags {
    tags = {
      Project    = var.project
      Env        = local.environment
      Component  = "foundation"
      ManagedBy  = "terraform"
      Root       = "infra/envs/staging/foundation"
      Repository = "zaugust4891/er-clinician-app"
    }
  }
}
