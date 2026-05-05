# migrate-ssm-ownership.ps1
#
# Transfers SSM parameter ownership from the legacy dev/ (EC2) Terraform
# environment to dev-ecs/. Run this ONCE before destroying the dev/ environment.
#
# Usage:
#   cd infrastructure\environments\dev-ecs
#   .\migrate-ssm-ownership.ps1

$ErrorActionPreference = "Continue"

Write-Host "=== SSM Ownership Migration: dev -> dev-ecs ===" -ForegroundColor Cyan
Write-Host ""

# Step 1: Import SSM secrets into dev-ecs state
Write-Host "-- Step 1: Importing SSM secrets into dev-ecs state --" -ForegroundColor Yellow

$secrets = @(
    "AUTH_JWT_SECRET"
    "AUTH_REFRESH_PEPPER"
    "PHRASE_ENCRYPTION_KEY"
    "PAIRING_CODE_PEPPER"
    "SFU_JWT_SECRET"
    "LIVEKIT_API_KEY"
    "LIVEKIT_API_SECRET"
    "RDS_MASTER_PASSWORD"
    "CF_ORIGIN_SECRET"
    "GITHUB_BUG_REPORT_TOKEN"
    "GITHUB_DEPLOY_KEY"
    "BUG_REPORT_LLM_API_KEY"
)

foreach ($key in $secrets) {
    Write-Host "  Importing aws_ssm_parameter.secrets[$key]..."
    terraform import "aws_ssm_parameter.secrets[\`"$key\`"]" "/wavis/dev/$key" 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  Warning: import failed or already in state, continuing" -ForegroundColor DarkYellow
    }
}

# Step 2: Import SSM existing_config into dev-ecs state
Write-Host ""
Write-Host "-- Step 2: Importing SSM existing_config into dev-ecs state --" -ForegroundColor Yellow

$existingConfig = @(
    "REQUIRE_TLS"
    "TRUST_PROXY_HEADERS"
    "RUST_LOG"
    "POSTGRES_USER"
    "POSTGRES_DB"
    "GITHUB_BUG_REPORT_REPO"
    "BUG_REPORT_RATE_LIMIT_MAX"
    "BUG_REPORT_RATE_LIMIT_WINDOW_SECS"
    "BUG_REPORT_LLM_MODEL"
    "LIVEKIT_HOST"
    "LIVEKIT_PUBLIC_HOST"
)

foreach ($key in $existingConfig) {
    Write-Host "  Importing aws_ssm_parameter.existing_config[$key]..."
    terraform import "aws_ssm_parameter.existing_config[\`"$key\`"]" "/wavis/dev/$key" 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  Warning: import failed or already in state, continuing" -ForegroundColor DarkYellow
    }
}

# Step 3: Remove SSM parameters from dev state
Write-Host ""
Write-Host "-- Step 3: Removing SSM parameters from dev state --" -ForegroundColor Yellow

$devSecrets = @(
    "AUTH_JWT_SECRET"
    "AUTH_REFRESH_PEPPER"
    "PHRASE_ENCRYPTION_KEY"
    "PAIRING_CODE_PEPPER"
    "SFU_JWT_SECRET"
    "LIVEKIT_API_KEY"
    "LIVEKIT_API_SECRET"
    "DATABASE_URL"
    "POSTGRES_PASSWORD"
    "CF_ORIGIN_SECRET"
    "GITHUB_DEPLOY_KEY"
    "GITHUB_BUG_REPORT_TOKEN"
)

foreach ($key in $devSecrets) {
    Write-Host "  Removing aws_ssm_parameter.secrets[$key] from dev state..."
    terraform -chdir="..\dev" state rm "aws_ssm_parameter.secrets[\`"$key\`"]" 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  Warning: not in state, skipping" -ForegroundColor DarkYellow
    }
}

$devConfig = @(
    "LIVEKIT_HOST"
    "LIVEKIT_PUBLIC_HOST"
    "REQUIRE_TLS"
    "TRUST_PROXY_HEADERS"
    "POSTGRES_USER"
    "POSTGRES_DB"
    "RUST_LOG"
    "GITHUB_BUG_REPORT_REPO"
    "BUG_REPORT_RATE_LIMIT_MAX"
    "BUG_REPORT_RATE_LIMIT_WINDOW_SECS"
)

foreach ($key in $devConfig) {
    Write-Host "  Removing aws_ssm_parameter.config[$key] from dev state..."
    terraform -chdir="..\dev" state rm "aws_ssm_parameter.config[\`"$key\`"]" 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  Warning: not in state, skipping" -ForegroundColor DarkYellow
    }
}

# Step 4: Remove shared LiveKit resources from dev state
Write-Host ""
Write-Host "-- Step 4: Removing shared LiveKit resources from dev state --" -ForegroundColor Yellow

$livekitResources = @(
    "aws_instance.livekit[0]"
    "aws_security_group.livekit[0]"
    "aws_iam_role.livekit[0]"
    "aws_iam_instance_profile.livekit[0]"
    "aws_iam_role_policy.livekit_ssm_session_manager[0]"
    "aws_iam_role_policy.livekit_ssm_read[0]"
    "aws_vpc_security_group_ingress_rule.livekit_http[0]"
    "aws_vpc_security_group_ingress_rule.livekit_ice_tcp[0]"
    "aws_vpc_security_group_ingress_rule.livekit_ice_media_udp[0]"
    "aws_vpc_security_group_ingress_rule.livekit_from_backend[0]"
    "aws_vpc_security_group_egress_rule.livekit_all_out[0]"
)

foreach ($res in $livekitResources) {
    Write-Host "  Removing $res from dev state..."
    terraform -chdir="..\dev" state rm $res 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host "  Warning: not in state, skipping" -ForegroundColor DarkYellow
    }
}

Write-Host ""
Write-Host "=== Migration complete ===" -ForegroundColor Green
Write-Host ""
Write-Host "Next steps:"
Write-Host "  1. terraform plan              # in dev-ecs - verify no destructive changes"
Write-Host "  2. cd ..\dev; terraform init -reconfigure; terraform plan"
Write-Host "     # should show only destroy of EC2 + CloudFront + SGs"
Write-Host "  3. Remove prevent_destroy from dev\ec2.tf and dev\cloudfront.tf"
Write-Host "     Then: terraform destroy"
