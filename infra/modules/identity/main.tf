# ---------------------------------------------------------------------------
# S6 identities
# ---------------------------------------------------------------------------
# One task role per process that signs AWS calls, and one execution role per process
# ECS starts. An execution role pulls the image, writes that process's logs and
# injects only that process's secrets. The processes share one image but no role.
# The DB bootstrap makes no AWS API call, so it has an execution role and no task
# role; the migration task role is deliberately empty.

locals {
  partition = "aws"

  task_components      = toset(["api", "worker", "migration", "onboarding"])
  execution_components = toset(["api", "worker", "migration", "db-bootstrap", "onboarding"])

  ecr_repository_arn = "arn:${local.partition}:ecr:${var.aws_region}:${var.aws_account_id}:repository/${coalesce(var.ecr_repository_name, var.name)}"

  # S9 creates these groups with exactly these names.
  log_group_names = { for component in local.execution_components : component => "/ecs/${var.name}/${component}" }
  log_group_arns  = { for component, group in local.log_group_names : component => "arn:${local.partition}:logs:${var.aws_region}:${var.aws_account_id}:log-group:${group}" }

  bedrock_profile_arn = "arn:${local.partition}:bedrock:${var.aws_region}:${var.aws_account_id}:inference-profile/${var.bedrock_inference_profile_id}"

  bedrock_model_arns = [
    for region in sort(tolist(var.bedrock_destination_regions)) :
    "arn:${local.partition}:bedrock:${region}::foundation-model/${trimprefix(var.bedrock_inference_profile_id, "us.")}"
  ]

  # ECS may assume these roles only for tasks in this account and Region, which also
  # closes the confused-deputy path through another account's cluster.
  ecs_trust = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Sid       = "EcsTasksInThisAccount"
      Effect    = "Allow"
      Principal = { Service = "ecs-tasks.amazonaws.com" }
      Action    = "sts:AssumeRole"
      Condition = {
        StringEquals = { "aws:SourceAccount" = var.aws_account_id }
        ArnLike      = { "aws:SourceArn" = "arn:${local.partition}:ecs:${var.aws_region}:${var.aws_account_id}:*" }
      }
    }]
  })
}

resource "aws_iam_role" "task" {
  for_each = local.task_components

  name               = "${var.name}-${each.key}-task"
  description        = "Signs the ${each.key} process's own AWS calls."
  assume_role_policy = local.ecs_trust
}

resource "aws_iam_role" "execution" {
  for_each = local.execution_components

  name               = "${var.name}-${each.key}-execution"
  description        = "Starts ${each.key}: image pull, its own logs and its own injected secrets."
  assume_role_policy = local.ecs_trust
}
