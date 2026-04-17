#!/usr/bin/env bash
# ssm-deploy.sh — Self-contained deploy script invoked by SSM send-command.
# Avoids the 2500-char SSM inline parameter limit by living on-disk.
#
# Usage (from GitHub Actions via SSM):
#   aws ssm send-command \
#     --document-name AWS-RunShellScript \
#     --parameters 'commands=["bash ~/wavis/deploy/ssm-deploy.sh"]' \
#     --instance-ids "$INSTANCE_ID"
#
# Requirements: 6.2, 6.7

set -euo pipefail

SSM_PREFIX="/wavis/dev"
REGION="${AWS_REGION:-us-east-2}"
REPO_DIR="${HOME}/wavis"
BRANCH="dev"

echo "=== Wavis SSM Deploy ==="
echo "Region: ${REGION}"
echo "Repo:   ${REPO_DIR}"
echo "Branch: ${BRANCH}"

# ---------------------------------------------------------------------------
# 1. Configure git credentials from SSM Parameter Store
# ---------------------------------------------------------------------------
echo "Fetching git deploy key from SSM (${SSM_PREFIX}/GITHUB_DEPLOY_KEY)..."

DEPLOY_KEY=$(aws ssm get-parameter \
  --name "${SSM_PREFIX}/GITHUB_DEPLOY_KEY" \
  --with-decryption \
  --query "Parameter.Value" \
  --output text \
  --region "$REGION")

if [[ -z "$DEPLOY_KEY" || "$DEPLOY_KEY" == *"CHANGE-ME"* ]]; then
  echo "ERROR: GITHUB_DEPLOY_KEY is missing or still a placeholder." >&2
  echo "Store a real deploy key in SSM at ${SSM_PREFIX}/GITHUB_DEPLOY_KEY before deploying." >&2
  exit 1
fi

# Determine credential type: SSH key starts with "-----BEGIN", otherwise treat as PAT.
if [[ "$DEPLOY_KEY" == -----BEGIN* ]]; then
  echo "Detected SSH deploy key — configuring GIT_SSH_COMMAND..."

  SSH_KEY_FILE=$(mktemp)
  # shellcheck disable=SC2064
  trap "rm -f '${SSH_KEY_FILE}'" EXIT

  printf '%s\n' "$DEPLOY_KEY" > "$SSH_KEY_FILE"
  chmod 600 "$SSH_KEY_FILE"

  export GIT_SSH_COMMAND="ssh -i ${SSH_KEY_FILE} -o StrictHostKeyChecking=accept-new"
else
  echo "Detected PAT — configuring git credential helper..."

  # Use a store-based credential helper with the PAT for HTTPS access.
  CRED_FILE=$(mktemp)
  # shellcheck disable=SC2064
  trap "rm -f '${CRED_FILE}'" EXIT

  printf 'https://x-access-token:%s@github.com\n' "$DEPLOY_KEY" > "$CRED_FILE"
  chmod 600 "$CRED_FILE"

  git config --global credential.helper "store --file=${CRED_FILE}"
fi

# ---------------------------------------------------------------------------
# 2. Update repository
# ---------------------------------------------------------------------------
echo "Changing to ${REPO_DIR}..."
cd "$REPO_DIR"

echo "Fetching origin..."
git fetch origin

echo "Pulling ${BRANCH} (fast-forward only)..."
git pull --ff-only origin "$BRANCH"

# ---------------------------------------------------------------------------
# 3. Fetch environment secrets from SSM
# ---------------------------------------------------------------------------
echo "Fetching SSM environment variables..."
bash deploy/fetch-ssm-env.sh .env

# ---------------------------------------------------------------------------
# 4. Build and start services
# ---------------------------------------------------------------------------
echo "Starting Docker Compose services..."
docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d --build --wait

# ---------------------------------------------------------------------------
# 5. Health check
# ---------------------------------------------------------------------------
echo "Running health check..."
curl --fail --silent --show-error --max-time 10 http://localhost:3000/health

echo ""
echo "=== Deploy complete ==="
