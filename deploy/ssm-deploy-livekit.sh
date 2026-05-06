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

# Paths accumulated for cleanup on exit (SSH key, PAT credential file, rendered
# config temp file). A single trap handles all of them so later additions don't
# clobber earlier ones.
CLEANUP_PATHS=()
cleanup_temp_files() {
  local p
  for p in "${CLEANUP_PATHS[@]:-}"; do
    [[ -n "$p" ]] && rm -f "$p"
  done
}
trap cleanup_temp_files EXIT

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
  CLEANUP_PATHS+=("$SSH_KEY_FILE")

  printf '%s\n' "$DEPLOY_KEY" > "$SSH_KEY_FILE"
  chmod 600 "$SSH_KEY_FILE"

  export GIT_SSH_COMMAND="ssh -i ${SSH_KEY_FILE} -o StrictHostKeyChecking=accept-new"
else
  echo "Detected PAT — configuring git credential helper..."

  CRED_FILE=$(mktemp)
  CLEANUP_PATHS+=("$CRED_FILE")

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

# Reset any on-host hotfix edits to deploy files so git pull is always fast-forward.
# Operators sometimes patch compose/template files in place during incidents; if
# those edits linger, the next deploy fails with "Your local changes ... would be
# overwritten". We intentionally narrow the scope to deploy/ to avoid clobbering
# unrelated local state.
if ! git diff --quiet -- deploy/; then
  echo "Reverting local modifications under deploy/ (hotfixes are not persisted)..."
  git checkout -- deploy/
fi

echo "Pulling ${BRANCH} (fast-forward only)..."
git pull --ff-only origin "$BRANCH"

# ---------------------------------------------------------------------------
# 3. Render LiveKit config from template + SSM secrets
# ---------------------------------------------------------------------------
# The compose file mounts /etc/wavis/livekit.rendered.yaml as the LiveKit config.
# Render it here (idempotent, survives reboots) instead of doing runtime
# substitution inside the container. This avoids the legacy sed-in-entrypoint
# pattern and its brittle bind-path resolution.
LIVEKIT_CONFIG_TPL="${REPO_DIR}/deploy/livekit.prod.yaml"
LIVEKIT_CONFIG_DIR="/etc/wavis"
LIVEKIT_CONFIG_RENDERED="${LIVEKIT_CONFIG_DIR}/livekit.rendered.yaml"

if [[ ! -f "$LIVEKIT_CONFIG_TPL" ]]; then
  echo "ERROR: template missing at ${LIVEKIT_CONFIG_TPL}" >&2
  exit 1
fi

echo "Fetching LiveKit secrets from SSM..."
LIVEKIT_API_KEY=$(aws ssm get-parameter \
  --name "${SSM_PREFIX}/LIVEKIT_API_KEY" \
  --with-decryption --region "$REGION" \
  --query "Parameter.Value" --output text)
LIVEKIT_API_SECRET=$(aws ssm get-parameter \
  --name "${SSM_PREFIX}/LIVEKIT_API_SECRET" \
  --with-decryption --region "$REGION" \
  --query "Parameter.Value" --output text)

if [[ -z "${LIVEKIT_API_KEY}" || "${LIVEKIT_API_KEY}" == *"CHANGE-ME"* ]]; then
  echo "ERROR: ${SSM_PREFIX}/LIVEKIT_API_KEY is missing or a placeholder." >&2
  exit 1
fi
if [[ -z "${LIVEKIT_API_SECRET}" || "${LIVEKIT_API_SECRET}" == *"CHANGE-ME"* ]]; then
  echo "ERROR: ${SSM_PREFIX}/LIVEKIT_API_SECRET is missing or a placeholder." >&2
  exit 1
fi

echo "Rendering ${LIVEKIT_CONFIG_RENDERED}..."
sudo mkdir -p "$LIVEKIT_CONFIG_DIR"
# Write to a temp file first so a failed sed never leaves a half-rendered config
# behind. Use env vars instead of sed -i to avoid shell-metachar injection from
# secret values (anchors, pipes, etc.).
RENDER_TMP=$(mktemp)
CLEANUP_PATHS+=("$RENDER_TMP")

# `|` is not a legal character in base64 or hex-encoded LiveKit secrets, so it's
# safe to use as the sed delimiter here. The secret never touches argv — we pass
# it through the environment.
LIVEKIT_API_KEY="$LIVEKIT_API_KEY" \
LIVEKIT_API_SECRET="$LIVEKIT_API_SECRET" \
  sed \
    -e "s|LIVEKIT_API_KEY_PLACEHOLDER|${LIVEKIT_API_KEY}|g" \
    -e "s|LIVEKIT_API_SECRET_PLACEHOLDER|${LIVEKIT_API_SECRET}|g" \
    "$LIVEKIT_CONFIG_TPL" > "$RENDER_TMP"

if grep -q "LIVEKIT_API_KEY_PLACEHOLDER\|LIVEKIT_API_SECRET_PLACEHOLDER" "$RENDER_TMP"; then
  echo "ERROR: template substitution left placeholders behind." >&2
  exit 1
fi

sudo install -o root -g root -m 0640 "$RENDER_TMP" "$LIVEKIT_CONFIG_RENDERED"
echo "Rendered config at ${LIVEKIT_CONFIG_RENDERED} ($(sudo stat -c %s "$LIVEKIT_CONFIG_RENDERED") bytes)."

# ---------------------------------------------------------------------------
# 4. Start LiveKit services
# ---------------------------------------------------------------------------
echo "Starting LiveKit Docker Compose services..."
docker compose -f "${LIVEKIT_COMPOSE_FILE}" up -d

# ---------------------------------------------------------------------------
# 5. Health check
# ---------------------------------------------------------------------------
echo "Running LiveKit health check..."
curl --fail --silent --show-error --max-time 10 http://localhost:7880/

echo ""
echo "=== LiveKit deploy complete ==="
