# S8 image repository and Fargate cluster; created only once production is enabled.
module "cluster" {
  source = "../../../modules/cluster"
  count  = var.enabled ? 1 : 0

  name                 = "${var.project}-${local.environment}"
  untagged_expiry_days = var.cluster.untagged_expiry_days
  container_insights   = var.cluster.container_insights

  depends_on = [terraform_data.account_guard]
}
