#!/usr/bin/env bash
# migrate-ssm-ownership.sh
#
# Transfers SSM parameter ownership from the legacy dev/ (EC2) Terraform
# environment to dev-ecs/. Run this ONCE before destroying the dev/ environment.
#
# Steps:
#   1. Import existing SSM parameters into dev-ecs state
#   2. Remove SSM parameters from dev state (so destroy won't delete them)
#   3. Remove the LiveKit instance from dev state (shared with dev-ecs)
#   4. Remove prevent_destroy resources from dev state
#
# Prerequisites:
#   - AWS credentials configured
#   - Terraform initialized in both environments
#
# Usage:
#   cd infrastructure/environments/dev-ecs
#   bash migrate-ssm-ownership.sh

set -euo pipefail

SSM_PREFIX="/wavis/dev"
DEV_DIR="../dev"
ECS_DIR="."
REGION="us-east-2"

echo "=== SSM Ownership Migration: dev → dev-ecs ==="
echo ""

# ─────────────────────────────────────────────────────────────────────
# Step 1: Import SSM secrets into dev-ecs state
# ─────────────────────────────────────────────────────────────────────
echo "── Step 1: Importing SSM secrets into dev-ecs state ──"

SECRETS=(
  AUTH_JWT_SECRET
  AUTH_REFRESH_PEPPER
  PHRASE_ENCRYPTION_KEY
  PAIRING_CODE_PEPPER
  SFU_JWT_SECRET
  LIVEKIT_API_KEY
  LIVEKIT_API_SECRET
  RDS_MASTER_PASSWORD
  CF_ORIGIN_SECRET
  GITHUB_BUG_REPORT_TOKEN
  GITHUB_DEPLOY_KEY
  BUG_REPORT_LLM_API_KEY
)

for key in "${SECRETS[@]}"; do
  echo "  Importing aws_ssm_parameter.secrets[\"${key}\"]..."
  terraform -chdir="$ECS_DIR" import \
    "aws_ssm_parameter.secrets[\"${key}\"]" \
    "${SSM_PREFIX}/${key}" || echo "  ⚠ Already in state or not found, skipping"
done

# ─────────────────────────────────────────────────────────────────────
# Step 2: Import SSM existing_config into dev-ecs state
# ─────────────────────────────────────────────────────────────────────
echo ""
echo "── Step 2: Importing SSM existing_config into dev-ecs state ──"

# These are the config keys NOT in managed_config_keys (i.e. existing_config)
EXISTING_CONFIG_KEYS=(
  REQUIRE_TLS
  TRUST_PROXY_HEADERS
  RUST_LOG
  POSTGRES_USER
  POSTGRES_DB
  GITHUB_BUG_REPORT_REPO
  BUG_REPORT_RATE_LIMIT_MAX
  BUG_REPORT_RATE_LIMIT_WINDOW_SECS
  BUG_REPORT_LLM_MODEL
  LIVEKIT_HOST
  LIVEKIT_PUBLIC_HOST
)

for key in "${EXISTING_CONFIG_KEYS[@]}"; do
  echo "  Importing aws_ssm_parameter.existing_config[\"${key}\"]..."
  terraform -chdir="$ECS_DIR" import \
    "aws_ssm_parameter.existing_config[\"${key}\"]" \
    "${SSM_PREFIX}/${key}" || echo "  ⚠ Already in state or not found, skipping"
done

# ─────────────────────────────────────────────────────────────────────
# Step 3: Remove SSM parameters from dev state
# ─────────────────────────────────────────────────────────────────────
echo ""
echo "── Step 3: Removing SSM parameters from dev state ──"

# Remove all secret SSM params from dev state
DEV_SECRETS=(
  AUTH_JWT_SECRET
  AUTH_REFRESH_PEPPER
  PHRASE_ENCRYPTION_KEY
  PAIRING_CODE_PEPPER
  SFU_JWT_SECRET
  LIVEKIT_API_KEY
  LIVEKIT_API_SECRET
  DATABASE_URL
  POSTGRES_PASSWORD
  CF_ORIGIN_SECRET
  GITHUB_DEPLOY_KEY
  GITHUB_BUG_REPORT_TOKEN
)

for key in "${DEV_SECRETS[@]}"; do
  echo "  Removing aws_ssm_parameter.secrets[\"${key}\"] from dev state..."
  terraform -chdir="$DEV_DIR" state rm \
    "aws_ssm_parameter.secrets[\"${key}\"]" 2>/dev/null || echo "  ⚠ Not in state, skipping"
done

# Remove all config SSM params from dev state
DEV_CONFIG_KEYS=(
  LIVEKIT_HOST
  LIVEKIT_PUBLIC_HOST
  REQUIRE_TLS
  TRUST_PROXY_HEADERS
  POSTGRES_USER
  POSTGRES_DB
  RUST_LOG
  GITHUB_BUG_REPORT_REPO
  BUG_REPORT_RATE_LIMIT_MAX
  BUG_REPORT_RATE_LIMIT_WINDOW_SECS
)

for key in "${DEV_CONFIG_KEYS[@]}"; do
  echo "  Removing aws_ssm_parameter.config[\"${key}\"] from dev state..."
  terraform -chdir="$DEV_DIR" state rm \
    "aws_ssm_parameter.config[\"${key}\"]" 2>/dev/null || echo "  ⚠ Not in state, skipping"
done

# ─────────────────────────────────────────────────────────────────────
# Step 4: Remove shared LiveKit instance from dev state (if present)
# ─────────────────────────────────────────────────────────────────────
echo ""
echo "── Step 4: Removing shared LiveKit resources from dev state ──"

# The LiveKit instance may be managed by dev/ with livekit_deployment_mode=separate.
# dev-ecs references it via existing_livekit_instance_id. Remove from dev state
# so destroy won't terminate it.
echo "  Removing aws_instance.livekit[0] from dev state..."
terraform -chdir="$DEV_DIR" state rm \
  "aws_instance.livekit[0]" 2>/dev/null || echo "  ⚠ Not in state, skipping"

echo "  Removing aws_security_group.livekit[0] from dev state..."
terraform -chdir="$DEV_DIR" state rm \
  "aws_security_group.livekit[0]" 2>/dev/null || echo "  ⚠ Not in state, skipping"

echo "  Removing aws_iam_role.livekit[0] from dev state..."
terraform -chdir="$DEV_DIR" state rm \
  "aws_iam_role.livekit[0]" 2>/dev/null || echo "  ⚠ Not in state, skipping"

echo "  Removing aws_iam_instance_profile.livekit[0] from dev state..."
terraform -chdir="$DEV_DIR" state rm \
  "aws_iam_instance_profile.livekit[0]" 2>/dev/null || echo "  ⚠ Not in state, skipping"

echo "  Removing aws_iam_role_policy.livekit_ssm_session_manager[0] from dev state..."
terraform -chdir="$DEV_DIR" state rm \
  "aws_iam_role_policy.livekit_ssm_session_manager[0]" 2>/dev/null || echo "  ⚠ Not in state, skipping"

echo "  Removing aws_iam_role_policy.livekit_ssm_read[0] from dev state..."
terraform -chdir="$DEV_DIR" state rm \
  "aws_iam_role_policy.livekit_ssm_read[0]" 2>/dev/null || echo "  ⚠ Not in state, skipping"

# LiveKit security group ingress/egress rules
for rule in livekit_http livekit_ice_tcp livekit_ice_media_udp livekit_from_backend; do
  echo "  Removing aws_vpc_security_group_ingress_rule.${rule}[0] from dev state..."
  terraform -chdir="$DEV_DIR" state rm \
    "aws_vpc_security_group_ingress_rule.${rule}[0]" 2>/dev/null || echo "  ⚠ Not in state, skipping"
done

echo "  Removing aws_vpc_security_group_egress_rule.livekit_all_out[0] from dev state..."
terraform -chdir="$DEV_DIR" state rm \
  "aws_vpc_security_group_egress_rule.livekit_all_out[0]" 2>/dev/null || echo "  ⚠ Not in state, skipping"

echo ""
echo "=== Migration complete ==="
echo ""
echo "Next steps:"
echo "  1. cd infrastructure/environments/dev-ecs"
echo "     terraform plan    # Verify no destructive changes"
echo ""
echo "  2. cd infrastructure/environments/dev"
echo "     terraform plan    # Should show only destroy of EC2 + CloudFront + SGs"
echo ""
echo "  3. Remove prevent_destroy from dev/ec2.tf and dev/cloudfront.tf"
echo "     Then: terraform destroy"
