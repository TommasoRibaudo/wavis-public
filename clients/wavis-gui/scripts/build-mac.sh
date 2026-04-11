#!/usr/bin/env bash
# Full macOS release build: sign → notarize → staple → verify
# Usage: ./scripts/build-mac.sh
# Requires: "wavis-notary" keychain profile (see doc/macos-distribution.md)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GUI_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
APP_PATH="$(cd "$GUI_DIR/../.." && pwd)/target/release/bundle/macos/Wavis.app"
ZIP_PATH="$(cd "$GUI_DIR/../.." && pwd)/target/release/bundle/macos/Wavis-notarize.zip"
KEYCHAIN_PROFILE="wavis-notary"
MACOS_CONF="src-tauri/tauri.macos.conf.json"

echo "==> Working directory: $GUI_DIR"
cd "$GUI_DIR"

# ── 1. Verify signing identity ────────────────────────────────────────────────
echo ""
echo "==> [1/5] Verifying signing identity..."
IDENTITY=$(security find-identity -v -p codesigning | grep "Developer ID Application" | head -1 | sed 's/.*"\(.*\)".*/\1/')
if [ -z "$IDENTITY" ]; then
  echo "ERROR: No valid Developer ID Application identity found in Keychain."
  echo "       See doc/macos-distribution.md for setup instructions."
  exit 1
fi
echo "    Identity: $IDENTITY"

# ── 2. Build + sign ───────────────────────────────────────────────────────────
echo ""
echo "==> [2/5] Building and signing app (this takes a few minutes)..."
npm run patch:livekit
NODE_ENV=production npm run tauri build -- --config "$MACOS_CONF"

if [ ! -d "$APP_PATH" ]; then
  echo "ERROR: Expected app not found at: $APP_PATH"
  exit 1
fi
echo "    App built at: $APP_PATH"

# Verify signature before notarizing
codesign --verify --deep --strict --verbose=2 "$APP_PATH"
echo "    Signature verified."

# ── 3. Zip for notarization ───────────────────────────────────────────────────
echo ""
echo "==> [3/6] Zipping app for notarization..."
rm -f "$ZIP_PATH"
ditto -c -k --keepParent "$APP_PATH" "$ZIP_PATH"
echo "    Zip: $ZIP_PATH"

# ── 4. Notarize ───────────────────────────────────────────────────────────────
echo ""
echo "==> [4/6] Submitting to Apple notary service (may take 1-5 min)..."
NOTARY_OUTPUT=$(xcrun notarytool submit "$ZIP_PATH" \
  --keychain-profile "$KEYCHAIN_PROFILE" \
  --wait 2>&1)
echo "$NOTARY_OUTPUT"

if ! echo "$NOTARY_OUTPUT" | grep -q "status: Accepted"; then
  echo ""
  echo "ERROR: Notarization was not accepted."
  REQUEST_ID=$(echo "$NOTARY_OUTPUT" | grep "^id:" | head -1 | awk '{print $2}')
  if [ -n "$REQUEST_ID" ]; then
    echo "       Fetching notarization log for request: $REQUEST_ID"
    xcrun notarytool log "$REQUEST_ID" --keychain-profile "$KEYCHAIN_PROFILE"
  fi
  exit 1
fi

# ── 5. Staple + verify app ────────────────────────────────────────────────────
echo ""
echo "==> [5/6] Stapling and verifying app..."
xcrun stapler staple "$APP_PATH"
xcrun stapler validate "$APP_PATH"
spctl -a -vvv "$APP_PATH"

if ! spctl -a -vvv "$APP_PATH" 2>&1 | grep -q "accepted"; then
  echo "ERROR: Gatekeeper check failed on .app"
  exit 1
fi

# ── 6. Build DMG from notarized app, notarize, and staple ────────────────────
echo ""
echo "==> [6/6] Building DMG from notarized app..."
DMG_DIR="$(cd "$GUI_DIR/../.." && pwd)/target/release/bundle/dmg"
mkdir -p "$DMG_DIR"
VERSION=$(node -p "require('./package.json').version")
DMG_PATH="$DMG_DIR/Wavis_${VERSION}_aarch64.dmg"

# Build DMG directly from the already-notarized .app — no recompile.
# A staging folder with an /Applications symlink gives users the standard
# drag-to-install experience.
STAGING=$(mktemp -d)
cp -R "$APP_PATH" "$STAGING/"
ln -s /Applications "$STAGING/Applications"
hdiutil create -volname "Wavis" \
  -srcfolder "$STAGING" \
  -ov -format UDZO \
  "$DMG_PATH"
rm -rf "$STAGING"
echo "    DMG: $DMG_PATH"

echo "    Notarizing DMG..."
DMG_NOTARY=$(xcrun notarytool submit "$DMG_PATH" \
  --keychain-profile "$KEYCHAIN_PROFILE" \
  --wait 2>&1)
echo "$DMG_NOTARY"

if ! echo "$DMG_NOTARY" | grep -q "status: Accepted"; then
  REQUEST_ID=$(echo "$DMG_NOTARY" | grep "^id:" | head -1 | awk '{print $2}')
  if [ -n "$REQUEST_ID" ]; then
    echo "       Fetching notarization log for request: $REQUEST_ID"
    xcrun notarytool log "$REQUEST_ID" --keychain-profile "$KEYCHAIN_PROFILE"
  fi
  echo "ERROR: DMG notarization failed."
  exit 1
fi

xcrun stapler staple "$DMG_PATH"
xcrun stapler validate "$DMG_PATH"
echo "    DMG notarized and stapled."

echo ""
echo "✓ Done. Distribute the DMG — it contains the signed, notarized, stapled app."
echo "  App: $APP_PATH"
echo "  DMG: $DMG_PATH"
