locals {
  endpoint_actions = {
    sqs             = ["sqs:GetQueueUrl", "sqs:GetQueueAttributes", "sqs:SendMessage", "sqs:ReceiveMessage", "sqs:DeleteMessage", "sqs:ChangeMessageVisibility"]
    bedrock-runtime = ["bedrock:InvokeModel"]
    secretsmanager  = ["secretsmanager:GetSecretValue"]
    "ecr.api"       = ["ecr:BatchCheckLayerAvailability", "ecr:GetDownloadUrlForLayer", "ecr:BatchGetImage"]
    "ecr.dkr"       = ["ecr:BatchCheckLayerAvailability", "ecr:GetDownloadUrlForLayer", "ecr:BatchGetImage"]
    logs            = ["logs:CreateLogStream", "logs:PutLogEvents"]
  }
  endpoint_sources = {
    sqs             = toset(["api", "worker"])
    bedrock-runtime = toset(["worker"])
    secretsmanager  = local.task_roles
    "ecr.api"       = local.task_roles
    "ecr.dkr"       = local.task_roles
    logs            = local.task_roles
  }
  endpoint_connections = merge({}, [for service in keys(var.interface_endpoints) : {
    for role in lookup(local.endpoint_sources, service, toset([])) : "${service}/${role}" => { service = service, role = role }
  }]...)
}

# Gateway policy controls only traffic through this endpoint. It does not add an
# aws:SourceVpce deny to any bucket; signed uploads from external clients still work.
resource "aws_vpc_endpoint" "s3" {
  vpc_id            = aws_vpc.this.id
  service_name      = "com.amazonaws.${var.aws_region}.s3"
  vpc_endpoint_type = "Gateway"
  route_table_ids   = [for table in aws_route_table.tasks : table.id]
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = concat([
      {
        Sid       = "EcrImageLayers"
        Effect    = "Allow"
        Principal = "*"
        Action    = ["s3:GetObject"]
        Resource  = ["arn:aws:s3:::prod-${var.aws_region}-starport-layer-bucket/*"]
      }
      ], length(var.s3_bucket_arns) == 0 ? [] : [
      {
        Sid       = "ApplicationObjects"
        Effect    = "Allow"
        Principal = "*"
        Action    = ["s3:GetObject", "s3:PutObject"]
        Resource  = [for arn in sort(tolist(var.s3_bucket_arns)) : "${arn}/*"]
      },
      {
        Sid       = "ApplicationBuckets"
        Effect    = "Allow"
        Principal = "*"
        Action    = ["s3:GetBucketLocation", "s3:ListBucket"]
        Resource  = sort(tolist(var.s3_bucket_arns))
      }
    ])
  })
  tags = { Name = "${var.name}-s3" }
}

resource "aws_security_group" "endpoints" {
  for_each    = var.interface_endpoints
  name        = "${var.name}-vpce-${replace(each.key, ".", "-")}"
  description = "HTTPS to ${each.key} from its required task paths only"
  vpc_id      = aws_vpc.this.id
}

resource "aws_vpc_security_group_ingress_rule" "endpoint_task" {
  for_each                     = local.endpoint_connections
  security_group_id            = aws_security_group.endpoints[each.value.service].id
  referenced_security_group_id = aws_security_group.tasks[each.value.role].id
  ip_protocol                  = "tcp"
  from_port                    = 443
  to_port                      = 443
}

resource "aws_vpc_endpoint" "interface" {
  for_each            = var.interface_endpoints
  vpc_id              = aws_vpc.this.id
  service_name        = "com.amazonaws.${var.aws_region}.${each.key}"
  vpc_endpoint_type   = "Interface"
  private_dns_enabled = true
  subnet_ids          = [for subnet in aws_subnet.tasks : subnet.id]
  security_group_ids  = [aws_security_group.endpoints[each.key].id]
  policy = jsonencode({
    Version = "2012-10-17"
    Statement = concat([
      {
        Sid       = "ScopedServiceAccess"
        Effect    = "Allow"
        Principal = "*"
        Action    = lookup(local.endpoint_actions, each.key, [])
        Resource  = sort(tolist(each.value.resource_arns))
      }
      ], contains(["ecr.api", "ecr.dkr"], each.key) ? [
      {
        Sid       = "EcrAuthorization"
        Effect    = "Allow"
        Principal = "*"
        Action    = ["ecr:GetAuthorizationToken"]
        Resource  = ["*"] # This action does not support resource-level permissions.
      }
    ] : [])
  })
  tags = { Name = "${var.name}-${each.key}" }
}
