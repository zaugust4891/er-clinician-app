variable "family" { type = string }
variable "component" {
  type = string
  validation {
    condition     = contains(["api", "worker", "migration", "db-bootstrap", "onboarding"], var.component)
    error_message = "Unknown task process."
  }
}
variable "image" {
  type = string
  validation {
    condition     = can(regex("^[0-9]{12}\\.dkr\\.ecr\\.[a-z0-9-]+\\.amazonaws\\.com/[a-z0-9-]+@sha256:[0-9a-f]{64}$", var.image))
    error_message = "Use an ECR image pinned to a digest, never a tag."
  }
}
variable "aws_region" { type = string }
variable "execution_role_arn" { type = string }
variable "task_role_arn" {
  type    = string
  default = null
}
variable "environment" { type = map(string) }
variable "secrets" { type = map(string) }
variable "log_group" { type = string }
variable "api_https_port" {
  type    = number
  default = 8443
}
locals {
  binaries = {
    api          = "er-server"
    worker       = "er-worker"
    migration    = "er-migrate"
    db-bootstrap = "er-db-bootstrap"
    onboarding   = "er-onboard"
  }
  container = {
    name                   = var.component
    image                  = var.image
    essential              = true
    entryPoint             = ["/usr/local/bin/${local.binaries[var.component]}"]
    user                   = "65532:65532"
    privileged             = false
    readonlyRootFilesystem = true
    linuxParameters        = { capabilities = { drop = ["ALL"] } }
    # In-memory processing only: no volumes, writable mount or shell health check.
    mountPoints  = []
    volumesFrom  = []
    portMappings = var.component == "api" ? [{ containerPort = var.api_https_port, hostPort = var.api_https_port, protocol = "tcp" }] : []
    stopTimeout  = var.component == "worker" ? 120 : 60
    environment  = [for k, v in var.environment : { name = k, value = v }]
    secrets      = [for k, v in var.secrets : { name = k, valueFrom = v }]
    logConfiguration = {
      logDriver = "awslogs"
      options = {
        awslogs-group         = var.log_group
        awslogs-region        = var.aws_region
        awslogs-stream-prefix = var.component
        mode                  = "non-blocking"
        max-buffer-size       = "4m"
      }
    }
  }
  definition = merge({
    family                  = var.family
    networkMode             = "awsvpc"
    requiresCompatibilities = ["FARGATE"]
    cpu                     = "512"
    memory                  = "1024"
    runtimePlatform         = { cpuArchitecture = "X86_64", operatingSystemFamily = "LINUX" }
    executionRoleArn        = var.execution_role_arn
    containerDefinitions    = [local.container]
    volumes                 = []
  }, var.task_role_arn == null ? {} : { taskRoleArn = var.task_role_arn })
}
output "definition" { value = local.definition }
output "register_payload" { value = jsonencode(local.definition) }
