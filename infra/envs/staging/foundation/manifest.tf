# Publish the contract the release root consumes. See infra/modules/manifest/README.md
# for why this is a manifest object and not `terraform_remote_state`.

module "manifest" {
  source = "../../../modules/manifest"

  depends_on = [module.network]

  enabled     = local.manifest_enabled
  bucket      = var.deploy_artifacts_bucket
  key         = local.manifest_key
  kms_key_arn = var.deploy_kms_key_arn

  manifest = {
    schema_version = "1.0"
    project        = var.project
    environment    = local.environment
    account_id     = var.aws_account_id
    region         = var.aws_region
    generated_by   = var.generated_by
    resources      = local.manifest_resources
    secret_refs    = local.manifest_secret_refs
  }
}
