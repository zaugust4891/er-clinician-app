# Pure templates: no secret values, ECS registration, DB work or service rollout.
module "operator_task" {
  source             = "../../../modules/task"
  for_each           = (var.enabled && var.operator_image_digest != null) ? toset(["db-bootstrap", "onboarding"]) : toset([])
  family             = "${var.project}-${local.environment}-${each.key}"
  component          = each.key
  image              = "${module.cluster[0].repository_url}@${var.operator_image_digest}"
  aws_region         = var.aws_region
  execution_role_arn = module.identity[0].execution_role_arns[each.key]
  task_role_arn      = each.key == "db-bootstrap" ? null : module.identity[0].task_role_arns.onboarding
  environment = {
    APP_ENV            = local.environment
    AWS_REGION         = var.aws_region
    DB_HOST            = module.data[0].db_host
    DB_PORT            = tostring(module.data[0].db_port)
    DB_NAME            = module.data[0].db_name
    DB_SSL_ROOT_CERT   = "/etc/ssl/certs/rds-global-bundle.pem"
    DB_MAX_CONNECTIONS = "1"
    RUST_LOG           = "info"
  }
  secrets   = module.identity[0].ecs_secrets[each.key]
  log_group = module.identity[0].log_group_names[each.key]
}
