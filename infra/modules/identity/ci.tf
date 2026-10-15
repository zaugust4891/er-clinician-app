# S11 enables these templates after adopting the account's OIDC provider and
# protecting all three GitHub environments. Null creates no CI trust or roles.
variable "ci" {
  description = "Reviewed S11 inputs. Infrastructure starts with state access only; its provisioning policy is attached separately after review. Smoke reads one S7 synthetic credential only."
  type = object({
    repository        = string
    environment       = string
    smoke_secret_arn  = string
    smoke_kms_key_arn = string
  })
  default = null

  validation {
    condition = var.ci == null ? true : (
      can(regex("^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$", var.ci.repository)) &&
      contains(["staging", "prod"], var.ci.environment) &&
      endswith(var.name, "-${var.ci.environment}") &&
      can(regex("^arn:aws:secretsmanager:${var.aws_region}:${var.aws_account_id}:secret:", var.ci.smoke_secret_arn)) &&
      can(regex("^arn:aws:kms:${var.aws_region}:${var.aws_account_id}:key/", var.ci.smoke_kms_key_arn)) &&
      var.ci.environment == "staging"
    )
    error_message = "CI requires one exact repository, this staging environment and account-local synthetic secret/key ARNs. Production smoke has no synthetic-credential exception."
  }
}

locals {
  ci_components = var.ci == null ? toset([]) : toset(["infrastructure", "release", "smoke"])
  ci_environment_names = var.ci == null ? {} : {
    infrastructure = "${var.ci.environment}-infrastructure"
    release        = var.ci.environment
    smoke          = "${var.ci.environment}-smoke"
  }
  ci_pass_roles = [
    for component in ["api", "worker", "migration"] : [
      aws_iam_role.task[component].arn, aws_iam_role.execution[component].arn
    ]
  ]
  ci_deny_data = {
    Sid      = "DenyClinicalAndProviderAccess", Effect = "Deny"
    Action   = ["s3:GetObject", "s3:GetObjectVersion", "s3:PutObject", "s3:DeleteObject", "s3:ListBucket"]
    Resource = [var.audio_bucket_arn, "${var.audio_bucket_arn}/*"]
  }
  ci_deny_secrets = {
    Sid      = "DenyApplicationSecrets", Effect = "Deny"
    Action   = ["secretsmanager:GetSecretValue"]
    Resource = concat([var.db_master_user_secret_arn], [for secret in aws_secretsmanager_secret.this : secret.arn])
  }
}

resource "aws_iam_role" "ci" {
  for_each = local.ci_components
  name     = "${var.name}-ci-${each.key}"
  assume_role_policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      Effect    = "Allow", Action = "sts:AssumeRoleWithWebIdentity"
      Principal = { Federated = "arn:aws:iam::${var.aws_account_id}:oidc-provider/token.actions.githubusercontent.com" }
      Condition = { StringEquals = {
        "token.actions.githubusercontent.com:aud" = "sts.amazonaws.com"
        "token.actions.githubusercontent.com:sub" = "repo:${var.ci.repository}:environment:${local.ci_environment_names[each.key]}"
      } }
    }]
  })
}

resource "aws_iam_role_policy_attachment" "ci_state" {
  for_each = var.ci == null ? toset([]) : toset(["infrastructure", "release"])
  role     = aws_iam_role.ci[each.key].name
  # Reuse S3's exact-root access and explicit denies of all other roots' state.
  policy_arn = "arn:aws:iam::${var.aws_account_id}:policy/${trimsuffix(var.name, "-${var.ci.environment}")}-tfstate-${var.ci.environment}-${each.key == "infrastructure" ? "foundation" : "release"}"
}

resource "aws_iam_role_policy" "ci_boundary" {
  for_each = local.ci_components
  role     = aws_iam_role.ci[each.key].id
  name     = "${var.name}-ci-boundary"
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [local.ci_deny_data, local.ci_deny_secrets, {
      Sid      = "NoRoleChainingOrProviders", Effect = "Deny"
      Action   = ["sts:AssumeRole", "sts:AssumeRoleWithWebIdentity", "bedrock:*", "sqs:*"]
      Resource = "*"
    }]
  })
}

resource "aws_iam_role_policy" "ci_release" {
  count = var.ci == null ? 0 : 1
  role  = aws_iam_role.ci["release"].id
  name  = "${var.name}-release"
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      { Sid = "RegistryToken", Effect = "Allow", Action = "ecr:GetAuthorizationToken", Resource = "*" },
      {
        Sid      = "PublishApplicationImage", Effect = "Allow"
        Action   = ["ecr:BatchCheckLayerAvailability", "ecr:InitiateLayerUpload", "ecr:UploadLayerPart", "ecr:CompleteLayerUpload", "ecr:PutImage", "ecr:BatchGetImage", "ecr:DescribeImages"]
        Resource = local.ecr_repository_arn
      },
      {
        # These two ECS APIs have no resource-level authorization. PassRole below
        # still prevents registering a task with a privileged bootstrap identity.
        Sid    = "TaskDefinitionMetadata", Effect = "Allow"
        Action = ["ecs:RegisterTaskDefinition", "ecs:DescribeTaskDefinition"], Resource = "*"
      },
      {
        Sid      = "ApplicationDefinitions", Effect = "Allow"
        Action   = ["ecs:DeregisterTaskDefinition", "ecs:TagResource", "ecs:ListTagsForResource"]
        Resource = [for component in ["api", "worker", "migration"] : "arn:aws:ecs:${var.aws_region}:${var.aws_account_id}:task-definition/${var.name}-${component}:*"]
      },
      {
        Sid      = "DeployApplicationServices", Effect = "Allow"
        Action   = ["ecs:CreateService", "ecs:UpdateService", "ecs:DeleteService", "ecs:DescribeServices", "ecs:TagResource", "ecs:UntagResource", "ecs:ListTagsForResource"]
        Resource = [for component in ["api", "worker"] : "arn:aws:ecs:${var.aws_region}:${var.aws_account_id}:service/${var.name}/${var.name}-${component}"]
      },
      {
        Sid       = "RunMigrationOnly", Effect = "Allow", Action = "ecs:RunTask"
        Resource  = "arn:aws:ecs:${var.aws_region}:${var.aws_account_id}:task-definition/${var.name}-migration:*"
        Condition = { ArnEquals = { "ecs:cluster" = "arn:aws:ecs:${var.aws_region}:${var.aws_account_id}:cluster/${var.name}" } }
      },
      {
        Sid      = "InspectApplicationTasks", Effect = "Allow", Action = ["ecs:DescribeTasks", "ecs:StopTask"]
        Resource = "arn:aws:ecs:${var.aws_region}:${var.aws_account_id}:task/${var.name}/*"
      },
      {
        Sid       = "PassApplicationRolesOnly", Effect = "Allow", Action = "iam:PassRole"
        Resource  = flatten(local.ci_pass_roles)
        Condition = { StringEquals = { "iam:PassedToService" = "ecs-tasks.amazonaws.com" } }
      },
      {
        Sid    = "DenyFoundationMutation", Effect = "Deny", Resource = "*"
        Action = ["ec2:*", "rds:*", "kms:PutKeyPolicy", "kms:CreateGrant", "iam:Create*", "iam:Put*", "iam:Attach*", "iam:Delete*", "iam:Update*", "iam:Detach*", "secretsmanager:Put*", "secretsmanager:Update*", "secretsmanager:Delete*", "secretsmanager:RotateSecret"]
      },
    ]
  })
}

resource "aws_iam_role_policy" "ci_smoke" {
  count = var.ci == null ? 0 : 1
  role  = aws_iam_role.ci["smoke"].id
  name  = "${var.name}-synthetic-smoke"
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [
      { Effect = "Allow", Action = "secretsmanager:GetSecretValue", Resource = var.ci.smoke_secret_arn },
      {
        Effect = "Allow", Action = "kms:Decrypt", Resource = var.ci.smoke_kms_key_arn
        Condition = { StringEquals = {
          "kms:ViaService"                  = "secretsmanager.${var.aws_region}.amazonaws.com"
          "kms:EncryptionContext:SecretARN" = var.ci.smoke_secret_arn
        } }
      },
      { Effect = "Deny", Action = "secretsmanager:GetSecretValue", NotResource = var.ci.smoke_secret_arn },
    ]
  })
}

output "ci_role_arns" {
  description = "Optional S11 role templates. The infrastructure role needs a separately reviewed provisioning policy; production CI remains disabled."
  value       = { for component, role in aws_iam_role.ci : component => role.arn }
}
