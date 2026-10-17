output "task_templates" {
  description = "RegisterTaskDefinition payloads, including the migration prerequisite; secret ARNs only."
  value       = { for p, task in module.task : p => task.definition }
}
output "services" { value = { for p, service in aws_ecs_service.app : p => service.name } }
output "task_definition_arns" { value = { for p, task in aws_ecs_task_definition.service : p => task.arn } }
output "migration_network_configuration" {
  value = { awsvpcConfiguration = { subnets = local.private_subnets, securityGroups = [local.resources.migration_security_group_id], assignPublicIp = "DISABLED" } }
}
output "connection_budget" {
  value = { steady_state = 15, rolling_api_maximum = 20, worker_maximum = 5, operator_reserve = 10, required = 35, available = var.db_connection_budget }
}
