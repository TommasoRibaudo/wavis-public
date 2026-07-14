# ---------- Marketing website (static) ----------
# Hosts the Next.js static export (website/out) on a private S3 bucket behind a
# dedicated CloudFront distribution. This is separate from the API/LiveKit
# distribution in cloudfront.tf — do not merge them; they serve different
# origins with different cache behavior.
#
# Toggle with enable_website. Free-tier eligible: CloudFront includes 1 TB out
# and 10M requests/month; the bucket holds a few hundred KB.

variable "enable_website" {
  description = "Provision the static marketing website (S3 + CloudFront)."
  type        = bool
  default     = true
}

locals {
  website_bucket = "${local.project}-${local.env}-website-${data.aws_caller_identity.current.account_id}"
}

# ── S3 bucket (private; reached only through CloudFront OAC) ──────────
resource "aws_s3_bucket" "website" {
  count  = var.enable_website ? 1 : 0
  bucket = local.website_bucket
  tags   = local.tags
}

resource "aws_s3_bucket_public_access_block" "website" {
  count                   = var.enable_website ? 1 : 0
  bucket                  = aws_s3_bucket.website[0].id
  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_ownership_controls" "website" {
  count  = var.enable_website ? 1 : 0
  bucket = aws_s3_bucket.website[0].id
  rule {
    object_ownership = "BucketOwnerEnforced"
  }
}

# ── CloudFront Origin Access Control (signed requests to S3) ─────────
resource "aws_cloudfront_origin_access_control" "website" {
  count                             = var.enable_website ? 1 : 0
  name                              = "${local.project}-${local.env}-website-oac"
  origin_access_control_origin_type = "s3"
  signing_behavior                  = "always"
  signing_protocol                  = "sigv4"
}

# ── CloudFront distribution ──────────────────────────────────────────
resource "aws_cloudfront_distribution" "website" {
  count               = var.enable_website ? 1 : 0
  enabled             = true
  is_ipv6_enabled     = true
  comment             = "Wavis ${local.env} — marketing website"
  default_root_object = "index.html"
  price_class         = "PriceClass_100"
  tags                = local.tags

  origin {
    domain_name              = aws_s3_bucket.website[0].bucket_regional_domain_name
    origin_id                = "website-s3"
    origin_access_control_id = aws_cloudfront_origin_access_control.website[0].id
  }

  default_cache_behavior {
    target_origin_id       = "website-s3"
    viewer_protocol_policy = "redirect-to-https"
    allowed_methods        = ["GET", "HEAD", "OPTIONS"]
    cached_methods         = ["GET", "HEAD"]
    compress               = true

    cache_policy_id = "658327ea-f89d-4fab-a63d-7e88639e58f6" # Managed-CachingOptimized
  }

  # Static export serves a single page plus a generated 404.html. With OAC and
  # GetObject-only access, S3 returns 403 for a missing key — map both to the
  # exported 404 page.
  custom_error_response {
    error_code            = 403
    response_code         = 404
    response_page_path    = "/404.html"
    error_caching_min_ttl = 60
  }
  custom_error_response {
    error_code            = 404
    response_code         = 404
    response_page_path    = "/404.html"
    error_caching_min_ttl = 60
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

# ── Bucket policy: allow only this distribution to read objects ──────
data "aws_iam_policy_document" "website" {
  count = var.enable_website ? 1 : 0
  statement {
    sid       = "AllowCloudFrontRead"
    actions   = ["s3:GetObject"]
    resources = ["${aws_s3_bucket.website[0].arn}/*"]

    principals {
      type        = "Service"
      identifiers = ["cloudfront.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "AWS:SourceArn"
      values   = [aws_cloudfront_distribution.website[0].arn]
    }
  }
}

resource "aws_s3_bucket_policy" "website" {
  count  = var.enable_website ? 1 : 0
  bucket = aws_s3_bucket.website[0].id
  policy = data.aws_iam_policy_document.website[0].json
}

# ── Outputs ──────────────────────────────────────────────────────────
output "website_bucket" {
  description = "S3 bucket holding the static website export."
  value       = var.enable_website ? aws_s3_bucket.website[0].bucket : null
}

output "website_cloudfront_id" {
  description = "CloudFront distribution ID for the website (for cache invalidation)."
  value       = var.enable_website ? aws_cloudfront_distribution.website[0].id : null
}

output "website_url" {
  description = "Public URL of the marketing website."
  value       = var.enable_website ? "https://${aws_cloudfront_distribution.website[0].domain_name}" : null
}
