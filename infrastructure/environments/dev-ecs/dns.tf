resource "aws_acm_certificate" "backend" {
  count = var.backend_enable_https && var.backend_certificate_arn == "" ? 1 : 0

  domain_name       = var.backend_public_hostname
  validation_method = "DNS"

  lifecycle {
    create_before_destroy = true
  }

  tags = merge(local.tags, {
    Name = "${local.project}-${local.env}-backend-cert"
  })
}

resource "aws_route53_record" "backend_certificate_validation" {
  for_each = local.use_route53 && var.backend_enable_https && var.backend_certificate_arn == "" ? {
    for dvo in aws_acm_certificate.backend[0].domain_validation_options :
    dvo.domain_name => {
      name   = dvo.resource_record_name
      record = dvo.resource_record_value
      type   = dvo.resource_record_type
    }
  } : {}

  zone_id = var.route53_hosted_zone_id
  name    = each.value.name
  type    = each.value.type
  ttl     = 60
  records = [each.value.record]
}

resource "aws_acm_certificate_validation" "backend" {
  count = local.use_route53 && var.backend_enable_https && var.backend_certificate_arn == "" ? 1 : 0

  certificate_arn         = aws_acm_certificate.backend[0].arn
  validation_record_fqdns = [for record in aws_route53_record.backend_certificate_validation : record.fqdn]
}

resource "aws_route53_record" "backend_alias" {
  count = local.use_route53 && var.backend_public_hostname != "" ? 1 : 0

  zone_id = var.route53_hosted_zone_id
  name    = var.backend_public_hostname
  type    = "A"

  alias {
    name                   = aws_lb.backend.dns_name
    zone_id                = aws_lb.backend.zone_id
    evaluate_target_health = true
  }
}

resource "aws_route53_record" "livekit" {
  count = local.use_route53 ? 1 : 0

  zone_id = var.route53_hosted_zone_id
  name    = var.livekit_public_hostname
  type    = "A"
  ttl     = 300
  records = [local.livekit_public_ip]
}
