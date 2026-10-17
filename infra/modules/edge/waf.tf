resource "aws_wafv2_web_acl" "api" {
  name  = "${var.name}-api"
  scope = "REGIONAL"
  default_action {
    allow {}
  }
  visibility_config {
    cloudwatch_metrics_enabled = true
    metric_name                = "${var.name}-api"
    sampled_requests_enabled   = false
  }

  dynamic "rule" {
    for_each = var.ingress_enabled ? [] : [1]
    content {
      name     = "IngressClosed"
      priority = 0
      action {
        block {}
      }
      statement {
        size_constraint_statement {
          comparison_operator = "GE"
          size                = 0
          field_to_match {
            uri_path {}
          }
          text_transformation {
            priority = 0
            type     = "NONE"
          }
        }
      }
      visibility_config {
        cloudwatch_metrics_enabled = true
        metric_name                = "IngressClosed"
        sampled_requests_enabled   = false
      }
    }
  }
  # Count adds a label and continues evaluation. It never allows/skips WAF rules.
  rule {
    name     = "LabelSyncBodies"
    priority = 5
    action {
      count {}
    }
    rule_label { name = "sync-body" }
    statement {
      and_statement {
        statement {
          or_statement {
            statement {
              and_statement {
                statement {
                  byte_match_statement {
                    positional_constraint = "EXACTLY"
                    search_string         = "POST"
                    field_to_match {
                      method {}
                    }
                    text_transformation {
                      priority = 0
                      type     = "NONE"
                    }
                  }
                }
                statement {
                  regex_match_statement {
                    regex_string = "^/v1/patients/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}/events$"
                    field_to_match {
                      uri_path {}
                    }
                    text_transformation {
                      priority = 0
                      type     = "NONE"
                    }
                  }
                }
              }
            }
            statement {
              and_statement {
                statement {
                  byte_match_statement {
                    positional_constraint = "EXACTLY"
                    search_string         = "PUT"
                    field_to_match {
                      method {}
                    }
                    text_transformation {
                      priority = 0
                      type     = "NONE"
                    }
                  }
                }
                statement {
                  regex_match_statement {
                    regex_string = "^/v1/patients/[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}/snapshot$"
                    field_to_match {
                      uri_path {}
                    }
                    text_transformation {
                      priority = 0
                      type     = "NONE"
                    }
                  }
                }
              }
            }
          }
        }
        statement {
          regex_match_statement {
            regex_string = "^application/json(?:;[ ]*charset=utf-8)?$"
            field_to_match {
              single_header { name = "content-type" }
            }
            text_transformation {
              priority = 0
              type     = "LOWERCASE"
            }
          }
        }
      }
    }
    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "LabelSyncBodies"
      sampled_requests_enabled   = false
    }
  }
  rule {
    name     = "CommonProtections"
    priority = 10
    override_action {
      none {}
    }
    statement {
      managed_rule_group_statement {
        name        = "AWSManagedRulesCommonRuleSet"
        vendor_name = "AWS"
        # Only the 8 KiB size rule is replaced below. Every other managed rule
        # continues inspecting the request, including the first 8 KiB of its body.
        rule_action_override {
          name = "SizeRestrictions_BODY"
          action_to_use {
            count {}
          }
        }
      }
    }
    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "CommonProtections"
      sampled_requests_enabled   = false
    }
  }
  rule {
    name     = "OversizeExceptSyncBodies"
    priority = 20
    action {
      block {}
    }
    statement {
      and_statement {
        statement {
          size_constraint_statement {
            comparison_operator = "GT"
            size                = 8192
            field_to_match {
              body { oversize_handling = "MATCH" }
            }
            text_transformation {
              priority = 0
              type     = "NONE"
            }
          }
        }
        statement {
          not_statement {
            statement {
              label_match_statement {
                scope = "LABEL"
                key   = "sync-body"
              }
            }
          }
        }
      }
    }
    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "OversizeExceptSyncBodies"
      sampled_requests_enabled   = false
    }
  }

  rule {
    name     = "PerIpRate"
    priority = 30
    action {
      block {}
    }
    statement {
      rate_based_statement {
        aggregate_key_type    = "IP"
        evaluation_window_sec = 300
        limit                 = var.rate_limit
      }
    }
    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "PerIpRate"
      sampled_requests_enabled   = false
    }
  }
}
resource "aws_wafv2_web_acl_association" "api" {
  resource_arn = aws_lb.api.arn
  web_acl_arn  = aws_wafv2_web_acl.api.arn
}
