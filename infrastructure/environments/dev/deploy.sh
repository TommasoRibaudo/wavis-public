#!/bin/bash
# Quick deploy: SSH into the existing instance, pull latest, restart services.
# Usage: ./deploy.sh [branch]
set -euo pipefail

BRANCH="${1:-dev}"
KEY="infrastructure/wavis-backend-dev-jey.pem"

# Get instance IP from Terraform output
IP=$(terraform output -raw public_ip 2>/dev/null)
if [ -z "$IP" ]; then
  echo "Error: Could not get public_ip from terraform output. Run 'terraform apply' first."
  exit 1
fi

echo "Deploying branch '$BRANCH' to $IP..."

ssh -i "$KEY" -o StrictHostKeyChecking=accept-new ec2-user@"$IP" <<EOF
  set -e
  cd ~/wavis
  git fetch origin
  git checkout $BRANCH
  git pull --ff-only origin $BRANCH
  docker compose -f docker-compose.yml -f docker-compose.prod.yml up -d --build --wait
  docker compose ps
  curl --fail http://localhost:3000/health && echo " -> healthy"
EOF

echo "Deploy complete."
