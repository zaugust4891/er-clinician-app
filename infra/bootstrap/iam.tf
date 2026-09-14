# ---------------------------------------------------------------------------
# One managed policy and one assumable role per Terraform root
# ---------------------------------------------------------------------------
# The point of this file is a single sentence: a credential that can write staging
# state must not be able to read or write production state. That is enforced three
# ways, and the third one is the one that actually holds.
#
#  1. Each policy only *allows* its own state object.
#  2. Each policy only allows its own lock object, granted separately from the state
#     object, so releasing a stuck lock never requires permission to delete state.
#  3. Each policy explicitly *denies* every other root's objects. An explicit Deny in
#     IAM beats any Allow, anywhere, so this survives a future policy being attached
#     to the same role by mistake.

data "aws_iam_policy_document" "state_scope" {
  for_each = local.state_scopes

  # The S3 backend lists the bucket during `init` (it checks for workspaces). Listing
  # reveals key names only, never object contents.
  statement {
    sid       = "ListStateBucket"
    actions   = ["s3:ListBucket"]
    resources = [aws_s3_bucket.state.arn]
  }

  statement {
    sid       = "ReadWriteOwnState"
    actions   = ["s3:GetObject", "s3:GetObjectVersion", "s3:PutObject"]
    resources = ["${aws_s3_bucket.state.arn}/${each.value.prefix}/terraform.tfstate"]
  }

  # Terraform 1.10+ takes its lock by writing `<key>.tflock` and releases it by
  # deleting that object. Deleting the state object itself is never part of normal
  # operation, so no policy here grants it.
  statement {
    sid       = "ManageOwnStateLock"
    actions   = ["s3:GetObject", "s3:PutObject", "s3:DeleteObject"]
    resources = ["${aws_s3_bucket.state.arn}/${each.value.prefix}/terraform.tfstate.tflock"]
  }

  statement {
    sid = "UseStateEncryptionKey"
    actions = [
      "kms:Encrypt",
      "kms:Decrypt",
      "kms:GenerateDataKey",
      "kms:DescribeKey",
    ]
    resources = [aws_kms_key.state.arn]

    # The key may only be used through S3 in this region. A stolen role cannot use it
    # to decrypt something that was handed to it out of band.
    condition {
      test     = "StringEquals"
      variable = "kms:ViaService"
      values   = ["s3.${var.aws_region}.amazonaws.com"]
    }
  }

  statement {
    sid    = "DenyEveryOtherRootsState"
    effect = "Deny"
    actions = [
      "s3:GetObject",
      "s3:GetObjectVersion",
      "s3:PutObject",
      "s3:DeleteObject",
      "s3:DeleteObjectVersion",
    ]
    resources = [
      for name, scope in local.state_scopes :
      "${aws_s3_bucket.state.arn}/${scope.prefix}/*" if name != each.key
    ]
  }

  # Foundation roots publish their environment's manifest; release roots only read it.
  # Neither can touch the other environment's manifest.
  dynamic "statement" {
    for_each = each.value.environment == null ? [] : [each.value]

    content {
      sid       = "ListOwnEnvironmentManifest"
      actions   = ["s3:ListBucket"]
      resources = [aws_s3_bucket.deploy.arn]

      condition {
        test     = "StringLike"
        variable = "s3:prefix"
        values   = ["envs/${statement.value.environment}/*"]
      }
    }
  }

  dynamic "statement" {
    for_each = each.value.environment == null ? [] : [each.value]

    content {
      sid = "ReadOwnEnvironmentManifest"
      actions = [
        "s3:GetObject",
        "s3:GetObjectVersion",
      ]
      resources = ["${aws_s3_bucket.deploy.arn}/${local.manifest_keys[statement.value.environment]}"]
    }
  }

  dynamic "statement" {
    for_each = each.value.manifest == "write" ? [each.value] : []

    content {
      sid       = "PublishOwnEnvironmentManifest"
      actions   = ["s3:PutObject"]
      resources = ["${aws_s3_bucket.deploy.arn}/${local.manifest_keys[statement.value.environment]}"]
    }
  }

  dynamic "statement" {
    for_each = each.value.environment == null ? [] : [each.value]

    content {
      sid    = "DenyEveryOtherEnvironmentManifest"
      effect = "Deny"
      actions = [
        "s3:GetObject",
        "s3:GetObjectVersion",
        "s3:PutObject",
        "s3:DeleteObject",
        "s3:DeleteObjectVersion",
      ]
      resources = [
        for env, key in local.manifest_keys :
        "${aws_s3_bucket.deploy.arn}/${key}" if env != statement.value.environment
      ]
    }
  }
}

resource "aws_iam_policy" "state_scope" {
  for_each = local.state_scopes

  name        = "${var.project}-tfstate-${each.key}"
  description = "Terraform state access for ${each.key}: ${each.value.description}"
  policy      = data.aws_iam_policy_document.state_scope[each.key].json
}

# ---------------------------------------------------------------------------
# Roles the policies hang on
# ---------------------------------------------------------------------------
# These are state-access roles for operators and, later, for the deployment pipeline.
# They are not the application's ECS task or execution roles; those belong to Step 8
# S6 and must never carry state access.

data "aws_iam_policy_document" "state_operator_trust" {
  for_each = local.state_scopes

  statement {
    sid     = "AllowApprovedOperators"
    actions = ["sts:AssumeRole"]

    principals {
      type = "AWS"
      # With no explicit list, trust is delegated to the account, which means an
      # Identity Center administrator can assume the role and a narrower principal
      # cannot unless its own policy allows sts:AssumeRole. Naming exact permission-set
      # role ARNs in `state_operator_principals` is strictly tighter; do that once the
      # SSO role ARNs are stable.
      identifiers = length(var.state_operator_principals) > 0 ? var.state_operator_principals : ["arn:${data.aws_partition.current.partition}:iam::${var.aws_account_id}:root"]
    }
  }
}

resource "aws_iam_role" "state_operator" {
  for_each = local.state_scopes

  name                 = "${var.project}-tfstate-${each.key}-operator"
  description          = "Assume to run Terraform for ${each.key}."
  assume_role_policy   = data.aws_iam_policy_document.state_operator_trust[each.key].json
  max_session_duration = 3600
}

resource "aws_iam_role_policy_attachment" "state_operator" {
  for_each = local.state_scopes

  role       = aws_iam_role.state_operator[each.key].name
  policy_arn = aws_iam_policy.state_scope[each.key].arn
}
