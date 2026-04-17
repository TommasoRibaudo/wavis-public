#!/usr/bin/env bash
# ssm-deploy-livekit.sh — Self-contained deploy script for LiveKit via SSM send-command.
# Follows the same pattern as ssm-deploy.sh but targets the LiveKit compose file
# and health-checks on port 7880 instead of the backend on port 3000.
#
# Usage (from GitHub Actions via SSM):
#   aws ssm send-command \
#     --document-name AWS-RunShellScript \
#     --parameters 'commands=["bash ~/wavis/deploy/ssm-deploy-livekit.sh"]' \
#     --instance-ids "$LIVEKIT_INSTANCE_ID"
#
# Requirements: 8.5

set -euo pipefail

SSM_PREFIX="${SSM_PREFIX:-/wavis/dev}"
REGION="${AWS_REGION:-us-east-2}"
REPO_DIR="${HOME}/wavis"
BRANCH="${BRANCH:-dev}"
REPO_URL="${REPO_URL:-git@github.com:example/wavis.git}"
LIVEKIT_COMPOSE_FILE="${LIVEKIT_COMPOSE_FILE:-deploy/docker-compose.livekit.yml}"

echo "=== Wavis LiveKit SSM Deploy ==="
echo "Region: ${REGION}"
echo "Repo:   ${REPO_DIR}"
echo "Branch: ${BRANCH}"
echo "URL:    ${REPO_URL}"
echo "Compose: ${LIVEKIT_COMPOSE_FILE}"

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
if [[ ! -d "$REPO_DIR/.git" ]]; then
  echo "Repository missing — cloning ${REPO_URL}..."
  mkdir -p "$(dirname "$REPO_DIR")"
  git clone "$REPO_URL" "$REPO_DIR"
fi

cd "$REPO_DIR"

echo "Fetching origin..."
git fetch origin

echo "Pulling ${BRANCH} (fast-forward only)..."
git pull --ff-only origin "$BRANCH"

# ---------------------------------------------------------------------------
# 3. Start LiveKit services
# ---------------------------------------------------------------------------
echo "Starting LiveKit Docker Compose services..."
docker compose -f "${LIVEKIT_COMPOSE_FILE}" up -d

# ---------------------------------------------------------------------------
# 4. Health check
# ---------------------------------------------------------------------------
echo "Running LiveKit health check..."
curl --fail --silent --show-error --max-time 10 http://localhost:7880/

echo ""
echo "=== LiveKit deploy complete ==="
