locals {
  processes = toset(["api", "worker", "migration"])
  resource_keys = concat([
    "ecr_repository_url", "ecs_cluster_arn", "private_subnet_ids", "api_target_group_arn",
    "api_security_group_id", "worker_security_group_id", "migration_security_group_id",
    "api_https_port", "db_endpoint", "db_port", "db_name", "audio_bucket", "stt_queue_name",
    "cognito_pool_id", "cognito_client_id", "cognito_region"
  ], flatten([for p in local.processes : ["${p}_task_role_arn", "${p}_execution_role_arn", "${p}_log_group"]]))
  # Defaults let the guard report one actionable error for an incomplete manifest.
  resources       = merge({ for key in local.resource_keys : key => "" }, var.resources)
  secret_refs     = merge({ api_db = "", worker_db = "", migration_db = "", deepgram = "" }, var.secret_refs)
  image           = "${local.resources.ecr_repository_url}@${var.image_digest}"
  private_subnets = try(tolist(jsondecode(local.resources.private_subnet_ids)), [])
  common_env = {
    APP_ENV                 = var.environment
    AWS_REGION              = var.aws_region
    DB_HOST                 = local.resources.db_endpoint
    DB_PORT                 = local.resources.db_port
    DB_NAME                 = local.resources.db_name
    DB_SSL_ROOT_CERT        = "/etc/ssl/certs/rds-global-bundle.pem"
    DB_ACQUIRE_TIMEOUT_SECS = "5"
    DB_CONNECT_TIMEOUT_SECS = "10"
    RUST_LOG                = "info"
  }
  process_env = {
    api = {
      AUDIO_BUCKET      = local.resources.audio_bucket
      STT_QUEUE         = local.resources.stt_queue_name
      AUTH_MODE         = "cognito"
      COGNITO_REGION    = local.resources.cognito_region
      COGNITO_POOL_ID   = local.resources.cognito_pool_id
      COGNITO_CLIENT_ID = local.resources.cognito_client_id
      BIND_ADDR         = "0.0.0.0:${local.resources.api_https_port}"
      API_TLS           = "self-signed"
    }
    worker = {
      AUDIO_BUCKET  = local.resources.audio_bucket
      STT_QUEUE     = local.resources.stt_queue_name
      TRANSPORT     = "bedrock"
      SUMMARY_MODEL = "sonnet"
      MDM_MODEL     = "sonnet"
      # Must agree with S6's exact profile/model IAM resources and S0 residency.
      BEDROCK_SUMMARY_MODEL_ID = "us.anthropic.claude-sonnet-4-6"
      BEDROCK_MDM_MODEL_ID     = "us.anthropic.claude-sonnet-4-6"
    }
    migration = {}
  }
}
resource "terraform_data" "contract" {
  lifecycle {
    precondition {
      condition     = alltrue([for k in local.resource_keys : local.resources[k] != ""])
      error_message = "The release requires the complete S8 edge and S9 log-group manifest before task templates can be rendered."
    }
    precondition {
      condition = (
        local.resources.ecr_repository_url == "${var.aws_account_id}.dkr.ecr.${var.aws_region}.amazonaws.com/${var.name}" &&
        local.resources.ecs_cluster_arn == "arn:aws:ecs:${var.aws_region}:${var.aws_account_id}:cluster/${var.name}" &&
        can(regex("^arn:aws:elasticloadbalancing:${var.aws_region}:${var.aws_account_id}:targetgroup/${var.name}-api/", local.resources.api_target_group_arn))
      )
      error_message = "ECR, cluster and target group must belong to this environment/account/region."
    }
    precondition {
      condition = alltrue([for p in local.processes : (
        local.resources["${p}_log_group"] == "/ecs/${var.name}/${p}" &&
        local.resources["${p}_task_role_arn"] == "arn:aws:iam::${var.aws_account_id}:role/${var.name}-${p}-task" &&
        local.resources["${p}_execution_role_arn"] == "arn:aws:iam::${var.aws_account_id}:role/${var.name}-${p}-execution"
      )])
      error_message = "Each process must use its S6 role pair and S9 log group, within the execution-policy scope."
    }
    precondition {
      condition = alltrue([for k in ["api_db", "worker_db", "migration_db", "deepgram"] : can(regex(
        "^arn:aws:secretsmanager:${var.aws_region}:${var.aws_account_id}:secret:${var.name}/${k == "deepgram" ? "deepgram" : "db/${trimsuffix(k, "_db")}"}-[A-Za-z0-9]{6}$",
        local.secret_refs[k]
      ))])
      error_message = "Secrets must be the process-specific S6 containers in this account/region."
    }
    precondition {
      condition     = length(distinct(local.private_subnets)) == 2 && alltrue([for s in local.private_subnets : can(regex("^subnet-[0-9a-f]+$", s))])
      error_message = "Use the two private foundation subnets encoded as a JSON array."
    }
    precondition {
      condition = (
        local.resources.cognito_region == var.aws_region &&
        can(regex("^${var.aws_region}_[A-Za-z0-9]+$", local.resources.cognito_pool_id)) &&
        can(regex("^[A-Za-z0-9]+$", local.resources.cognito_client_id)) &&
        can(regex("^[0-9]+$", local.resources.api_https_port))
      )
      error_message = "Cloud API requires valid regional Cognito identifiers and its HTTPS port."
    }
  }
}
module "task" {
  source             = "../task"
  for_each           = local.processes
  family             = "${var.name}-${each.key}"
  component          = each.key
  image              = local.image
  aws_region         = var.aws_region
  execution_role_arn = local.resources["${each.key}_execution_role_arn"]
  task_role_arn      = local.resources["${each.key}_task_role_arn"]
  environment        = merge(local.common_env, local.process_env[each.key], { DB_MAX_CONNECTIONS = each.key == "migration" ? "1" : "5" })
  secrets = merge({
    DB_USERNAME = "${local.secret_refs["${each.key}_db"]}:username::"
    DB_PASSWORD = "${local.secret_refs["${each.key}_db"]}:password::"
  }, each.key == "worker" ? { DEEPGRAM_API_KEY = "${local.secret_refs.deepgram}:api_key::" } : {})
  log_group      = local.resources["${each.key}_log_group"]
  api_https_port = try(tonumber(local.resources.api_https_port), 8443)
  depends_on     = [terraform_data.contract]
}
# Migration is an operator/pipeline one-shot payload, never a service and never
# implicitly executed by terraform apply. S11 registers/runs/checks it first.
resource "aws_ecs_task_definition" "service" {
  for_each = var.services_enabled ? toset(["api", "worker"]) : toset([])
  # Keep prior accepted revisions runnable for S11 rollback. Reference-aware
  # cleanup is explicit; Terraform must not deregister them on replacement.
  skip_destroy             = true
  family                   = module.task[each.key].definition.family
  network_mode             = "awsvpc"
  requires_compatibilities = ["FARGATE"]
  cpu                      = "512"
  memory                   = "1024"
  execution_role_arn       = local.resources["${each.key}_execution_role_arn"]
  task_role_arn            = local.resources["${each.key}_task_role_arn"]
  runtime_platform {
    cpu_architecture        = "X86_64"
    operating_system_family = "LINUX"
  }
  container_definitions = jsonencode(module.task[each.key].definition.containerDefinitions)
  lifecycle { create_before_destroy = true }
}
resource "aws_ecs_service" "app" {
  for_each                           = aws_ecs_task_definition.service
  name                               = "${var.name}-${each.key}"
  cluster                            = local.resources.ecs_cluster_arn
  task_definition                    = each.value.arn
  launch_type                        = "FARGATE"
  platform_version                   = "1.4.0"
  desired_count                      = each.key == "api" ? 2 : 1
  deployment_minimum_healthy_percent = each.key == "api" ? 100 : 0
  deployment_maximum_percent         = each.key == "api" ? 200 : 100
  availability_zone_rebalancing      = each.key == "api" ? "ENABLED" : "DISABLED"
  enable_execute_command             = false
  wait_for_steady_state              = true
  health_check_grace_period_seconds  = each.key == "api" ? 120 : null
  deployment_circuit_breaker {
    enable   = true
    rollback = true
  }
  network_configuration {
    subnets          = local.private_subnets
    security_groups  = [local.resources["${each.key}_security_group_id"]]
    assign_public_ip = false
  }
  dynamic "load_balancer" {
    for_each = each.key == "api" ? [1] : []
    content {
      target_group_arn = local.resources.api_target_group_arn
      container_name   = "api"
      container_port   = tonumber(local.resources.api_https_port)
    }
  }
  timeouts {
    create = "20m"
    update = "20m"
  }
}
