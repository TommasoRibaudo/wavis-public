# ---------- SSM Parameter Store (secrets) ----------
# Created with placeholder values. Update after first apply:
#   aws ssm put-parameter --region us-east-2 --name "/wavis/dev/AUTH_JWT_SECRET" --value "real-secret" --type SecureString --overwrite

locals {
  ssm_prefix = "/${local.project}/${local.env}"
  postgres_user = "wavis"
  postgres_db   = "wavis"
  postgres_password = "CHANGE-ME-postgres-password"

  secrets = {
    AUTH_JWT_SECRET       = "CHANGE-ME-auth-secret-32-bytes!!"
    AUTH_REFRESH_PEPPER   = "CHANGE-ME-pepper-32-bytes-min!!!"
    PHRASE_ENCRYPTION_KEY = "CHANGE-ME-base64-32-byte-key!!!!"
    PAIRING_CODE_PEPPER   = "CHANGE-ME-pairing-pepper-32b!!!!"
    SFU_JWT_SECRET        = "CHANGE-ME-sfu-secret-32-bytes!!!"
    LIVEKIT_API_KEY       = "devkey"
    LIVEKIT_API_SECRET    = "secret"
    DATABASE_URL = var.enable_rds ? "postgres://${var.rds_master_username}:${try(aws_ssm_parameter.rds_master_password[0].value, "")}@${try(aws_db_instance.wavis[0].endpoint, "")}/${var.rds_db_name}" : "postgres://${local.postgres_user}:${local.postgres_password}@postgres:5432/${local.postgres_db}"
    POSTGRES_PASSWORD     = local.postgres_password
    CF_ORIGIN_SECRET          = "CHANGE-ME-cf-origin-secret"
    GITHUB_DEPLOY_KEY         = "CHANGE-ME-github-deploy-key"
    GITHUB_BUG_REPORT_TOKEN   = "CHANGE-ME-github-bug-report-token"
  }

  # When LiveKit runs on a separate instance, point LIVEKIT_HOST at its
  # private IP (backend→LiveKit API) and LIVEKIT_PUBLIC_HOST at its public
  # IP (client→LiveKit WebSocket).  Otherwise keep the colocated defaults.
  # Requirements: 8.3, 8.4
  config = {
    LIVEKIT_HOST        = local.livekit_enabled ? "ws://${aws_instance.livekit[0].private_ip}:7880" : "ws://livekit:7880"
    LIVEKIT_PUBLIC_HOST = local.livekit_enabled ? "wss://${aws_instance.livekit[0].public_ip}:7880" : "wss://dt2nm86rf5ksq.cloudfront.net/livekit"
    REQUIRE_TLS         = "true"
    TRUST_PROXY_HEADERS = "true"
    POSTGRES_USER       = local.postgres_user
    POSTGRES_DB         = local.postgres_db
    RUST_LOG                        = "info"
    GITHUB_BUG_REPORT_REPO          = "Davalf99/wavis"
    BUG_REPORT_RATE_LIMIT_MAX       = "5"
    BUG_REPORT_RATE_LIMIT_WINDOW_SECS = "3600"
  }
}

resource "aws_ssm_parameter" "secrets" {
  for_each = local.secrets

  name  = "${local.ssm_prefix}/${each.key}"
  type  = "SecureString"
  value = each.value
  tags  = local.tags

  lifecycle {
    ignore_changes = [value]
  }
}

resource "aws_ssm_parameter" "config" {
  for_each = local.config

  name  = "${local.ssm_prefix}/${each.key}"
  type  = "String"
  value = each.value
  tags  = local.tags
}
