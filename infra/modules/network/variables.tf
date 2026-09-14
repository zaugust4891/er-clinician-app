variable "name" {
  description = "Environment-qualified resource prefix, e.g. ernote-staging."
  type        = string
}

variable "aws_region" {
  description = "Region of the provider and service endpoints."
  type        = string
}

variable "availability_zones" {
  description = "Exactly two distinct standard AZs in aws_region; order determines the single NAT's AZ."
  type        = list(string)
  validation {
    condition = length(var.availability_zones) == 2 && length(toset(var.availability_zones)) == 2 && alltrue([
      for az in var.availability_zones : can(regex("^${var.aws_region}[a-z]$", az))
    ])
    error_message = "Choose two distinct standard availability zones in aws_region."
  }
}

variable "vpc_cidr" {
  description = "Canonical IPv4 /16-/20; divided into eight equal subnets, six used. Check overlap before applying."
  type        = string
  default     = "10.40.0.0/16"
  validation {
    condition = try(
      cidrnetmask(var.vpc_cidr) != "" &&
      tonumber(split("/", var.vpc_cidr)[1]) >= 16 &&
      tonumber(split("/", var.vpc_cidr)[1]) <= 20 &&
      cidrhost(var.vpc_cidr, 0) == split("/", var.vpc_cidr)[0], false
    )
    error_message = "vpc_cidr must be a canonical IPv4 network with prefix /16 through /20."
  }
}

variable "nat_per_az" {
  description = "False: one NAT in the first AZ. True: one per AZ. NAT remains required for Deepgram and Cognito JWKS."
  type        = bool
  default     = false
}

variable "client_ipv4_cidrs" {
  description = "Approved client networks for ALB HTTPS. Empty keeps ingress closed until S8's WAF/TLS gate."
  type        = set(string)
  default     = []
  validation {
    condition     = alltrue([for cidr in var.client_ipv4_cidrs : can(cidrnetmask(cidr))])
    error_message = "Client ingress must use valid IPv4 CIDRs."
  }
}

variable "http_redirect_enabled" {
  description = "Allow port 80 from the same client CIDRs only when S8 provisions a redirect-only listener."
  type        = bool
  default     = false
}

variable "api_https_port" {
  description = "Dedicated TLS listener required from S8; never point this rule at the current development HTTP listener."
  type        = number
  default     = 8443
  validation {
    condition     = var.api_https_port >= 1024 && var.api_https_port <= 65535 && floor(var.api_https_port) == var.api_https_port
    error_message = "api_https_port must be an unprivileged integer TCP port."
  }
}

variable "s3_bucket_arns" {
  description = "Exact application bucket ARNs supplied by S5. Gateway policy permits object Get/Put and bucket List/location, plus ECR layer reads."
  type        = set(string)
  default     = []
  validation {
    condition     = alltrue([for arn in var.s3_bucket_arns : can(regex("^arn:aws:s3:::[a-z0-9][a-z0-9.-]{1,61}[a-z0-9]$", arn))])
    error_message = "Supply exact S3 bucket ARNs, without object paths or wildcards."
  }
}

variable "interface_endpoints" {
  description = <<-EOT
    Opt-in service switches: sqs, bedrock-runtime, secretsmanager, ecr.api, ecr.dkr, logs.
    Each enabled service requires reviewed resource ARNs from S5/S6/S8/S9; actions and
    source SGs are fixed here. Logs ARNs may end in :* to include their streams.
    ECR endpoints must be enabled together. Empty buys no endpoint ENIs.
  EOT
  type        = map(object({ resource_arns = set(string) }))
  default     = {}
  validation {
    condition     = alltrue([for service in keys(var.interface_endpoints) : contains(["sqs", "bedrock-runtime", "secretsmanager", "ecr.api", "ecr.dkr", "logs"], service)])
    error_message = "Unsupported interface endpoint service."
  }
  validation {
    condition = alltrue([for service, config in var.interface_endpoints : length(config.resource_arns) > 0 && alltrue([
      for arn in config.resource_arns : can(regex("^arn:aws:${lookup({ "bedrock-runtime" = "bedrock", "ecr.api" = "ecr", "ecr.dkr" = "ecr" }, service, service)}:[a-z0-9-]+:[0-9]*:.+$", arn)) &&
      !strcontains(arn, "?") && !strcontains(service == "logs" ? trimsuffix(arn, ":*") : arn, "*")
    ])])
    error_message = "Each endpoint needs nonempty, service-matching resource ARNs without wildcards (except a logs ARN's trailing :*)."
  }
  validation {
    condition     = contains(keys(var.interface_endpoints), "ecr.api") == contains(keys(var.interface_endpoints), "ecr.dkr")
    error_message = "ECR image pulls require both ecr.api and ecr.dkr interface endpoints."
  }
}
