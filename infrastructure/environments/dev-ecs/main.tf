terraform {
  required_version = ">= 1.5"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 5.0"
    }
  }
}

provider "aws" {
  region = var.region
}

data "aws_availability_zones" "available" {
  state = "available"
}

locals {
  project = "wavis"
  env     = var.environment_name

  azs = slice(data.aws_availability_zones.available.names, 0, 2)

  tags = {
    Project     = local.project
    Environment = local.env
    Owner       = "wavis"
    CostCenter  = "wavis"
    ManagedBy   = "terraform"
  }

  ssm_prefix  = "/${local.project}/${local.env}"
  use_route53 = var.route53_hosted_zone_id != ""

  config = {
    REQUIRE_TLS                       = "false"
    TRUST_PROXY_HEADERS               = "true"
    RUST_LOG                          = "info"
    POSTGRES_USER                     = var.rds_master_username
    POSTGRES_DB                       = var.rds_db_name
    REQUIRE_INVITE_CODE               = "true"
    MAX_ROOM_PARTICIPANTS             = "6"
    INVITE_SWEEP_INTERVAL_SECS        = "60"
    SFU_JWT_ISSUER                    = "wavis-backend"
    SFU_TOKEN_TTL_SECS                = "600"
    REFRESH_TOKEN_TTL_DAYS            = "180"
    GITHUB_BUG_REPORT_REPO            = "Davalf99/wavis"
    BUG_REPORT_RATE_LIMIT_MAX         = "5"
    BUG_REPORT_RATE_LIMIT_WINDOW_SECS = "3600"
    BUG_REPORT_LLM_MODEL              = "claude-sonnet-4-20250514"
    LIVEKIT_HOST                      = "ws://${aws_eip.livekit.public_ip}:7880"
    LIVEKIT_PUBLIC_HOST               = "wss://${aws_cloudfront_distribution.backend.domain_name}"
  }

  managed_config_keys = toset([
    "REQUIRE_INVITE_CODE",
    "MAX_ROOM_PARTICIPANTS",
    "INVITE_SWEEP_INTERVAL_SECS",
    "SFU_JWT_ISSUER",
    "SFU_TOKEN_TTL_SECS",
    "REFRESH_TOKEN_TTL_DAYS",
  ])

  existing_config = {
    for key, value in local.config : key => value
    if !contains(local.managed_config_keys, key)
  }

  managed_config = {
    for key, value in local.config : key => value
    if contains(local.managed_config_keys, key)
  }

  secrets = {
    AUTH_JWT_SECRET         = "CHANGE-ME-auth-secret-32-bytes!!"
    AUTH_REFRESH_PEPPER     = "CHANGE-ME-pepper-32-bytes-min!!!"
    PHRASE_ENCRYPTION_KEY   = "CHANGE-ME-base64-32-byte-key!!!!"
    PAIRING_CODE_PEPPER     = "CHANGE-ME-pairing-pepper-32b!!!!"
    SFU_JWT_SECRET          = "CHANGE-ME-sfu-secret-32-bytes!!!"
    LIVEKIT_API_KEY         = "CHANGE-ME-livekit-api-key"
    LIVEKIT_API_SECRET      = "CHANGE-ME-livekit-api-secret"
    RDS_MASTER_PASSWORD     = "CHANGE-ME-rds-master-password"
    CF_ORIGIN_SECRET        = "CHANGE-ME-cf-origin-secret"
    GITHUB_BUG_REPORT_TOKEN = "CHANGE-ME-github-bug-report-token"
    GITHUB_DEPLOY_KEY       = "CHANGE-ME-github-deploy-key"
    BUG_REPORT_LLM_API_KEY  = "CHANGE-ME-bug-report-llm-api-key"
  }

  backend_secret_keys = toset([
    for key in keys(local.secrets) : key
  ])

  backend_certificate_arn = var.backend_enable_https ? (
    var.backend_certificate_arn != "" ? var.backend_certificate_arn : aws_acm_certificate.backend[0].arn
  ) : ""
  livekit_instance_id           = var.create_livekit_instance ? aws_instance.livekit[0].id : data.aws_instance.livekit[0].id
  livekit_public_ip             = aws_eip.livekit.public_ip
  livekit_eip_hostname          = "ec2-${replace(aws_eip.livekit.public_ip, ".", "-")}.${var.region}.compute.amazonaws.com"
  livekit_private_ip            = var.create_livekit_instance ? aws_instance.livekit[0].private_ip : data.aws_instance.livekit[0].private_ip
  livekit_instance_profile_name = var.create_livekit_instance ? aws_iam_instance_profile.livekit[0].name : null
}
