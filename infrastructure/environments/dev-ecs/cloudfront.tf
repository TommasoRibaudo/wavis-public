locals {
  backend_origin_id = "${local.project}-${local.env}-alb-origin"
  livekit_origin_id = "${local.project}-${local.env}-livekit-origin"
}

resource "aws_cloudfront_distribution" "backend" {
  enabled         = true
  is_ipv6_enabled = true
  comment         = "Wavis ${local.env} ECS backend + LiveKit signaling"
  tags            = local.tags

  # When enable_waf = true, attach the Terraform-managed WebACL.
  # When false (dev), the distribution runs without a WAF.
  web_acl_id = var.enable_waf ? aws_wafv2_web_acl.cloudfront[0].arn : null

  lifecycle {
    # price_class is left to AWS's CloudFront security plan defaults; we don't
    # want Terraform fighting with console-side adjustments to it.
    #
    # web_acl_id used to be ignored here too: while the CloudFront security
    # pricing-plan subscription was active, AWS rejected any UpdateDistribution
    # call that removed the WebACL, even one that only meant to change tags.
    # The subscription has since lapsed and the dev WebACL was detached
    # out-of-band (CLI, deliberately outside any apply -- an AWS-side rejection
    # mid-apply is what aborted a previous maintenance pass). Terraform now owns
    # the attribute again, so enable_waf is the single switch: false leaves the
    # distribution unprotected in dev, true recreates the WebACL from waf.tf.
    # See docs/dev-cost-maintenance-runbook.md.
    ignore_changes = [price_class]

    # Prod safety net: refuse to plan if someone tries to deploy a prod
    # environment with WAF disabled. Add new prod-like environment names
    # (e.g. "staging" if it should follow the same rule) here.
    precondition {
      condition     = var.enable_waf || !contains(["prod", "production"], lower(var.environment_name))
      error_message = "WAF must be enabled for prod/production environments. Set enable_waf = true (or remove the override) in terraform.tfvars."
    }
  }

  origin {
    domain_name = aws_lb.backend.dns_name
    origin_id   = local.backend_origin_id

    custom_origin_config {
      http_port                = 80
      https_port               = 443
      origin_protocol_policy   = "http-only"
      origin_ssl_protocols     = ["TLSv1.2"]
      origin_read_timeout      = 120
      origin_keepalive_timeout = 60
    }

    custom_header {
      name  = "X-Forwarded-Proto"
      value = "https"
    }

    custom_header {
      name  = "X-Wavis-Forwarded-Proto"
      value = "https"
    }

    custom_header {
      name  = "X-Origin-Verify"
      value = aws_ssm_parameter.secrets["CF_ORIGIN_SECRET"].value
    }
  }

  origin {
    domain_name = local.livekit_eip_hostname
    origin_id   = local.livekit_origin_id

    custom_origin_config {
      http_port                = 7880
      https_port               = 443
      origin_protocol_policy   = "http-only"
      origin_ssl_protocols     = ["TLSv1.2"]
      origin_read_timeout      = 120
      origin_keepalive_timeout = 60
    }

    custom_header {
      name  = "X-Origin-Verify"
      value = aws_ssm_parameter.secrets["CF_ORIGIN_SECRET"].value
    }
  }

  default_cache_behavior {
    target_origin_id       = local.backend_origin_id
    viewer_protocol_policy = "redirect-to-https"
    allowed_methods        = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]
    cached_methods         = ["GET", "HEAD"]
    compress               = true

    cache_policy_id          = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad"
    origin_request_policy_id = "b689b0a8-53d0-40ab-baf2-68738e2966ac"
  }

  ordered_cache_behavior {
    path_pattern           = "/ws*"
    target_origin_id       = local.backend_origin_id
    viewer_protocol_policy = "redirect-to-https"
    allowed_methods        = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]
    cached_methods         = ["GET", "HEAD"]
    compress               = true

    cache_policy_id          = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad"
    origin_request_policy_id = "b689b0a8-53d0-40ab-baf2-68738e2966ac"
  }

  ordered_cache_behavior {
    path_pattern           = "/rtc*"
    target_origin_id       = local.livekit_origin_id
    viewer_protocol_policy = "redirect-to-https"
    allowed_methods        = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]
    cached_methods         = ["GET", "HEAD"]
    compress               = true

    cache_policy_id          = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad"
    origin_request_policy_id = "216adef6-5c7f-47e4-b989-5492eafa07d3"
  }

  restrictions {
    geo_restriction {
      restriction_type = "none"
    }
  }

  viewer_certificate {
    cloudfront_default_certificate = true
  }
}
