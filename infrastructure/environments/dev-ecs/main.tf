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

  # Apply Wavis-standard tags to every resource that supports tags. Per-resource
  # `tags` blocks (via `merge(local.tags, ...)`) still work — they merge with
  # these defaults. This is what guarantees the cost-allocation tags show up on
  # auto-created sub-resources (e.g. the LiveKit EBS root volume, ACM cert,
  # CloudWatch alarms missing explicit `tags = local.tags`, ECS task-def
  # revisions, DynamoDB tables, etc.).
  default_tags {
    tags = {
      Project     = "wavis"
      Environment = var.environment_name
      Owner       = "wavis"
      CostCenter  = "wavis"
      ManagedBy   = "terraform"
    }
  }
}

# Aliased provider for us-east-1 — required for CloudFront WAFs (always global,
# managed in us-east-1) and any other CloudFront-bound resources like ACM certs
# used as viewer certificates.
provider "aws" {
  alias  = "us_east_1"
  region = "us-east-1"

  default_tags {
    tags = {
      Project     = "wavis"
      Environment = var.environment_name
      Owner       = "wavis"
      CostCenter  = "wavis"
      ManagedBy   = "terraform"
    }
  }
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
    GITHUB_BUG_REPORT_REPO            = "TommasoRibaudo/wavis-public"
    BUG_REPORT_RATE_LIMIT_MAX         = "5"
    BUG_REPORT_RATE_LIMIT_WINDOW_SECS = "3600"
    LLM_RATE_LIMIT_MAX                = "5"
    LLM_RATE_LIMIT_WINDOW_SECS        = "86400"
    BUG_REPORT_LLM_MODEL              = "claude-sonnet-4-20250514"
    LIVEKIT_HOST                      = "ws://${aws_eip.livekit.public_ip}:7880"
    LIVEKIT_PUBLIC_HOST               = "wss://${aws_cloudfront_distribution.backend.domain_name}"
    # TURN configuration intentionally absent in dev-ecs.
    #
    # The backend's HMAC-SHA1 issuer (wavis-backend/src/voice/turn_cred.rs)
    # produces RFC 5766 long-term credentials. LiveKit v1.9.3's embedded
    # TURN does not honor that auth scheme — it derives credentials from
    # LIVEKIT_API_KEY / LIVEKIT_API_SECRET per participant. Pointing
    # WAVIS_TURN_URLS at LiveKit's TURN endpoint would produce backend-
    # issued iceServers entries that fail STUN Allocate on every join.
    #
    # With WAVIS_TURN_URLS / TURN_SHARED_SECRET unset, TurnConfig::try_from_env
    # returns Ok(None), inject_turn_credentials no-ops, and the GUI relies
    # exclusively on LiveKit's JoinResponse.ice_servers for relay.
    #
    # Re-enable when a TURN server that honors RFC 5766 (e.g. a coturn
    # sidecar) lands. Tracked in the workspace-only spec at
    # .kiro/specs/turn-credentials-audit/ (Addendum: LiveKit auth).
    # See also: .kiro/specs/turn-relay-symmetric-nat-fix/ — defines the
    # post-Option-B contingency path; backend issuer remains dormant
    # pending standalone-coturn promotion.
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
    # TURN_SHARED_SECRET intentionally absent — see local.config above.
    #
    # IMPORTANT: removing this key from local.secrets causes
    # `aws_ssm_parameter.secrets["TURN_SHARED_SECRET"]` (created by a
    # previous apply) to be **destroyed** on the next apply. That is
    # the desired outcome — the backend stops issuing dead HMAC
    # credentials — but it is a destructive change. Review
    # `terraform plan` before running apply.
    # See also: .kiro/specs/turn-relay-symmetric-nat-fix/ — defines the
    # post-Option-B contingency path; backend issuer remains dormant
    # pending standalone-coturn promotion.
    LIVEKIT_API_KEY         = "CHANGE-ME-livekit-api-key"
    LIVEKIT_API_SECRET      = "CHANGE-ME-livekit-api-secret"
    RDS_MASTER_PASSWORD     = "CHANGE-ME-rds-master-password"
    CF_ORIGIN_SECRET        = "CHANGE-ME-cf-origin-secret"
    GITHUB_BUG_REPORT_TOKEN = "CHANGE-ME-github-bug-report-token"
    GITHUB_DEPLOY_KEY       = "CHANGE-ME-github-deploy-key"
    BUG_REPORT_LLM_API_KEY  = "CHANGE-ME-bug-report-llm-api-key"
    ADMIN_TOKEN             = "CHANGE-ME-admin-token"
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
