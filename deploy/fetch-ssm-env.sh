#!/usr/bin/env bash
# fetch-ssm-env.sh — Pull secrets from SSM Parameter Store into a .env file.
# Runs on the EC2 self-hosted runner during deploy.
#
# Usage: ./deploy/fetch-ssm-env.sh [output_path]
#   output_path  Path for the generated .env file (default: .env)

set -euo pipefail

SSM_PREFIX="/wavis/dev"
REGION="${AWS_REGION:-us-east-2}"
OUTPUT="${1:-.env}"

echo "Fetching SSM parameters under ${SSM_PREFIX}/ ..."

raw=$(aws ssm get-parameters-by-path \
  --region "$REGION" \
  --path "$SSM_PREFIX/" \
  --with-decryption \
  --recursive \
  --query "Parameters[*].[Name,Value]" \
  --output text)

if [[ -z "$raw" ]]; then
  echo "ERROR: No parameters found under ${SSM_PREFIX}/" >&2
  exit 1
fi

# Strip the SSM prefix to produce clean KEY=value lines.
env_lines=""
placeholder_hits=""

while IFS=$'\t' read -r name value; do
  key="${name#${SSM_PREFIX}/}"

  if [[ "$value" == *"CHANGE-ME"* ]]; then
    placeholder_hits+="  ${key}"$'\n'
  fi

  env_lines+="${key}=${value}"$'\n'
done <<< "$raw"

# Fail fast if any un-rotated placeholders remain.
if [[ -n "$placeholder_hits" ]]; then
  echo "ERROR: The following parameters still contain CHANGE-ME placeholders:" >&2
  echo "$placeholder_hits" >&2
  echo "Rotate them in SSM before deploying." >&2
  exit 1
fi

# Write the .env file with restricted permissions.
install -m 600 /dev/null "$OUTPUT"
printf '%s' "$env_lines" > "$OUTPUT"

count=$(echo "$raw" | wc -l)
echo "Wrote ${count} parameter(s) to ${OUTPUT} (mode 600)."
