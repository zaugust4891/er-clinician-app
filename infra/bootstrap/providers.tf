provider "aws" {
  region = var.aws_region

  # The first guard. If the credentials in the environment belong to a different
  # account, the provider refuses before it plans a single resource. This is what
  # stops a production session from quietly creating staging resources, or worse.
  allowed_account_ids = [var.aws_account_id]

  # Applied to every resource this provider creates, so nothing is untagged and
  # nothing carries clinical data in a tag.
  default_tags {
    tags = {
      Project    = var.project
      Env        = "shared"
      Component  = "terraform-state"
      ManagedBy  = "terraform"
      Root       = "infra/bootstrap"
      Repository = "zaugust4891/er-clinician-app"
    }
  }
}
