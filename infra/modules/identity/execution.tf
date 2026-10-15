# ---------------------------------------------------------------------------
# Secret containers
# ---------------------------------------------------------------------------
# Terraform creates names, encryption and access policy but never a secret version,
# so no value enters state. The DB bootstrap procedure stages and applies database
# logins; an operator puts this environment's own Deepgram key. There is no Anthropic
# secret: cloud AI runs through Bedrock with the worker's task role.

locals {
  secrets = {
    migration_db  = { name = "${var.name}/db/migration", description = "Schema-owner login for er-migrate: JSON username and password." }
    api_db        = { name = "${var.name}/db/api", description = "API runtime login: JSON username and password." }
    worker_db     = { name = "${var.name}/db/worker", description = "Worker runtime login: JSON username and password." }
    onboarding_db = { name = "${var.name}/db/onboarding", description = "One-shot identity setup login: JSON username and password." }
    deepgram      = { name = "${var.name}/deepgram", description = "This environment's own Deepgram project key: JSON api_key." }
  }

  # Execution roles that may read each secret. The bootstrap reads the four logins
  # at explicitly pinned version IDs in its one-shot task definition.
  secret_readers = {
    migration_db  = ["migration", "db-bootstrap"]
    api_db        = ["api", "db-bootstrap"]
    worker_db     = ["worker", "db-bootstrap"]
    onboarding_db = ["onboarding", "db-bootstrap"]
    deepgram      = ["worker"]
  }

  execution_secrets = {
    for component in local.execution_components : component => sort([
      for key, readers in local.secret_readers : key if contains(readers, component)
    ])
  }
}

resource "aws_secretsmanager_secret" "this" {
  for_each = local.secrets

  name                    = each.value.name
  description             = each.value.description
  kms_key_id              = var.kms_key_arn
  recovery_window_in_days = var.secret_recovery_window_days
}

resource "aws_secretsmanager_secret_policy" "this" {
  for_each = local.secrets

  secret_arn = aws_secretsmanager_secret.this[each.key].arn
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = [{
      # A second layer beside IAM: no other principal, including an administrator or
      # a deployment role, reads the value without first changing this policy.
      Sid       = "DenyReadsOutsideInjection"
      Effect    = "Deny"
      Principal = "*"
      Action    = "secretsmanager:GetSecretValue"
      Resource  = "*"
      Condition = { StringNotEquals = { "aws:PrincipalArn" = sort([for component in local.secret_readers[each.key] : aws_iam_role.execution[component].arn]) } }
    }]
  })
}

# ---------------------------------------------------------------------------
# Execution roles: start the process and nothing else
# ---------------------------------------------------------------------------

locals {
  own_secret_arns = {
    for component, keys in local.execution_secrets : component => [for key in keys : aws_secretsmanager_secret.this[key].arn]
  }

  # Only the bootstrap also receives the RDS-managed master credential.
  readable_secrets = {
    for component, arns in local.own_secret_arns : component => concat(arns, component == "db-bootstrap" ? [var.db_master_user_secret_arn] : [])
  }

  secret_statements = {
    for component, arns in local.own_secret_arns : component => concat(
      [for granted in [arns] : {
        Sid      = "InjectOwnSecrets"
        Effect   = "Allow"
        Action   = "secretsmanager:GetSecretValue"
        Resource = granted
      } if length(granted) > 0 && component != "db-bootstrap"],
      [for granted in [arns] : {
        Sid      = "InjectBootstrapLogins"
        Effect   = "Allow"
        Action   = "secretsmanager:GetSecretValue"
        Resource = granted
      } if component == "db-bootstrap"],
      [for granted in [var.db_master_user_secret_arn] : {
        Sid      = "InjectMasterCredential"
        Effect   = "Allow"
        Action   = "secretsmanager:GetSecretValue"
        Resource = granted
      } if component == "db-bootstrap"],
      [for granted in [local.readable_secrets[component]] : {
        Sid      = "DecryptOwnSecrets"
        Effect   = "Allow"
        Action   = "kms:Decrypt"
        Resource = var.kms_key_arn
        Condition = {
          StringEquals = {
            "kms:ViaService"                  = "secretsmanager.${var.aws_region}.amazonaws.com"
            "kms:EncryptionContext:SecretARN" = granted
          }
        }
      } if length(granted) > 0],
      # Every secret this process must not receive, including other processes'
      # logins and, for all but the bootstrap, the master credential.
      [for granted in [local.readable_secrets[component]] : {
        Sid         = "DenyOtherSecrets"
        Effect      = "Deny"
        Action      = "secretsmanager:GetSecretValue"
        NotResource = granted
      } if length(granted) > 0],
      [for granted in [local.readable_secrets[component]] : {
        Sid      = "DenyAllSecrets"
        Effect   = "Deny"
        Action   = "secretsmanager:GetSecretValue"
        Resource = "*"
      } if length(granted) == 0],
    )
  }

  execution_policies = {
    for component in local.execution_components : component => jsonencode({
      Version = "2012-10-17"
      Statement = concat([
        {
          # GetAuthorizationToken has no resource-level scope; pulls are scoped below.
          Sid      = "AuthenticateToRegistry"
          Effect   = "Allow"
          Action   = "ecr:GetAuthorizationToken"
          Resource = "*"
        },
        {
          Sid      = "PullApplicationImage"
          Effect   = "Allow"
          Action   = ["ecr:BatchCheckLayerAvailability", "ecr:GetDownloadUrlForLayer", "ecr:BatchGetImage"]
          Resource = local.ecr_repository_arn
        },
        {
          Sid      = "WriteOwnLogs"
          Effect   = "Allow"
          Action   = ["logs:CreateLogStream", "logs:PutLogEvents"]
          Resource = [local.log_group_arns[component], "${local.log_group_arns[component]}:*"]
        },
      ], local.secret_statements[component])
    })
  }
}

resource "aws_iam_role_policy" "execution" {
  for_each = local.execution_components

  name   = "${var.name}-${each.key}-execution"
  role   = aws_iam_role.execution[each.key].id
  policy = local.execution_policies[each.key]
}
