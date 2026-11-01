# S6 permissions and secret containers; values are populated outside Terraform.
module "identity" {
  source = "../../../modules/identity"
  count  = 1

  ci = var.ci == null ? null : {
    repository              = var.ci.repository
    environment             = var.ci.environment
    smoke_secret_arn        = var.ci.smoke_secret_arn
    smoke_kms_key_arn       = var.ci.smoke_kms_key_arn
    subjects                = var.ci.subjects
    deploy_artifacts_bucket = var.deploy_artifacts_bucket
    deploy_kms_key_arn      = var.deploy_kms_key_arn
    private_subnet_ids      = module.network[0].private_subnet_ids
  }
  name                      = "${var.project}-${local.environment}"
  aws_region                = var.aws_region
  aws_account_id            = var.aws_account_id
  kms_key_arn               = module.data[0].kms_key_arn
  audio_bucket_arn          = local.audio_bucket_arn
  stt_queue_arn             = "arn:aws:sqs:${var.aws_region}:${var.aws_account_id}:${var.project}-${local.environment}-jobs"
  stt_dlq_arn               = "arn:aws:sqs:${var.aws_region}:${var.aws_account_id}:${var.project}-${local.environment}-jobs-dlq"
  db_master_user_secret_arn = module.data[0].db_master_user_secret_arn
  cognito_user_pool_arn     = one(module.auth[*].user_pool_arn)

  depends_on = [terraform_data.account_guard]
}
