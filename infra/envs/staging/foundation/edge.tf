# DNS ownership and certificate validation are explicit live prerequisites.
module "edge" {
  source                = "../../../modules/edge"
  count                 = local.edge_enabled ? 1 : 0
  name                  = "${var.project}-${local.environment}"
  vpc_id                = module.network[0].vpc_id
  public_subnet_ids     = module.network[0].public_subnet_ids
  alb_security_group_id = module.network[0].security_group_ids.alb
  api_https_port        = module.network[0].api_https_port
  hostname              = var.edge.hostname
  hosted_zone_id        = var.edge.hosted_zone_id
  ingress_enabled       = var.edge.ingress_enabled
  http_redirect_enabled = var.network.http_redirect_enabled
  rate_limit            = var.edge.rate_limit
}
locals {
  edge_enabled = var.edge != null
}
resource "terraform_data" "ingress_guard" {
  lifecycle {
    precondition {
      condition     = length(var.network.client_ipv4_cidrs) == 0 || (local.edge_enabled && try(var.edge.ingress_enabled, false))
      error_message = "Keep client CIDRs empty until the WAF-protected edge is explicitly enabled for synthetic validation."
    }
    precondition {
      condition     = !var.network.http_redirect_enabled || local.edge_enabled
      error_message = "Port 80 may open only with the edge's HTTPS redirect listener."
    }
    precondition {
      condition     = !try(var.edge.ingress_enabled, false) || (local.edge_enabled && length(var.network.client_ipv4_cidrs) > 0)
      error_message = "Explicit ingress needs a reviewed client CIDR set; no runner reachability fallback is automatic."
    }
  }
}
