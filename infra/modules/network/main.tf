locals {
  zones      = { for index, az in var.availability_zones : tostring(index) => az }
  nat_zones  = { for index, az in local.zones : index => az if var.nat_per_az || index == "0" }
  task_roles = toset(["api", "worker", "migration", "onboarding"])
}

resource "aws_vpc" "this" {
  cidr_block           = var.vpc_cidr
  enable_dns_support   = true
  enable_dns_hostnames = true
  tags                 = { Name = var.name }
}

# Remove the implicit allow-all egress and same-group ingress from the default SG.
resource "aws_default_security_group" "this" {
  vpc_id = aws_vpc.this.id
  tags   = { Name = "${var.name}-unused-default" }
}

resource "aws_internet_gateway" "this" {
  vpc_id = aws_vpc.this.id
  tags   = { Name = var.name }
}

resource "aws_subnet" "public" {
  for_each                = local.zones
  vpc_id                  = aws_vpc.this.id
  availability_zone       = each.value
  cidr_block              = cidrsubnet(var.vpc_cidr, 3, tonumber(each.key))
  map_public_ip_on_launch = false
  tags                    = { Name = "${var.name}-public-${each.key}", Tier = "public" }
}

resource "aws_subnet" "tasks" {
  for_each                = local.zones
  vpc_id                  = aws_vpc.this.id
  availability_zone       = each.value
  cidr_block              = cidrsubnet(var.vpc_cidr, 3, 2 + tonumber(each.key))
  map_public_ip_on_launch = false
  tags                    = { Name = "${var.name}-tasks-${each.key}", Tier = "private-tasks" }
}

resource "aws_subnet" "database" {
  for_each                = local.zones
  vpc_id                  = aws_vpc.this.id
  availability_zone       = each.value
  cidr_block              = cidrsubnet(var.vpc_cidr, 3, 4 + tonumber(each.key))
  map_public_ip_on_launch = false
  tags                    = { Name = "${var.name}-db-${each.key}", Tier = "isolated-database" }
}

resource "aws_route_table" "public" {
  vpc_id = aws_vpc.this.id
  tags   = { Name = "${var.name}-public" }
}

resource "aws_route" "internet" {
  route_table_id         = aws_route_table.public.id
  destination_cidr_block = "0.0.0.0/0"
  gateway_id             = aws_internet_gateway.this.id
}

resource "aws_route_table_association" "public" {
  for_each       = local.zones
  subnet_id      = aws_subnet.public[each.key].id
  route_table_id = aws_route_table.public.id
}

resource "aws_eip" "nat" {
  for_each = local.nat_zones
  domain   = "vpc"
  tags     = { Name = "${var.name}-nat-${each.key}" }
}

resource "aws_nat_gateway" "this" {
  for_each      = local.nat_zones
  allocation_id = aws_eip.nat[each.key].id
  subnet_id     = aws_subnet.public[each.key].id
  tags          = { Name = "${var.name}-nat-${each.key}" }
  depends_on    = [aws_internet_gateway.this, aws_route.internet, aws_route_table_association.public]
}

resource "aws_route_table" "tasks" {
  for_each = local.zones
  vpc_id   = aws_vpc.this.id
  tags     = { Name = "${var.name}-tasks-${each.key}" }
}

resource "aws_route" "nat" {
  for_each               = local.zones
  route_table_id         = aws_route_table.tasks[each.key].id
  destination_cidr_block = "0.0.0.0/0"
  nat_gateway_id         = aws_nat_gateway.this[var.nat_per_az ? each.key : "0"].id
}

resource "aws_route_table_association" "tasks" {
  for_each       = local.zones
  subnet_id      = aws_subnet.tasks[each.key].id
  route_table_id = aws_route_table.tasks[each.key].id
}

# No default route, NAT, internet gateway, or endpoints in database route tables.
resource "aws_route_table" "database" {
  for_each = local.zones
  vpc_id   = aws_vpc.this.id
  tags     = { Name = "${var.name}-db-${each.key}" }
}

resource "aws_route_table_association" "database" {
  for_each       = local.zones
  subnet_id      = aws_subnet.database[each.key].id
  route_table_id = aws_route_table.database[each.key].id
}

resource "aws_db_subnet_group" "this" {
  name       = "${var.name}-db"
  subnet_ids = [for subnet in aws_subnet.database : subnet.id]
  tags       = { Name = "${var.name}-db" }
}
