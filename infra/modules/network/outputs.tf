output "availability_zones" {
  description = "Resolved ordered AZ pair; pin it after the first reviewed plan."
  value       = var.availability_zones
}

output "vpc_id" {
  value = aws_vpc.this.id
}

output "public_subnet_ids" {
  value = [for subnet in aws_subnet.public : subnet.id]
}

output "private_subnet_ids" {
  description = "Private task subnets, ordered by input AZ. ECS must also set assign_public_ip = false."
  value       = [for subnet in aws_subnet.tasks : subnet.id]
}

output "database_subnet_ids" {
  value = [for subnet in aws_subnet.database : subnet.id]
}

output "db_subnet_group_name" {
  description = "Two-AZ placement only; S5 must set RDS publicly_accessible=false and choose Multi-AZ separately."
  value       = aws_db_subnet_group.this.name
}

output "security_group_ids" {
  value = merge({ alb = aws_security_group.alb.id, database = aws_security_group.database.id }, {
    for role, group in aws_security_group.tasks : role => group.id
  })
}

output "api_https_port" {
  value = var.api_https_port
}

output "s3_endpoint_id" {
  value = aws_vpc_endpoint.s3.id
}

output "s3_gateway_bucket_arns" {
  description = "Application buckets the gateway policy grants object access to. S5 supplies the audio bucket; endpoint permission is a boundary, not an IAM grant."
  value       = var.s3_bucket_arns
}

output "interface_endpoint_ids" {
  value = { for service, endpoint in aws_vpc_endpoint.interface : service => endpoint.id }
}

output "task_route_table_ids" {
  value = [for table in aws_route_table.tasks : table.id]
}

output "database_route_table_ids" {
  value = [for table in aws_route_table.database : table.id]
}

output "nat_gateway_ids" {
  value = [for nat in aws_nat_gateway.this : nat.id]
}
