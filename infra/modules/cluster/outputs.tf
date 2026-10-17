output "repository_name" {
  description = "ECR repository name; equals the S6 execution roles' pull scope."
  value       = aws_ecr_repository.app.name
}

output "repository_arn" {
  description = "ECR repository ARN."
  value       = aws_ecr_repository.app.arn
}

output "repository_url" {
  description = "`<registry>/<name>`; release task definitions append `@sha256:...`."
  value       = aws_ecr_repository.app.repository_url
}

output "lifecycle_policy" {
  description = "The ECR lifecycle policy JSON, exposed so tests can evaluate it against image histories."
  value       = local.lifecycle_policy
}

output "cluster_arn" {
  description = "ECS cluster ARN for release services and one-shot tasks."
  value       = aws_ecs_cluster.this.arn
}

output "cluster_name" {
  description = "ECS cluster name."
  value       = aws_ecs_cluster.this.name
}
