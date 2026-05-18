# ---------- CloudFront WAFv2 ----------
#
# Optional Terraform-owned WebACL attached to the dev/prod CloudFront
# distribution. Gated by var.enable_waf so dev can opt out (cost) while prod
# always gets one by default.
#
# A `lifecycle.precondition` on aws_cloudfront_distribution.backend
# (in cloudfront.tf) refuses to plan if environment_name is "prod" or
# "production" while enable_waf = false. That's the prod safety net.
#
# CloudFront WebACLs are GLOBAL but billed/managed in us-east-1, hence the
# aliased provider.

resource "aws_wafv2_web_acl" "cloudfront" {
  count = var.enable_waf ? 1 : 0

  provider = aws.us_east_1

  name        = "${local.project}-${local.env}-cloudfront"
  description = "Wavis ${local.env} CloudFront WebACL — managed by Terraform"
  scope       = "CLOUDFRONT"

  default_action {
    allow {}
  }

  # AWS managed common rule set: OWASP-style baseline (XSS, SQLi, bad bots, etc.)
  rule {
    name     = "AWSManagedRulesCommonRuleSet"
    priority = 1

    override_action {
      none {}
    }

    statement {
      managed_rule_group_statement {
        name        = "AWSManagedRulesCommonRuleSet"
        vendor_name = "AWS"
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.project}-${local.env}-cf-common-rules"
      sampled_requests_enabled   = true
    }
  }

  # AWS managed IP reputation list: blocks known bad actors
  rule {
    name     = "AWSManagedRulesAmazonIpReputationList"
    priority = 2

    override_action {
      none {}
    }

    statement {
      managed_rule_group_statement {
        name        = "AWSManagedRulesAmazonIpReputationList"
        vendor_name = "AWS"
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.project}-${local.env}-cf-ip-reputation"
      sampled_requests_enabled   = true
    }
  }

  # AWS managed known-bad-inputs: protects against well-known exploits
  rule {
    name     = "AWSManagedRulesKnownBadInputsRuleSet"
    priority = 3

    override_action {
      none {}
    }

    statement {
      managed_rule_group_statement {
        name        = "AWSManagedRulesKnownBadInputsRuleSet"
        vendor_name = "AWS"
      }
    }

    visibility_config {
      cloudwatch_metrics_enabled = true
      metric_name                = "${local.project}-${local.env}-cf-known-bad-inputs"
      sampled_requests_enabled   = true
    }
  }

  visibility_config {
    cloudwatch_metrics_enabled = true
    metric_name                = "${local.project}-${local.env}-cloudfront"
    sampled_requests_enabled   = true
  }

  tags = local.tags
}
