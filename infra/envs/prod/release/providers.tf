provider "aws" {
  region = var.aws_region

  # Refuses to plan anything if the ambient credentials belong to another account.
  allowed_account_ids = [var.aws_account_id]

  default_tags {
    tags = {
      Project    = var.project
      Env        = local.environment
      Component  = "release"
      ManagedBy  = "terraform"
      Root       = "infra/envs/prod/release"
      Repository = "zaugust4891/er-clinician-app"
    }
  }
}
