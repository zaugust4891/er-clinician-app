# CI may read/write only deployment records. The smoke role can read exactly one
# operator-approved synthetic WAV, never the application's audio bucket.
resource "aws_iam_role_policy" "ci_artifacts" {
  for_each = var.ci == null ? toset([]) : var.ci.deploy_artifacts_bucket == null ? toset([]) : local.ci_components
  role     = aws_iam_role.ci[each.key].id
  name     = "${var.name}-deployment-records"
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = concat([
      {
        Sid      = "DeploymentObjects", Effect = "Allow"
        Action   = each.key == "smoke" ? ["s3:GetObject"] : ["s3:GetObject", "s3:GetObjectVersion", "s3:PutObject"]
        Resource = [for key in(each.key == "smoke" ? ["fixtures/smoke.wav"] : each.key == "release" ? ["accepted-release.json", "releases/*"] : ["foundation-plans/*"]) : "arn:aws:s3:::${var.ci.deploy_artifacts_bucket}/envs/${var.ci.environment}/${key}"]
      },
      {
        Sid       = "DeploymentEncryption", Effect = "Allow"
        Action    = each.key == "smoke" ? ["kms:Decrypt"] : ["kms:Decrypt", "kms:GenerateDataKey"]
        Resource  = var.ci.deploy_kms_key_arn
        Condition = { StringEquals = { "kms:ViaService" = "s3.${var.aws_region}.amazonaws.com" } }
      }
      ], each.key == "release" ? [{
        Sid       = "DiscoverReleaseRecords", Effect = "Allow", Action = "s3:ListBucket"
        Resource  = "arn:aws:s3:::${var.ci.deploy_artifacts_bucket}"
        Condition = { StringLike = { "s3:prefix" = ["envs/${var.ci.environment}/accepted-release.json", "envs/${var.ci.environment}/releases/*"] } }
    }] : [])
  })
}
