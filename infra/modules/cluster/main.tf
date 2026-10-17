# ---------------------------------------------------------------------------
# S8 foundation compute: one image repository and one Fargate cluster
# ---------------------------------------------------------------------------
# The release root owns task definitions and services; this module owns only the
# long-lived containers they run in, so a routine deploy cannot change either.

locals {
  # ECR cannot see task definitions, and every lifecycle rule's only action is
  # expire, so any rule that selects tagged images has a threshold past which it
  # deletes one — possibly the running or rollback image. No rule here selects a
  # tagged image: every tagged image (candidate or released) is deployment-
  # eligible and is kept until reference-aware cleanup, which checks running and
  # designated rollback task definitions first, exists. With immutable tags an
  # image only ever loses its tags by deliberate deletion. The build must push a
  # single-platform manifest (no index or attestation children), so no deployable
  # image is ever untagged.
  lifecycle_policy = jsonencode({
    rules = [
      {
        rulePriority = 1
        description  = "Expire untagged images from interrupted pushes"
        selection = {
          tagStatus   = "untagged"
          countType   = "sinceImagePushed"
          countUnit   = "days"
          countNumber = var.untagged_expiry_days
        }
        action = { type = "expire" }
      },
    ]
  })
}

resource "aws_ecr_repository" "app" {
  name = var.name

  # A pushed tag can never be moved to different bytes, so `sha-<commit>` and
  # `release-<commit>` always name the image that was reviewed.
  image_tag_mutability = "IMMUTABLE"
  force_delete         = false

  image_scanning_configuration {
    scan_on_push = true
  }

  encryption_configuration {
    encryption_type = "AES256"
  }
}

resource "aws_ecr_lifecycle_policy" "app" {
  repository = aws_ecr_repository.app.name
  policy     = local.lifecycle_policy

  lifecycle {
    # Any rule over tagged images would have an eviction threshold. Adding one
    # requires replacing this guard with reference-aware cleanup.
    precondition {
      condition     = alltrue([for rule in jsondecode(local.lifecycle_policy).rules : rule.selection.tagStatus == "untagged"])
      error_message = "The ECR lifecycle policy may only expire untagged images."
    }
  }
}

resource "aws_ecs_cluster" "this" {
  name = var.name

  setting {
    name  = "containerInsights"
    value = var.container_insights
  }
}

# Fargate only: no EC2 container instances to patch or to share a host with.
resource "aws_ecs_cluster_capacity_providers" "this" {
  cluster_name       = aws_ecs_cluster.this.name
  capacity_providers = ["FARGATE"]

  default_capacity_provider_strategy {
    capacity_provider = "FARGATE"
    weight            = 1
  }
}
