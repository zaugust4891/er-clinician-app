# Record the resolved AZ pair after the first plan and pin it in network.availability_zones.
data "aws_availability_zones" "network" {
  count = var.network.availability_zones == null ? 1 : 0
  state = "available"
  filter {
    name   = "zone-type"
    values = ["availability-zone"]
  }
}

module "network" {
  source = "../../../modules/network"
  count  = 1 # Match the indexed interface of the gated production root.

  name                  = "${var.project}-${local.environment}"
  aws_region            = var.aws_region
  availability_zones    = var.network.availability_zones != null ? var.network.availability_zones : slice(sort(data.aws_availability_zones.network[0].names), 0, 2)
  vpc_cidr              = var.network.vpc_cidr
  nat_per_az            = var.network.nat_per_az
  client_ipv4_cidrs     = var.network.client_ipv4_cidrs
  http_redirect_enabled = var.network.http_redirect_enabled
  api_https_port        = var.network.api_https_port
  interface_endpoints   = var.network.interface_endpoints

  # S5's audio bucket is always in scope for the S3 gateway endpoint. The name is a
  # local, not a `module.data` output, so the gateway policy does not make the network
  # depend on the data module that depends on this network.
  s3_bucket_arns = setunion(var.network.s3_bucket_arns, [local.audio_bucket_arn])

  depends_on = [terraform_data.account_guard, terraform_data.ingress_guard]
}
