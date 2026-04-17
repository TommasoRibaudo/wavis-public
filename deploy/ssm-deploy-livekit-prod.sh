#!/usr/bin/env bash
set -euo pipefail

export SSM_PREFIX="/wavis/prod"
export BRANCH="${BRANCH:-main}"
export LIVEKIT_COMPOSE_FILE="deploy/docker-compose.livekit.yml"

bash "$(dirname "$0")/ssm-deploy-livekit.sh"
