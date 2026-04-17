<#
.SYNOPSIS
    Syncs files from the main Wavis workspace into the public macOS build repo.

.DESCRIPTION
    Copies the minimal set of crates and frontend files needed to build
    wavis-gui on macOS. Run from the workspace root.

    The public repo layout mirrors the workspace paths so that relative
    Cargo path dependencies (../../shared, etc.) resolve without edits.

.PARAMETER DestDir
    Path to the public repo checkout. Created if it doesn't exist.

.EXAMPLE
    .\scripts\sync-public-repo.ps1 -DestDir ..\wavis-app
#>
param(
    [Parameter(Mandatory)]
    [string]$DestDir
)

$ErrorActionPreference = 'Stop'

# Resolve to absolute path
$DestDir = [System.IO.Path]::GetFullPath($DestDir)

Write-Host "═══ Syncing to $DestDir ═══" -ForegroundColor Cyan

# ─── Helper: mirror a directory (robocopy-style, delete extras) ───
function Sync-Dir {
    param([string]$Src, [string]$Dst, [string[]]$Exclude = @())

    if (-not (Test-Path $Src)) {
        Write-Warning "Source not found, skipping: $Src"
        return
    }

    # Build exclusion filter
    $excludeSet = [System.Collections.Generic.HashSet[string]]::new(
        [System.StringComparer]::OrdinalIgnoreCase
    )
    foreach ($e in $Exclude) { [void]$excludeSet.Add($e) }

    # Ensure destination exists
    if (-not (Test-Path $Dst)) {
        New-Item -ItemType Directory -Path $Dst -Force | Out-Null
    }

    # Copy files
    $srcFull = (Resolve-Path $Src).Path
    Get-ChildItem -Path $Src -Recurse -File | ForEach-Object {
        $relPath = $_.FullName.Substring($srcFull.Length + 1)
        $skip = $false
        foreach ($e in $Exclude) {
            if ($relPath -like $e) { $skip = $true; break }
        }
        if (-not $skip) {
            $destFile = Join-Path $Dst $relPath
            $destDir2 = Split-Path $destFile -Parent
            if (-not (Test-Path $destDir2)) {
                New-Item -ItemType Directory -Path $destDir2 -Force | Out-Null
            }
            Copy-Item $_.FullName $destFile -Force
        }
    }
}

# ─── 1. shared/ (signaling types crate) ──────────────────────────
Write-Host "  shared/" -ForegroundColor Yellow
Sync-Dir "shared/src" "$DestDir/shared/src"
Copy-Item "shared/Cargo.toml" "$DestDir/shared/Cargo.toml" -Force

# ─── 2. clients/shared/ (client shared crate) ────────────────────
Write-Host "  clients/shared/" -ForegroundColor Yellow
Sync-Dir "clients/shared/src" "$DestDir/clients/shared/src"
Copy-Item "clients/shared/Cargo.toml" "$DestDir/clients/shared/Cargo.toml" -Force

# ─── 3. clients/wavis-gui/ (frontend + src-tauri) ────────────────
Write-Host "  clients/wavis-gui/src/" -ForegroundColor Yellow
Sync-Dir "clients/wavis-gui/src" "$DestDir/clients/wavis-gui/src" @("**/__tests__/*", "*.test.ts")

Write-Host "  clients/wavis-gui/src-tauri/" -ForegroundColor Yellow
Sync-Dir "clients/wavis-gui/src-tauri/src" "$DestDir/clients/wavis-gui/src-tauri/src"
Sync-Dir "clients/wavis-gui/src-tauri/icons" "$DestDir/clients/wavis-gui/src-tauri/icons"
Sync-Dir "clients/wavis-gui/src-tauri/capabilities" "$DestDir/clients/wavis-gui/src-tauri/capabilities"
Copy-Item "clients/wavis-gui/src-tauri/Cargo.toml" "$DestDir/clients/wavis-gui/src-tauri/Cargo.toml" -Force
Copy-Item "clients/wavis-gui/src-tauri/build.rs" "$DestDir/clients/wavis-gui/src-tauri/build.rs" -Force
Copy-Item "clients/wavis-gui/src-tauri/tauri.conf.json" "$DestDir/clients/wavis-gui/src-tauri/tauri.conf.json" -Force
Copy-Item "clients/wavis-gui/src-tauri/Info.plist" "$DestDir/clients/wavis-gui/src-tauri/Info.plist" -Force

# Top-level GUI config files
Write-Host "  clients/wavis-gui/ config files" -ForegroundColor Yellow
$guiConfigs = @(
    "package.json", "package-lock.json", "tsconfig.json",
    "vite.config.ts", "index.html", "app-icon.png", ".env.example"
)
foreach ($f in $guiConfigs) {
    $src = "clients/wavis-gui/$f"
    if (Test-Path $src) {
        Copy-Item $src "$DestDir/clients/wavis-gui/$f" -Force
    }
}

# ─── 4. Workspace Cargo.toml (trimmed to GUI-only members) ───────
Write-Host "  Cargo.toml (workspace)" -ForegroundColor Yellow
$workspaceToml = @'
[workspace]
resolver = "3"
members = [
    "shared",
    "clients/shared",
    "clients/wavis-gui/src-tauri",
]
'@
[System.IO.File]::WriteAllText("$DestDir/Cargo.toml", $workspaceToml + "`n")

# ─── 5. Cargo.lock (for reproducible builds) ─────────────────────
Write-Host "  Cargo.lock" -ForegroundColor Yellow
Copy-Item "Cargo.lock" "$DestDir/Cargo.lock" -Force

# ─── 6. .gitignore ───────────────────────────────────────────────
Write-Host "  .gitignore" -ForegroundColor Yellow
$gitignore = @'
target/
node_modules/
dist/
.env
.DS_Store
Thumbs.db
*.log
/clients/wavis-gui/src-tauri/gen/
'@
[System.IO.File]::WriteAllText("$DestDir/.gitignore", $gitignore + "`n")

# ─── 7. build.sh (macOS one-command build script) ────────────────
Write-Host "  build.sh" -ForegroundColor Yellow
$buildSh = @'
#!/usr/bin/env bash
set -euo pipefail

APP_NAME="Wavis"

echo ""
echo "  ═══ $APP_NAME — macOS Build ═══"
echo ""

# ─── Xcode CLT ───────────────────────────────────────────────────
if ! xcode-select -p &>/dev/null; then
  echo "Installing Xcode Command Line Tools..."
  xcode-select --install
  echo "Re-run this script after Xcode CLT finishes installing."
  exit 1
fi

# ─── Homebrew ─────────────────────────────────────────────────────
if ! command -v brew &>/dev/null; then
  echo "Installing Homebrew..."
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
  eval "$(/opt/homebrew/bin/brew shellenv 2>/dev/null || /usr/local/bin/brew shellenv)"
fi

# ─── Rust ─────────────────────────────────────────────────────────
if ! command -v rustc &>/dev/null; then
  echo "Installing Rust..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  source "$HOME/.cargo/env"
fi

# ─── Node.js ──────────────────────────────────────────────────────
if ! command -v node &>/dev/null; then
  echo "Installing Node.js via Homebrew..."
  brew install node
fi

# ─── Navigate to repo root ───────────────────────────────────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# ─── Install frontend deps ───────────────────────────────────────
echo "Installing npm dependencies..."
cd clients/wavis-gui
npm ci

# ─── Build ────────────────────────────────────────────────────────
echo ""
echo "Building $APP_NAME (this takes a few minutes on first run)..."
echo ""
npx tauri build 2>&1

# ─── Output ───────────────────────────────────────────────────────
DMG_PATH=$(find src-tauri/target/release/bundle -name "*.dmg" 2>/dev/null | head -1)
APP_PATH=$(find src-tauri/target/release/bundle -name "*.app" -type d 2>/dev/null | head -1)

echo ""
echo "  ═══ Build complete ═══"
echo ""
if [ -n "${DMG_PATH:-}" ]; then
  echo "  DMG: $DMG_PATH"
  echo ""
  echo "  Opening DMG..."
  open "$DMG_PATH"
elif [ -n "${APP_PATH:-}" ]; then
  echo "  App: $APP_PATH"
  echo "  Drag to /Applications or run directly."
  open -R "$APP_PATH"
else
  echo "  Build output: src-tauri/target/release/bundle/"
fi
'@
[System.IO.File]::WriteAllText("$DestDir/build.sh", $buildSh + "`n")

# ─── 8. install.sh (curl|bash one-liner for mac users) ───────────
Write-Host "  install.sh" -ForegroundColor Yellow
$installSh = @'
#!/usr/bin/env bash
set -euo pipefail

APP_NAME="Wavis"
REPO_URL="https://github.com/TommasoRibaudo/wavis-test-client-for-mac.git"
BRANCH="main"
BUILD_DIR="$HOME/wavis-build"

echo ""
echo "  ═══ $APP_NAME — Install ═══"
echo ""

# ─── Xcode CLT ───────────────────────────────────────────────────
if ! xcode-select -p &>/dev/null; then
  echo "Installing Xcode Command Line Tools..."
  xcode-select --install
  echo ""
  echo "  Xcode CLT is installing. When it finishes, re-run:"
  echo "  curl -fsSL https://raw.githubusercontent.com/TommasoRibaudo/wavis-test-client-for-mac/main/install.sh | bash"
  exit 1
fi

# ─── Homebrew ─────────────────────────────────────────────────────
if ! command -v brew &>/dev/null; then
  echo "Installing Homebrew..."
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
  eval "$(/opt/homebrew/bin/brew shellenv 2>/dev/null || /usr/local/bin/brew shellenv)"
fi

# ─── Rust ─────────────────────────────────────────────────────────
if ! command -v rustc &>/dev/null; then
  echo "Installing Rust..."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  source "$HOME/.cargo/env"
fi

# ─── Node.js ──────────────────────────────────────────────────────
if ! command -v node &>/dev/null; then
  echo "Installing Node.js via Homebrew..."
  brew install node
fi

# ─── Clone or update ─────────────────────────────────────────────
if [ -d "$BUILD_DIR/.git" ]; then
  echo "Updating existing source..."
  cd "$BUILD_DIR"
  git fetch origin "$BRANCH"
  git reset --hard "origin/$BRANCH"
else
  echo "Cloning source..."
  rm -rf "$BUILD_DIR"
  git clone --depth 1 --branch "$BRANCH" "$REPO_URL" "$BUILD_DIR"
  cd "$BUILD_DIR"
fi

# ─── Install frontend deps ───────────────────────────────────────
echo "Installing npm dependencies..."
cd clients/wavis-gui
npm ci

# ─── Build ────────────────────────────────────────────────────────
echo ""
echo "Building $APP_NAME (this takes a few minutes on first run)..."
echo ""
npx tauri build 2>&1

# ─── Install to /Applications ────────────────────────────────────
APP_BUNDLE=$(find src-tauri/target/release/bundle -name "*.app" -type d 2>/dev/null | head -1)
DMG_PATH=$(find src-tauri/target/release/bundle -name "*.dmg" 2>/dev/null | head -1)

echo ""
echo "  ═══ Build complete ═══"
echo ""

if [ -n "${APP_BUNDLE:-}" ]; then
  DEST="/Applications/$(basename "$APP_BUNDLE")"
  if [ -d "$DEST" ]; then
    echo "  Removing old $DEST..."
    rm -rf "$DEST"
  fi
  cp -R "$APP_BUNDLE" /Applications/
  echo "  Installed to $DEST"
  echo ""
  echo "  Launching $APP_NAME..."
  open "$DEST"
elif [ -n "${DMG_PATH:-}" ]; then
  echo "  DMG: $DMG_PATH"
  open "$DMG_PATH"
else
  echo "  Build output: src-tauri/target/release/bundle/"
fi

echo ""
echo "  To update later, just re-run this same command."
echo ""
'@
[System.IO.File]::WriteAllText("$DestDir/install.sh", $installSh + "`n")

# ─── 9. README.md ────────────────────────────────────────────────
Write-Host "  README.md" -ForegroundColor Yellow
$readme = @'
# Wavis

Native voice chat for small private groups. This repo contains the macOS desktop client.

## Install (one command)

Open Terminal and paste:

```bash
curl -fsSL https://raw.githubusercontent.com/TommasoRibaudo/wavis-test-client-for-mac/main/install.sh | bash
```

This installs prerequisites (Xcode CLT, Homebrew, Rust, Node.js) if missing,
clones the repo, builds the app, and copies it to `/Applications`.

To update, run the same command again.

## Manual Build

If you prefer to do it yourself:

```bash
git clone https://github.com/TommasoRibaudo/wavis-test-client-for-mac.git
cd wavis-test-client-for-mac/clients/wavis-gui
npm ci
npx tauri build
```

The `.app` bundle will be in `src-tauri/target/release/bundle/macos/`.

## Notes

- The app is unsigned (no Apple Developer certificate). Since you built it locally,
  macOS Gatekeeper allows it to run without issues.
- If you copy the `.app` to another Mac, right-click → Open on first launch
  (or run `xattr -cr Wavis.app`) to clear the quarantine flag.
- Build files are cached in `~/wavis-build/`. Delete it to reclaim disk space.

## License

MIT
'@
[System.IO.File]::WriteAllText("$DestDir/README.md", $readme + "`n")

# ─── Done ─────────────────────────────────────────────────────────
Write-Host ""
Write-Host "═══ Sync complete ═══" -ForegroundColor Green
Write-Host "  Public repo: $DestDir"
Write-Host "  Next: cd $DestDir; git add -A; git commit -m 'sync from main repo'"
Write-Host ""
