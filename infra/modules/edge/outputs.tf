output "api_target_group_arn" {
  value = aws_lb_target_group.api.arn
  # ECS requires the target group to be associated with a listener first.
  depends_on = [aws_lb_listener.https]
}
output "api_base_url" { value = "https://${var.hostname}" }
output "alb_arn" { value = aws_lb.api.arn }
output "alb_arn_suffix" { value = aws_lb.api.arn_suffix }
output "target_group_arn_suffix" { value = aws_lb_target_group.api.arn_suffix }
output "waf_acl_arn" { value = aws_wafv2_web_acl.api.arn }
output "certificate_arn" { value = aws_acm_certificate.api.arn }
output "ingress_enabled" { value = var.ingress_enabled }
