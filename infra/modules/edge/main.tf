# The provider region is the ALB region, so the certificate is regional too.
resource "aws_acm_certificate" "api" {
  domain_name       = var.hostname
  validation_method = "DNS"
  lifecycle { create_before_destroy = true }
}
resource "aws_route53_record" "validation" {
  for_each = toset([var.hostname])
  zone_id  = var.hosted_zone_id
  name     = one(aws_acm_certificate.api.domain_validation_options).resource_record_name
  type     = one(aws_acm_certificate.api.domain_validation_options).resource_record_type
  records  = [one(aws_acm_certificate.api.domain_validation_options).resource_record_value]
  ttl      = 300
}
resource "aws_acm_certificate_validation" "api" {
  certificate_arn         = aws_acm_certificate.api.arn
  validation_record_fqdns = [for record in aws_route53_record.validation : record.fqdn]
}
resource "aws_lb" "api" {
  name                       = "${var.name}-api"
  internal                   = false
  load_balancer_type         = "application"
  ip_address_type            = "ipv4"
  subnets                    = var.public_subnet_ids
  security_groups            = [var.alb_security_group_id]
  enable_deletion_protection = true
  drop_invalid_header_fields = true
  desync_mitigation_mode     = "strictest"
  enable_http2               = true
  idle_timeout               = 60
  # Fail closed if the ALB cannot reach WAF. Raw request access logs stay off (S9).
  enable_waf_fail_open = false
}
resource "aws_lb_target_group" "api" {
  name                 = "${var.name}-api"
  vpc_id               = var.vpc_id
  port                 = var.api_https_port
  protocol             = "HTTPS"
  protocol_version     = "HTTP1"
  target_type          = "ip"
  deregistration_delay = 45
  health_check {
    enabled             = true
    protocol            = "HTTPS"
    port                = "traffic-port"
    path                = "/readyz"
    matcher             = "200"
    interval            = 15
    timeout             = 5
    healthy_threshold   = 2
    unhealthy_threshold = 2
  }
}
resource "aws_lb_listener" "https" {
  load_balancer_arn = aws_lb.api.arn
  port              = 443
  protocol          = "HTTPS"
  ssl_policy        = "ELBSecurityPolicy-TLS13-1-2-2021-06"
  certificate_arn   = aws_acm_certificate_validation.api.certificate_arn
  default_action {
    type             = "forward"
    target_group_arn = aws_lb_target_group.api.arn
  }
  # A new listener cannot accept even its first request without its WAF association.
  depends_on = [aws_wafv2_web_acl_association.api]
}
resource "aws_lb_listener" "redirect" {
  count             = var.http_redirect_enabled ? 1 : 0
  load_balancer_arn = aws_lb.api.arn
  port              = 80
  protocol          = "HTTP"
  default_action {
    type = "redirect"
    redirect {
      protocol    = "HTTPS"
      port        = "443"
      status_code = "HTTP_301"
    }
  }
  depends_on = [aws_wafv2_web_acl_association.api]
}
resource "aws_route53_record" "api" {
  zone_id = var.hosted_zone_id
  name    = var.hostname
  type    = "A"
  alias {
    name                   = aws_lb.api.dns_name
    zone_id                = aws_lb.api.zone_id
    evaluate_target_health = true
  }
  depends_on = [aws_lb_listener.https]
}
