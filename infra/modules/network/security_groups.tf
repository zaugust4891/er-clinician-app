resource "aws_security_group" "alb" {
  name        = "${var.name}-alb"
  description = "Client HTTPS to the load balancer; ingress closed by default"
  vpc_id      = aws_vpc.this.id
}

resource "aws_security_group" "tasks" {
  for_each    = local.task_roles
  name        = "${var.name}-${each.key}"
  description = "Private ${each.key} tasks; HTTPS egress and verified PostgreSQL TLS"
  vpc_id      = aws_vpc.this.id
}

resource "aws_security_group" "database" {
  name        = "${var.name}-db"
  description = "PostgreSQL only from approved task security groups; no outbound initiation"
  vpc_id      = aws_vpc.this.id
}

resource "aws_vpc_security_group_ingress_rule" "client_https" {
  for_each          = var.client_ipv4_cidrs
  security_group_id = aws_security_group.alb.id
  cidr_ipv4         = each.value
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
}

resource "aws_vpc_security_group_ingress_rule" "client_redirect" {
  for_each          = var.http_redirect_enabled ? var.client_ipv4_cidrs : toset([])
  security_group_id = aws_security_group.alb.id
  cidr_ipv4         = each.value
  ip_protocol       = "tcp"
  from_port         = 80
  to_port           = 80
}

resource "aws_vpc_security_group_egress_rule" "alb_api" {
  security_group_id            = aws_security_group.alb.id
  referenced_security_group_id = aws_security_group.tasks["api"].id
  ip_protocol                  = "tcp"
  from_port                    = var.api_https_port
  to_port                      = var.api_https_port
}

resource "aws_vpc_security_group_ingress_rule" "api_alb" {
  security_group_id            = aws_security_group.tasks["api"].id
  referenced_security_group_id = aws_security_group.alb.id
  ip_protocol                  = "tcp"
  from_port                    = var.api_https_port
  to_port                      = var.api_https_port
}

# SGs cannot filter DNS names. Task-role IAM, scoped endpoint policies and verified
# HTTPS clients restrict service use. AmazonProvidedDNS needs no port-53 SG rule.
resource "aws_vpc_security_group_egress_rule" "task_https" {
  for_each          = local.task_roles
  security_group_id = aws_security_group.tasks[each.key].id
  cidr_ipv4         = "0.0.0.0/0"
  ip_protocol       = "tcp"
  from_port         = 443
  to_port           = 443
}

resource "aws_vpc_security_group_egress_rule" "task_database" {
  for_each                     = local.task_roles
  security_group_id            = aws_security_group.tasks[each.key].id
  referenced_security_group_id = aws_security_group.database.id
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
}

resource "aws_vpc_security_group_ingress_rule" "database_task" {
  for_each                     = local.task_roles
  security_group_id            = aws_security_group.database.id
  referenced_security_group_id = aws_security_group.tasks[each.key].id
  ip_protocol                  = "tcp"
  from_port                    = 5432
  to_port                      = 5432
}
