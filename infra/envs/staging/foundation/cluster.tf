# S8 image repository and Fargate cluster. The repository name is the same
# `<project>-<environment>` string S6's execution roles already scope pulls to.
module "cluster" {
  source = "../../../modules/cluster"
  count  = 1 # Match the indexed interface of the gated production root.

  name                 = "${var.project}-${local.environment}"
  untagged_expiry_days = var.cluster.untagged_expiry_days
  container_insights   = var.cluster.container_insights

  depends_on = [terraform_data.account_guard]
}
