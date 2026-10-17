resource "terraform_data" "release_guard" {
  lifecycle {
    precondition {
      condition     = !var.services_enabled || (local.manifest_required && var.image_digest != null)
      error_message = "Services require an enabled environment and a reviewed image digest."
    }
    precondition {
      condition     = var.image_digest == null || (local.manifest_required && try(length(trimspace(var.manifest_version_id)) > 0, false))
      error_message = "Pin a nonempty manifest_version_id before rendering/deploying an image in an enabled environment."
    }
  }
}
module "compute" {
  source               = "../../../modules/compute"
  count                = local.manifest_required && var.image_digest != null ? 1 : 0
  name                 = "${var.project}-${local.environment}"
  environment          = local.environment
  aws_account_id       = var.aws_account_id
  aws_region           = var.aws_region
  resources            = local.manifest.resources
  secret_refs          = local.manifest.secret_refs
  image_digest         = var.image_digest
  services_enabled     = var.services_enabled
  db_connection_budget = var.db_connection_budget
  depends_on           = [terraform_data.account_guard, terraform_data.manifest_guard, terraform_data.release_guard]
}
