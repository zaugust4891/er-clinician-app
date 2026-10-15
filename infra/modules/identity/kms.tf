# ---------------------------------------------------------------------------
# Key-policy statements for the S5 data key
# ---------------------------------------------------------------------------
# The data module appends these statements to its key policy. Account-principal
# grants delegate to IAM, with each role named in aws:PrincipalArn. A direct role
# principal would independently allow service use and bypass the exact encryption
# contexts in IAM whenever those IAM conditions do not match.
#
# Only role ARNs, services and the account are referenced here. Exact queue and
# secret contexts stay in IAM, avoiding a cycle with resources encrypted by the
# key. The data module retains its existing account administration/delegation
# statement; these statements do not replace that ownership policy.

locals {
  key_policy_grants = {
    AudioUploadsViaS3 = { principals = [aws_iam_role.task["api"].arn], actions = ["kms:GenerateDataKey", "kms:Decrypt"], service = "s3" }
    AudioReadsViaS3   = { principals = [aws_iam_role.task["worker"].arn], actions = ["kms:Decrypt"], service = "s3" }
    JobSendsViaSqs    = { principals = [aws_iam_role.task["api"].arn], actions = ["kms:Decrypt", "kms:GenerateDataKey"], service = "sqs" }
    JobReceivesViaSqs = { principals = [aws_iam_role.task["worker"].arn], actions = ["kms:Decrypt"], service = "sqs" }
    SecretInjection = {
      principals = sort([for component, keys in local.execution_secrets : aws_iam_role.execution[component].arn if length(keys) > 0])
      actions    = ["kms:Decrypt"]
      service    = "secretsmanager"
    }
  }

  kms_key_policy_statements = [
    for sid, grant in local.key_policy_grants : {
      Sid       = sid
      Effect    = "Allow"
      Principal = { AWS = "arn:${local.partition}:iam::${var.aws_account_id}:root" }
      Action    = grant.actions
      Resource  = "*"
      Condition = {
        StringEquals = {
          "aws:PrincipalArn"  = grant.principals
          "kms:CallerAccount" = var.aws_account_id
          "kms:ViaService"    = "${grant.service}.${var.aws_region}.amazonaws.com"
        }
      }
    }
  ]
}
