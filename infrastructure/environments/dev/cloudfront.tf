# ---------- CloudFront Distribution ----------
# Imported into Terraform via:
#   terraform import aws_cloudfront_distribution.wavis E20JDZ30DRXUY6
#
# Two origins: backend (port 3000) and LiveKit (port 7880).
# TLS termination + WAF + origin verification.
#
# When enable_private_subnet = true, the backend origin switches from
# custom origin (public DNS) to VPC origin (private ENI → port 3000).
# Requirements: 4.1, 4.2, 4.3, 4.4

locals {
  cf_distribution_id = "E20JDZ30DRXUY6"
  # When private subnet is enabled, the instance has no public DNS.
  # The VPC origin handles connectivity; domain_name is set to the
  # private IP for the origin block (CloudFront ignores it for VPC origins
  # but the field is still required by the schema).
  origin_domain     = var.enable_private_subnet ? aws_instance.wavis.private_dns : aws_instance.wavis.public_dns
  backend_origin_id = "${local.project}-${local.env}-ec2-origin"
  livekit_origin_id = "${local.project}-livekit-7880"
}

# ── CloudFront VPC Origin (private subnet mode) ────────────────────
# Creates a managed ENI in the private subnet that CloudFront uses to
# reach the backend on port 3000 without a public IP.
# Requirements: 4.1

resource "aws_cloudfront_vpc_origin" "wavis" {
  count = var.enable_private_subnet ? 1 : 0

  vpc_origin_endpoint_config {
    name                   = "${local.project}-backend-vpc-origin-${local.env}"
    arn                    = aws_instance.wavis.arn
    http_port              = 3000
    https_port             = 443
    origin_protocol_policy = "http-only"

    origin_ssl_protocols {
      items    = ["TLSv1.2"]
      quantity = 1
    }
  }

  tags = local.tags
}

# ── CloudFront Distribution ─────────────────────────────────────────

resource "aws_cloudfront_distribution" "wavis" {
  enabled         = true
  is_ipv6_enabled = true
  comment         = "Wavis ${local.env} — API + WebSocket + LiveKit"
  price_class     = "PriceClass_All"
  web_acl_id      = var.waf_web_acl_arn
  tags            = local.tags

  # --- Origin 1: Backend on port 3000 (custom origin — public subnet) ---
  # Used when enable_private_subnet = false (current/default behavior).
  dynamic "origin" {
    for_each = var.enable_private_subnet ? [] : [1]
    content {
      domain_name = local.origin_domain
      origin_id   = local.backend_origin_id

      custom_origin_config {
        http_port                = 3000
        https_port               = 443
        origin_protocol_policy   = "http-only"
        origin_ssl_protocols     = ["SSLv3", "TLSv1", "TLSv1.1", "TLSv1.2"]
        origin_read_timeout      = 60
        origin_keepalive_timeout = 5
      }

      custom_header {
        name  = "X-Forwarded-Proto"
        value = "https"
      }

      custom_header {
        name  = "X-Origin-Verify"
        value = var.cf_origin_secret
      }
    }
  }

  # --- Origin 1: Backend on port 3000 (VPC origin — private subnet) ---
  # Used when enable_private_subnet = true. CloudFront connects via a
  # managed ENI in the private subnet — no public IP needed.
  # Requirements: 4.1, 4.2
  dynamic "origin" {
    for_each = var.enable_private_subnet ? [1] : []
    content {
      domain_name = local.origin_domain
      origin_id   = local.backend_origin_id

      vpc_origin_config {
        vpc_origin_id            = aws_cloudfront_vpc_origin.wavis[0].id
        origin_read_timeout      = 60
        origin_keepalive_timeout = 60
      }

      # Requirement 4.3: increased timeout for WebSocket idle connections.
      # vpc_origin_config caps origin_read_timeout at 60s. The AWS API now
      # supports response_completion_timeout (up to 180s) but the Terraform
      # AWS provider does not yet expose it (as of v5.100). Once provider
      # support lands, add: response_completion_timeout = 180 here.
      # Track: https://github.com/hashicorp/terraform-provider-aws/issues/44116

      custom_header {
        name  = "X-Forwarded-Proto"
        value = "https"
      }

      # Requirement 4.4: origin secret header preserved in VPC origin mode
      custom_header {
        name  = "X-Origin-Verify"
        value = var.cf_origin_secret
      }
    }
  }

  # --- Origin 2: LiveKit on port 7880 ---
  # When livekit_deployment_mode = "separate", points to the dedicated LiveKit
  # instance. Otherwise, points to the backend instance (colocated).
  origin {
    domain_name = local.livekit_enabled ? aws_instance.livekit[0].public_dns : local.origin_domain
    origin_id   = local.livekit_origin_id

    custom_origin_config {
      http_port                = 7880
      https_port               = 443
      origin_protocol_policy   = "http-only"
      origin_ssl_protocols     = ["SSLv3", "TLSv1", "TLSv1.1", "TLSv1.2"]
      origin_read_timeout      = 30
      origin_keepalive_timeout = 5
    }

    custom_header {
      name  = "X-Origin-Verify"
      value = var.cf_origin_secret
    }
  }

  # --- Default behavior: Backend API traffic ---
  default_cache_behavior {
    target_origin_id       = local.backend_origin_id
    viewer_protocol_policy = "redirect-to-https"
    allowed_methods        = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]
    cached_methods         = ["GET", "HEAD"]
    compress               = true

    cache_policy_id          = "83da9c7e-98b4-4e11-a168-04f0df8e2c65" # Managed-CachingOptimized
    origin_request_policy_id = "b689b0a8-53d0-40ab-baf2-68738e2966ac" # Managed-AllViewerExceptHostHeader
  }

  # --- /ws* behavior: WebSocket signaling ---
  ordered_cache_behavior {
    path_pattern           = "/ws*"
    target_origin_id       = local.backend_origin_id
    viewer_protocol_policy = "redirect-to-https"
    allowed_methods        = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]
    cached_methods         = ["GET", "HEAD"]
    compress               = true

    cache_policy_id          = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad" # Managed-CachingDisabled
    origin_request_policy_id = "b689b0a8-53d0-40ab-baf2-68738e2966ac" # Managed-AllViewerExceptHostHeader
  }

  # --- /rtc* behavior: LiveKit WebRTC signaling ---
  ordered_cache_behavior {
    path_pattern           = "/rtc*"
    target_origin_id       = local.livekit_origin_id
    viewer_protocol_policy = "redirect-to-https"
    allowed_methods        = ["DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT"]
    cached_methods         = ["GET", "HEAD"]
    compress               = true

    cache_policy_id          = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad" # Managed-CachingDisabled
    origin_request_policy_id = "216adef6-5c7f-47e4-b989-5492eafa07d3" # Managed-AllViewer
  }

  restrictions {
    geo_restriction {
      restriction_type = "none"
    }
  }

  viewer_certificate {
    cloudfront_default_certificate = true
  }

  lifecycle {
    prevent_destroy = true
    ignore_changes  = [origin]
  }
}
