# ---------------------------------------------------------------------------
# Task roles: each process's own SDK calls
# ---------------------------------------------------------------------------
# Every allow follows a call in the code: presigned PutObject and HeadObject in
# routes.rs, GetObject in audio.rs, SendMessage in queue.rs, Receive, Delete and
# ChangeMessageVisibility in worker.rs, GetQueueUrl for both queues in aws.rs and
# InvokeModel in er-ai's transport.rs. HeadObject is authorized by s3:GetObject;
# there is no s3:HeadObject action. Presigning is local, but S3 evaluates the
# signing role when the device uploads. Explicit denies preserve the listed
# forbidden operations if another policy is attached later.

locals {
  # S3 sends the bucket ARN as encryption context with Bucket Keys enabled and the
  # object ARN without them. Accepting both keeps either setting working.
  s3_kms_context = [var.audio_bucket_arn, "${var.audio_bucket_arn}/*"]

  queue_lookup = {
    Sid      = "ResolveQueues"
    Effect   = "Allow"
    Action   = "sqs:GetQueueUrl"
    Resource = [var.stt_queue_arn, var.stt_dlq_arn]
  }

  task_allow = {
    api = [
      {
        Sid      = "SignUploadsAndVerifyThem"
        Effect   = "Allow"
        Action   = ["s3:PutObject", "s3:GetObject"]
        Resource = "${var.audio_bucket_arn}/*"
      },
      {
        # Without ListBucket a missing key's HEAD returns 403, and verify_chunk_object
        # would report a storage fault instead of chunk_not_uploaded. It exposes key
        # names to this already-privileged role; no route lists objects.
        Sid      = "DistinguishMissingUploads"
        Effect   = "Allow"
        Action   = "s3:ListBucket"
        Resource = var.audio_bucket_arn
      },
      {
        Sid      = "EnqueueJobs"
        Effect   = "Allow"
        Action   = "sqs:SendMessage"
        Resource = var.stt_queue_arn
      },
      local.queue_lookup,
      {
        # S3 Bucket Key use also needs Decrypt; S5 enables Bucket Keys.
        Sid      = "EncryptUploads"
        Effect   = "Allow"
        Action   = ["kms:GenerateDataKey", "kms:Decrypt"]
        Resource = var.kms_key_arn
        Condition = {
          StringEquals = { "kms:ViaService" = "s3.${var.aws_region}.amazonaws.com" }
          StringLike   = { "kms:EncryptionContext:aws:s3:arn" = local.s3_kms_context }
        }
      },
      {
        # An SQS producer also needs Decrypt to reuse its cached data key.
        Sid      = "EncryptJobs"
        Effect   = "Allow"
        Action   = ["kms:GenerateDataKey", "kms:Decrypt"]
        Resource = var.kms_key_arn
        Condition = {
          StringEquals = {
            "kms:ViaService"                    = "sqs.${var.aws_region}.amazonaws.com"
            "kms:EncryptionContext:aws:sqs:arn" = var.stt_queue_arn
          }
        }
      },
    ]
    worker = [
      {
        Sid      = "ReadUploadedAudio"
        Effect   = "Allow"
        Action   = "s3:GetObject"
        Resource = "${var.audio_bucket_arn}/*"
      },
      {
        Sid      = "ConsumeJobs"
        Effect   = "Allow"
        Action   = ["sqs:ReceiveMessage", "sqs:DeleteMessage", "sqs:ChangeMessageVisibility"]
        Resource = var.stt_queue_arn
      },
      local.queue_lookup,
      {
        Sid      = "DecryptAudio"
        Effect   = "Allow"
        Action   = "kms:Decrypt"
        Resource = var.kms_key_arn
        Condition = {
          StringEquals = { "kms:ViaService" = "s3.${var.aws_region}.amazonaws.com" }
          StringLike   = { "kms:EncryptionContext:aws:s3:arn" = local.s3_kms_context }
        }
      },
      {
        Sid      = "DecryptJobs"
        Effect   = "Allow"
        Action   = "kms:Decrypt"
        Resource = var.kms_key_arn
        Condition = {
          StringEquals = {
            "kms:ViaService"                    = "sqs.${var.aws_region}.amazonaws.com"
            "kms:EncryptionContext:aws:sqs:arn" = var.stt_queue_arn
          }
        }
      },
      {
        Sid      = "InvokeApprovedProfile"
        Effect   = "Allow"
        Action   = "bedrock:InvokeModel"
        Resource = local.bedrock_profile_arn
      },
      {
        # The profile's destination models, invocable only through that profile.
        Sid       = "InvokeProfileDestinations"
        Effect    = "Allow"
        Action    = "bedrock:InvokeModel"
        Resource  = local.bedrock_model_arns
        Condition = { StringEquals = { "bedrock:InferenceProfileArn" = local.bedrock_profile_arn } }
      },
    ]
    migration = []
    onboarding = [for pool in compact([var.cognito_user_pool_arn]) : {
      # Reads a user's subject for the clinician bridge row. S7 adds anything more.
      Sid      = "ReadUserSubjects"
      Effect   = "Allow"
      Action   = "cognito-idp:AdminGetUser"
      Resource = pool
    }]
  }

  # Infrastructure no task may create or change, and secrets no task role reads:
  # ECS injects secrets with the execution role before the process starts.
  task_denied_everywhere = [
    "sts:AssumeRole", "sts:AssumeRoleWithWebIdentity",
    "ecs:*", "ec2:*", "iam:*", "rds:*", "secretsmanager:*",
    "kms:CreateGrant", "kms:DisableKey", "kms:PutKeyPolicy", "kms:ScheduleKeyDeletion",
    "s3:CreateBucket", "s3:DeleteBucket*", "s3:PutBucket*", "s3:PutEncryptionConfiguration",
    "s3:PutLifecycleConfiguration", "sqs:CreateQueue", "sqs:DeleteQueue", "sqs:PurgeQueue",
    "sqs:SetQueueAttributes",
  ]

  task_denied = {
    api        = ["bedrock:*", "cognito-idp:*", "sqs:ChangeMessageVisibility", "sqs:DeleteMessage", "sqs:ReceiveMessage"]
    worker     = ["cognito-idp:*", "s3:DeleteObject", "s3:PutObject", "sqs:SendMessage"]
    migration  = ["bedrock:*", "cognito-idp:*", "s3:*", "sqs:*"]
    onboarding = ["bedrock:*", "s3:*", "sqs:*"]
  }

  task_policies = {
    for component in local.task_components : component => jsonencode({
      Version = "2012-10-17"
      Statement = concat(local.task_allow[component], [{
        Sid      = "DenyOutsideBoundary"
        Effect   = "Deny"
        Action   = sort(distinct(concat(local.task_denied_everywhere, local.task_denied[component])))
        Resource = "*"
      }])
    })
  }
}

resource "aws_iam_role_policy" "task" {
  for_each = local.task_components

  name   = "${var.name}-${each.key}-task"
  role   = aws_iam_role.task[each.key].id
  policy = local.task_policies[each.key]
}
