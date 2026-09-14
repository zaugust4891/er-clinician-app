# Mocked apply evaluates resource references and policies, without AWS credentials.
mock_provider "aws" {}

variables {
  name               = "ernote-synthetic-test"
  aws_region         = "us-east-2"
  availability_zones = ["us-east-2a", "us-east-2b"]
}

run "closed_staging" {
  command = apply

  assert {
    condition     = aws_vpc.this.enable_dns_support && aws_vpc.this.enable_dns_hostnames
    error_message = "VPC resolver and private DNS must be available before task startup."
  }
  assert {
    condition = length(aws_subnet.public) == 2 && length(aws_subnet.tasks) == 2 && length(aws_subnet.database) == 2 && length(toset(concat(
      [for s in aws_subnet.public : s.cidr_block],
      [for s in aws_subnet.tasks : s.cidr_block],
      [for s in aws_subnet.database : s.cidr_block]
    ))) == 6 && alltrue([for s in concat(values(aws_subnet.public), values(aws_subnet.tasks), values(aws_subnet.database)) : !s.map_public_ip_on_launch])
    error_message = "Six disjoint subnets with no automatic public IP assignment are required."
  }
  assert {
    condition     = length(aws_nat_gateway.this) == 1 && alltrue([for r in aws_route.nat : r.nat_gateway_id == aws_nat_gateway.this["0"].id && r.destination_cidr_block == "0.0.0.0/0"])
    error_message = "Both private task routes must use the one staging NAT."
  }
  assert {
    condition     = aws_nat_gateway.this["0"].subnet_id == aws_subnet.public["0"].id && aws_route.internet.gateway_id == aws_internet_gateway.this.id
    error_message = "NAT must be in the first public AZ with an internet gateway route."
  }
  assert {
    condition = alltrue([for key, assoc in aws_route_table_association.database :
      assoc.route_table_id == aws_route_table.database[key].id && assoc.subnet_id == aws_subnet.database[key].id
      ]) && alltrue([for table in aws_route_table.database : length(table.route) == 0]) && (
      toset(aws_db_subnet_group.this.subnet_ids) == toset([for s in aws_subnet.database : s.id])
    )
    error_message = "The two-AZ DB subnet group must use isolated route tables, never the task NAT route tables."
  }
  assert {
    condition     = length(aws_vpc_security_group_ingress_rule.client_https) == 0 && length(aws_vpc_security_group_ingress_rule.client_redirect) == 0 && length(aws_default_security_group.this.ingress) == 0 && length(aws_default_security_group.this.egress) == 0
    error_message = "Application ingress and the unused default SG must start closed."
  }
  assert {
    condition = aws_vpc_security_group_ingress_rule.api_alb.referenced_security_group_id == aws_security_group.alb.id && aws_vpc_security_group_ingress_rule.api_alb.security_group_id == aws_security_group.tasks["api"].id && aws_vpc_security_group_ingress_rule.api_alb.from_port == 8443 && aws_vpc_security_group_ingress_rule.api_alb.to_port == 8443 && (
      aws_vpc_security_group_egress_rule.alb_api.referenced_security_group_id == aws_security_group.tasks["api"].id
    )
    error_message = "Only the ALB may reach the API TLS port."
  }
  assert {
    condition = length(aws_vpc_security_group_ingress_rule.database_task) == 4 && alltrue([
      for role, rule in aws_vpc_security_group_ingress_rule.database_task :
      rule.security_group_id == aws_security_group.database.id && rule.referenced_security_group_id == aws_security_group.tasks[role].id && rule.from_port == 5432 && rule.to_port == 5432 && rule.ip_protocol == "tcp"
      ]) && alltrue([for role, rule in aws_vpc_security_group_egress_rule.task_database :
      rule.security_group_id == aws_security_group.tasks[role].id && rule.referenced_security_group_id == aws_security_group.database.id && rule.from_port == 5432 && rule.to_port == 5432
    ])
    error_message = "Database access must use bidirectional 5432 rules referencing the four distinct approved task SGs."
  }
  assert {
    condition = alltrue([for rule in aws_vpc_security_group_egress_rule.task_https : rule.cidr_ipv4 == "0.0.0.0/0" && rule.from_port == 443 && rule.to_port == 443 && rule.ip_protocol == "tcp"]) && (
      length(aws_vpc_endpoint.interface) == 0 && length(aws_security_group.endpoints) == 0
    )
    error_message = "Default service egress must use HTTPS without purchasing interface endpoints."
  }
  assert {
    condition = aws_vpc_endpoint.s3.vpc_endpoint_type == "Gateway" && toset(aws_vpc_endpoint.s3.route_table_ids) == toset([for t in aws_route_table.tasks : t.id]) && (
      jsondecode(aws_vpc_endpoint.s3.policy).Statement[0].Resource[0] == "arn:aws:s3:::prod-us-east-2-starport-layer-bucket/*" && jsondecode(aws_vpc_endpoint.s3.policy).Statement[0].Action[0] == "s3:GetObject"
    )
    error_message = "S3 gateway must cover both task route tables and permit ECR layer downloads."
  }
}

run "selected_endpoints" {
  command = apply
  variables {
    s3_bucket_arns = ["arn:aws:s3:::ernote-synthetic-audio"]
    interface_endpoints = {
      sqs = { resource_arns = ["arn:aws:sqs:us-east-2:000000000000:synthetic-jobs"] }
      bedrock-runtime = { resource_arns = [
        "arn:aws:bedrock:us-east-2:000000000000:inference-profile/us.anthropic.claude-sonnet-4-6",
        "arn:aws:bedrock:us-east-2::foundation-model/anthropic.claude-sonnet-4-6",
        "arn:aws:bedrock:us-east-1::foundation-model/anthropic.claude-sonnet-4-6",
        "arn:aws:bedrock:us-west-2::foundation-model/anthropic.claude-sonnet-4-6"
      ] }
      secretsmanager = { resource_arns = ["arn:aws:secretsmanager:us-east-2:000000000000:secret:synthetic-db-abcdef"] }
      "ecr.api"      = { resource_arns = ["arn:aws:ecr:us-east-2:000000000000:repository/synthetic-app"] }
      "ecr.dkr"      = { resource_arns = ["arn:aws:ecr:us-east-2:000000000000:repository/synthetic-app"] }
      logs           = { resource_arns = ["arn:aws:logs:us-east-2:000000000000:log-group:/ernote/synthetic:*"] }
    }
  }
  assert {
    condition = length(aws_vpc_endpoint.interface) == 6 && length(aws_nat_gateway.this) == 1 && alltrue([
      for service, endpoint in aws_vpc_endpoint.interface : endpoint.private_dns_enabled && endpoint.vpc_endpoint_type == "Interface" &&
      toset(endpoint.subnet_ids) == toset([for s in aws_subnet.tasks : s.id]) &&
      toset(endpoint.security_group_ids) == toset([aws_security_group.endpoints[service].id]) &&
      toset(jsondecode(endpoint.policy).Statement[0].Resource) == var.interface_endpoints[service].resource_arns &&
      !contains(jsondecode(endpoint.policy).Statement[0].Action, "*")
    ])
    error_message = "Each selected endpoint needs private DNS, two AZs, its own SG, scoped resources/actions, and retained NAT."
  }
  assert {
    condition = length(aws_vpc_security_group_ingress_rule.endpoint_task) == 19 && alltrue([
      for key, rule in aws_vpc_security_group_ingress_rule.endpoint_task :
      rule.security_group_id == aws_security_group.endpoints[split("/", key)[0]].id &&
      rule.referenced_security_group_id == aws_security_group.tasks[split("/", key)[1]].id && rule.from_port == 443 && rule.to_port == 443
    ]) && !contains(keys(aws_vpc_security_group_ingress_rule.endpoint_task), "bedrock-runtime/api") && !contains(keys(aws_vpc_security_group_ingress_rule.endpoint_task), "sqs/migration")
    error_message = "Only worker reaches Bedrock, only API/worker reach SQS, and execution services permit all four task paths."
  }
  assert {
    condition = alltrue([for service in ["ecr.api", "ecr.dkr"] :
      jsondecode(aws_vpc_endpoint.interface[service].policy).Statement[1].Action == ["ecr:GetAuthorizationToken"] &&
      jsondecode(aws_vpc_endpoint.interface[service].policy).Statement[1].Resource == ["*"]
    ])
    error_message = "ECR needs its resource-less authorization action in addition to repository-scoped pulls."
  }
  assert {
    condition = jsondecode(aws_vpc_endpoint.s3.policy).Statement[1].Resource[0] == "arn:aws:s3:::ernote-synthetic-audio/*" && (
      !strcontains(aws_vpc_endpoint.s3.policy, "SourceVpce") && !strcontains(aws_vpc_endpoint.s3.policy, "Deny")
    )
    error_message = "Application access must name the actual bucket and must not impose a VPC-only upload contract."
  }
}

run "per_az_nat_and_reviewed_ingress" {
  command = apply
  variables {
    nat_per_az            = true
    client_ipv4_cidrs     = ["192.0.2.0/24"]
    http_redirect_enabled = true
  }
  assert {
    condition = length(aws_nat_gateway.this) == 2 && alltrue([for key, route in aws_route.nat :
      route.nat_gateway_id == aws_nat_gateway.this[key].id && aws_nat_gateway.this[key].subnet_id == aws_subnet.public[key].id
    ])
    error_message = "Per-AZ NAT must route each task subnet through its own AZ."
  }
  assert {
    condition     = length(aws_vpc_security_group_ingress_rule.client_https) == 1 && length(aws_vpc_security_group_ingress_rule.client_redirect) == 1 && aws_vpc_security_group_ingress_rule.client_https["192.0.2.0/24"].from_port == 443 && aws_vpc_security_group_ingress_rule.client_redirect["192.0.2.0/24"].from_port == 80
    error_message = "Only explicitly approved client CIDRs can reach HTTPS and optional redirect."
  }
}

run "reject_duplicate_az" {
  command = plan
  variables { availability_zones = ["us-east-2a", "us-east-2a"] }
  expect_failures = [var.availability_zones]
}
run "reject_wrong_region_az" {
  command = plan
  variables { availability_zones = ["us-east-1a", "us-east-1b"] }
  expect_failures = [var.availability_zones]
}
run "reject_ipv6_vpc" {
  command = plan
  variables { vpc_cidr = "2001:db8::/16" }
  expect_failures = [var.vpc_cidr]
}
run "reject_noncanonical_vpc" {
  command = plan
  variables { vpc_cidr = "10.40.1.1/16" }
  expect_failures = [var.vpc_cidr]
}
run "reject_unpaired_ecr" {
  command = plan
  variables { interface_endpoints = { "ecr.api" = { resource_arns = ["arn:aws:ecr:us-east-2:000000000000:repository/app"] } } }
  expect_failures = [var.interface_endpoints]
}
run "reject_unknown_endpoint" {
  command = plan
  variables { interface_endpoints = { sts = { resource_arns = ["arn:aws:sts:us-east-2:000000000000:example"] } } }
  expect_failures = [var.interface_endpoints]
}
run "reject_endpoint_wildcard" {
  command = plan
  variables { interface_endpoints = { sqs = { resource_arns = ["*"] } } }
  expect_failures = [var.interface_endpoints]
}
run "reject_empty_endpoint_scope" {
  command = plan
  variables { interface_endpoints = { sqs = { resource_arns = [] } } }
  expect_failures = [var.interface_endpoints]
}
run "reject_wrong_service_scope" {
  command = plan
  variables { interface_endpoints = { sqs = { resource_arns = ["arn:aws:ecr:us-east-2:000000000000:repository/app"] } } }
  expect_failures = [var.interface_endpoints]
}
run "reject_wildcard_bucket" {
  command = plan
  variables { s3_bucket_arns = ["arn:aws:s3:::*"] }
  expect_failures = [var.s3_bucket_arns]
}
