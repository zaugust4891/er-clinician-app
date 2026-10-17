mock_provider "aws" {
  mock_resource "aws_lb" {
    defaults = { arn = "arn:aws:elasticloadbalancing:us-east-2:000000000000:loadbalancer/app/ernote-staging-api/1234567890abcdef", dns_name = "synthetic.us-east-2.elb.amazonaws.com", zone_id = "ZSYNTHETIC" }
  }
  mock_resource "aws_lb_target_group" {
    defaults = { arn = "arn:aws:elasticloadbalancing:us-east-2:000000000000:targetgroup/ernote-staging-api/1234567890abcdef" }
  }
  mock_resource "aws_acm_certificate" {
    defaults = {
      arn                       = "arn:aws:acm:us-east-2:000000000000:certificate/00000000-0000-0000-0000-000000000000"
      domain_validation_options = [{ domain_name = "api.example.test", resource_record_name = "_synthetic.api.example.test", resource_record_type = "CNAME", resource_record_value = "_synthetic.acm-validations.aws." }]
    }
  }
  mock_resource "aws_acm_certificate_validation" {
    defaults = { certificate_arn = "arn:aws:acm:us-east-2:000000000000:certificate/00000000-0000-0000-0000-000000000000" }
  }
  mock_resource "aws_wafv2_web_acl" {
    defaults = { arn = "arn:aws:wafv2:us-east-2:000000000000:regional/webacl/ernote-staging-api/00000000-0000-0000-0000-000000000000" }
  }
}
variables {
  name                  = "ernote-staging"
  vpc_id                = "vpc-0123456789abcdef0"
  public_subnet_ids     = ["subnet-0123456789abcdef0", "subnet-0123456789abcdef1"]
  alb_security_group_id = "sg-0123456789abcdef0"
  hostname              = "api.example.test"
  hosted_zone_id        = "ZSYNTHETIC"
}
run "closed_edge_is_encrypted_and_waf_attached" {
  command = apply
  assert {
    condition = (
      aws_lb_listener.https.ssl_policy == "ELBSecurityPolicy-TLS13-1-2-2021-06" &&
      aws_lb_target_group.api.protocol == "HTTPS" && aws_lb_target_group.api.target_type == "ip" &&
      aws_lb_target_group.api.port == 8443 && aws_lb_target_group.api.deregistration_delay == "45" &&
      aws_lb_target_group.api.health_check[0].path == "/readyz" && aws_lb_target_group.api.health_check[0].protocol == "HTTPS" &&
      aws_lb_target_group.api.health_check[0].matcher == "200" && !aws_lb.api.enable_waf_fail_open &&
      aws_wafv2_web_acl_association.api.resource_arn == aws_lb.api.arn &&
      length(aws_lb_listener.redirect) == 0 && !output.ingress_enabled
    )
    error_message = "The closed edge needs WAF, TLS on both hops, readiness-only target health, and no HTTP listener by default."
  }
  assert {
    condition = (
      length([for r in aws_wafv2_web_acl.api.rule : r if r.name == "IngressClosed" && r.priority == 0 && try(length(r.action[0].block) == 1, false)]) == 1 &&
      alltrue([for r in aws_wafv2_web_acl.api.rule : !r.visibility_config[0].sampled_requests_enabled]) &&
      !aws_wafv2_web_acl.api.visibility_config[0].sampled_requests_enabled
    )
    error_message = "Default ingress must block all requests without recording sensitive sampled requests."
  }
  assert {
    condition = (
      aws_acm_certificate.api.validation_method == "DNS" &&
      aws_route53_record.validation["api.example.test"].type == "CNAME" &&
      aws_route53_record.api.type == "A" && output.api_base_url == "https://api.example.test"
    )
    error_message = "ACM DNS validation and the HTTPS hostname alias must remain managed."
  }
}
run "explicit_opening_keeps_managed_body_and_rate_protections" {
  command = apply
  variables {
    ingress_enabled       = true
    http_redirect_enabled = true
  }
  assert {
    condition = (
      length(aws_wafv2_web_acl.api.rule) == 4 &&
      aws_lb_listener.redirect[0].default_action[0].type == "redirect" &&
      aws_lb_listener.redirect[0].default_action[0].redirect[0].protocol == "HTTPS" &&
      one([for r in aws_wafv2_web_acl.api.rule : r if r.name == "PerIpRate"]).statement[0].rate_based_statement[0].limit == 2000
    )
    error_message = "Opening retains rate/common/body rules and HTTP may only redirect."
  }
  assert {
    condition = (
      one(one([for r in aws_wafv2_web_acl.api.rule : r if r.name == "CommonProtections"]).statement[0].managed_rule_group_statement[0].rule_action_override).name == "SizeRestrictions_BODY" &&
      length(one([for r in aws_wafv2_web_acl.api.rule : r if r.name == "CommonProtections"]).statement[0].managed_rule_group_statement[0].rule_action_override) == 1 &&
      length([for r in aws_wafv2_web_acl.api.rule : r if r.name == "OversizeExceptSyncBodies" && try(length(r.action[0].block) == 1, false)]) == 1
    )
    error_message = "Only the managed size rule can be replaced; a blocking oversize rule must remain."
  }
}
run "sync_body_content_type_exception_is_narrow_and_pcre_compatible" {
  command = plan
  assert {
    condition = alltrue([for test in [
      { header = "application/json", allowed = true },
      { header = "Application/JSON; charset=UTF-8", allowed = true },
      { header = "application/jsonp", allowed = false },
      { header = "text/plain", allowed = false },
      { header = "application/json; charset=ascii", allowed = false },
      ] : can(regex(
        one([for s in one([for r in aws_wafv2_web_acl.api.rule : r if r.name == "LabelSyncBodies"]).statement[0].and_statement[0].statement : s.regex_match_statement[0].regex_string if length(s.regex_match_statement) > 0]),
        lower(test.header)
    )) == test.allowed])
    error_message = "Only the documented JSON media types receive the sync-body exception."
  }
  # AWS rejects capturing subexpressions even though Terraform's regex validator accepts them.
  assert {
    condition = !can(regex("\\([^?]",
      one([for s in one([for r in aws_wafv2_web_acl.api.rule : r if r.name == "LabelSyncBodies"]).statement[0].and_statement[0].statement : s.regex_match_statement[0].regex_string if length(s.regex_match_statement) > 0])
    ))
    error_message = "Use noncapturing groups for AWS WAF's restricted PCRE syntax."
  }
}
run "reject_wildcard_hostname" {
  command = plan
  variables { hostname = "*.example.test" }
  expect_failures = [var.hostname]
}
run "reject_single_subnet" {
  command = plan
  variables { public_subnet_ids = ["subnet-0123456789abcdef0"] }
  expect_failures = [var.public_subnet_ids]
}
run "reject_invalid_rate" {
  command = plan
  variables { rate_limit = 0 }
  expect_failures = [var.rate_limit]
}
