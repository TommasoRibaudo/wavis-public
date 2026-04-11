# macOS Distribution Guide

How to build, sign, notarize, and distribute Wavis for macOS.

---

## Installing on Another Mac

After sending `Wavis.zip`:

1. Unzip it
2. Drag `Wavis.app` into `/Applications` — this step is required
3. Open it from `/Applications`

On first launch, macOS will ask for microphone access and keychain access — both must be allowed for the app to work.

Do NOT open the app directly from the zip or from Downloads/Desktop. macOS will translocate it (run it from a sandboxed temp path), which prevents the WebView from loading and the permission prompts from appearing, resulting in a blank white window.

---



- macOS machine with Xcode Command Line Tools installed
- Apple Developer Program membership
- A Developer ID Application certificate installed in Keychain (with private key)
- An app-specific password from appleid.apple.com

---

## One-time Setup

### 1. Get a Developer ID Application certificate

You need the certificate AND its private key in your Keychain. There are two ways:

**If the cert was created on another Mac:**
1. On that Mac: open Keychain Access → find "Developer ID Application: ..." → right-click → Export → save as `.p12` with a password
2. Transfer the `.p12` to this Mac → double-click → enter password to import

**If starting fresh:**
1. Open Keychain Access → menu: Keychain Access → Certificate Assistant → Request a Certificate from a Certificate Authority
2. Enter your Apple ID email, leave CA email blank, select "Saved to disk" → save the `.certSigningRequest`
3. Go to [developer.apple.com/account/resources/certificates](https://developer.apple.com/account/resources/certificates)
4. Click `+` → choose "Developer ID Application" → upload the `.certSigningRequest`
5. Download the `.cer` → double-click to install

Verify it worked:
```bash
security find-identity -v -p codesigning
```
You should see: `Developer ID Application: Your Name (TEAMID)`

### 2. Get an app-specific password

1. Go to [appleid.apple.com](https://appleid.apple.com) → Sign-In and Security → App-Specific Passwords
2. Click `+` → name it "Wavis Notarization" → copy the generated password (`xxxx-xxxx-xxxx-xxxx`)

### 3. Store notarization credentials in Keychain

Run this once — it saves credentials under the profile name `wavis-notary` so they never need to be in env vars or files:

```bash
xcrun notarytool store-credentials "wavis-notary" \
  --apple-id "your@email.com" \
  --team-id "YOUR_TEAM_ID" \
  --password "xxxx-xxxx-xxxx-xxxx"
```

Your Team ID is the 10-character string in parentheses from the `security find-identity` output above.

Verify credentials work:
```bash
xcrun notarytool history --keychain-profile "wavis-notary"
```

---

## Building a Release

From `clients/wavis-gui/`:

```bash
npm run build:mac
```

This script (`scripts/build-mac.sh`) runs the full pipeline:

1. Verifies the signing identity exists in Keychain
2. Builds the app with `tauri build` using `tauri.macos.conf.json` (which sets the signing identity)
3. Verifies the code signature
4. Zips the `.app` for submission
5. Submits to Apple's notary service and waits for `Accepted`
6. Staples the notarization ticket to the `.app`
7. Runs a final Gatekeeper check (`spctl`) — must show `accepted` + `source=Notarized Developer ID`

Output app: `target/release/bundle/macos/Wavis.app`

---

## Signing Configuration

The signing identity is set in `src-tauri/tauri.macos.conf.json`:

```json
{
  "bundle": {
    "macOS": {
      "signingIdentity": "Developer ID Application: Santiago Alfonso Pineda (9459534V77)",
      "minimumSystemVersion": "10.15",
      "entitlements": "Entitlements.plist"
    }
  }
}
```

If the certificate is ever reissued (e.g. it expires — Developer ID certs last 5 years), update the `signingIdentity` string here to match the new cert's exact name from `security find-identity`.

Entitlements are in `src-tauri/Entitlements.plist`:
- `com.apple.security.cs.allow-jit` — required for WebKit JIT
- `com.apple.security.device.audio-input` — microphone access
- `com.apple.security.cs.disable-library-validation` — required for system audio capture via Core Audio process tap

---

## Troubleshooting

**`0 valid identities found`**
The certificate is missing its private key. Either import the `.p12` from the Mac where the CSR was generated, or revoke and reissue the cert (generating the CSR on this machine).

**Notarization returns `Invalid`**
Run the log command to see why:
```bash
xcrun notarytool log <REQUEST_ID> --keychain-profile "wavis-notary"
```
Common causes: missing entitlements, unsigned binaries inside the bundle, or hardened runtime not enabled.

**`spctl` shows `rejected` instead of `accepted`**
The staple step may have failed, or the app was modified after notarization. Rebuild from scratch.

**Apple notary service returns HTTP 500**
Apple-side outage. Wait a few minutes and retry.

**DMG bundling fails during `tauri build`**
Not a blocker — the `.app` is still built and signed. The `build:mac` script works directly with the `.app`, not the DMG.

---

## Certificate Expiry

Developer ID Application certificates are valid for 5 years. When it expires:
1. Revoke the old cert at [developer.apple.com](https://developer.apple.com/account/resources/certificates)
2. Generate a new CSR on this Mac and create a new cert (same process as initial setup)
3. Update `signingIdentity` in `src-tauri/tauri.macos.conf.json`
4. Re-run `xcrun notarytool store-credentials` with the new team ID if it changed
