# S6 permissions and secret containers; values are populated outside Terraform.
module "identity" {
  source = "../../../modules/identity"
  count  = 1

  ci                        = var.ci
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
