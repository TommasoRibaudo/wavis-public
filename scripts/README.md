# Scripts

Development and build utility scripts. Run from the workspace root.

## sync-public-repo.ps1

Syncs the minimal file set needed to build the GUI client into a separate public repo. Mac users can then clone that repo and build locally without an Apple Developer account (locally-built apps bypass Gatekeeper).

The public repo includes an `install.sh` that mac users run via:

```
curl -fsSL https://raw.githubusercontent.com/TommasoRibaudo/wavis-test-client-for-mac/main/install.sh | bash
```

That one command installs prerequisites (Xcode CLT, Homebrew, Rust, Node.js), clones the repo, builds the Tauri app, and copies it to `/Applications`.

### Usage

```powershell
.\scripts\sync-public-repo.ps1 -DestDir ..\wavis-app
```

### What it copies

| Source | Purpose |
|--------|---------|
| `shared/` | Signaling types crate |
| `clients/shared/` | Client shared crate (audio, WebRTC, LiveKit) |
| `clients/wavis-gui/` | Frontend (React + Vite) and Tauri backend (excludes tests) |
| `Cargo.lock` | Reproducible Rust dependency versions |

It also generates in the target directory:
- `Cargo.toml` — trimmed workspace with only the 3 required members
- `.gitignore`
- `build.sh` — local build script (for users who already cloned)
- `install.sh` — curl-pipe-bash installer (clone + build + install to /Applications)
- `README.md`

### Workflow

1. Make changes in the main repo
2. Run the sync script
3. `cd` into the public repo, commit, push

```powershell
.\scripts\sync-public-repo.ps1 -DestDir ..\wavis-app
cd ..\wavis-app
git add -A
git commit -m "sync: <description>"
git push
```

## ws-test.ps1

Interactive PowerShell WebSocket client for testing signaling. Connects, lets you type JSON messages, and prints responses.

```powershell
.\scripts\ws-test.ps1                              # default: ws://localhost:3000/ws
.\scripts\ws-test.ps1 -Url "ws://myserver:3000/ws"
```

## ws-sfu-test/

Rust-based interactive WebSocket test client (async, tokio). Same idea as `ws-test.ps1` but with better async I/O handling.

```powershell
$env:WS_URL = "ws://localhost:3000/ws"   # optional, defaults to localhost
cargo run -p ws-sfu-test
```
