#!/usr/bin/env bash
# update-latest-json.sh — Merge a platform entry into latest.json and upload to GitHub Release.
#
# Usage: ./scripts/update-latest-json.sh <tag> <version> <platform_key> <artifact_url> <sig_file>
#
# Example:
#   ./scripts/update-latest-json.sh desktop-v0.1.1 0.1.1 linux-x86_64 \
#     "https://github.com/user/repo/releases/download/desktop-v0.1.1/Wavis_0.1.1_amd64.AppImage.tar.gz" \
#     target/release/bundle/appimage/Wavis_0.1.1_amd64.AppImage.tar.gz.sig
#
# Requires: gh (authenticated), jq
set -euo pipefail

TAG="$1"
VERSION="$2"
PLATFORM_KEY="$3"
ARTIFACT_URL="$4"
SIG_FILE="$5"

SIGNATURE=$(cat "$SIG_FILE")
PUB_DATE=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
REPO="${GITHUB_REPOSITORY:-TommasoRibaudo/wavis-public}"

# Try to download existing latest.json from the release (may not exist yet)
EXISTING="{}"
if gh release download "$TAG" --pattern "latest.json" --dir /tmp --repo "$REPO" --clobber 2>/dev/null; then
  EXISTING=$(cat /tmp/latest.json)
fi

# Merge this platform into the existing JSON
UPDATED=$(echo "$EXISTING" | jq \
  --arg version "$VERSION" \
  --arg pub_date "$PUB_DATE" \
  --arg platform "$PLATFORM_KEY" \
  --arg url "$ARTIFACT_URL" \
  --arg sig "$SIGNATURE" \
  '{
    version: $version,
    notes: (.notes // ""),
    pub_date: $pub_date,
    platforms: ((.platforms // {}) + {($platform): {url: $url, signature: $sig}})
  }')

echo "$UPDATED" > /tmp/latest.json
echo "Generated latest.json with platform $PLATFORM_KEY:"
cat /tmp/latest.json

gh release upload "$TAG" /tmp/latest.json --repo "$REPO" --clobber
