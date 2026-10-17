variable "name" { type = string }
variable "vpc_id" { type = string }
variable "public_subnet_ids" {
  type = list(string)
  validation {
    condition     = length(distinct(var.public_subnet_ids)) == 2
    error_message = "The public ALB needs the two distinct foundation AZ subnets."
  }
}
variable "alb_security_group_id" { type = string }
variable "api_https_port" {
  type    = number
  default = 8443
}
variable "hostname" {
  type = string
  validation {
    condition     = can(regex("^[a-z0-9][a-z0-9.-]+\\.[a-z]{2,}$", var.hostname)) && !strcontains(var.hostname, "..")
    error_message = "hostname must be an exact lowercase DNS name, without a wildcard or trailing dot."
  }
}
variable "hosted_zone_id" { type = string }
variable "ingress_enabled" {
  description = "Explicit synthetic-validation gate. False blocks every request at WAF, including when the SG was already open."
  type        = bool
  default     = false
}
variable "http_redirect_enabled" {
  type    = bool
  default = false
}
variable "rate_limit" {
  description = "Requests per IP over 300 seconds; a staging hypothesis, not a clinician quota."
  type        = number
  default     = 2000
  validation {
    condition     = var.rate_limit >= 10 && var.rate_limit <= 2000000000 && floor(var.rate_limit) == var.rate_limit
    error_message = "rate_limit must be an integer in WAF's supported range."
  }
}
