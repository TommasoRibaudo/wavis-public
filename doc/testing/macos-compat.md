# macOS Compatibility Smoke Tests

The local compatibility runner ships an already-built `Wavis.app` to configured
macOS targets over SSH, launches it, captures crash evidence, and writes a
merged report under `compat-results/`.

## Machine Setup

Each target machine needs:

- SSH key-based login for the configured user.
- Remote Login enabled in macOS Sharing settings.
- `codesign`, `log`, `sw_vers`, `sysctl`, `pgrep`, and `osascript` available.
- A stable LAN hostname or IP address.
- One-time manual approval of first-launch prompts for the Wavis bundle you are
  testing, when macOS shows them.

Copy the sample inventory and edit it for your lab:

```sh
cp tools/compat/machines.example.toml tools/compat/machines.local.toml
```

`machines.local.toml` is gitignored because it stores local addresses and SSH
key paths.

## TCC Permissions

Before running media-sensitive tiers on a physical Mac, grant permissions for
the Wavis bundle identifier `com.wavis.desktop`:

- Microphone
- Screen Recording
- System Audio Recording, on macOS versions that expose that permission

Use System Settings, then launch Wavis once as the same user that SSH will use.
Virtual machines can be unreliable for microphone and screen permissions; treat
VM media failures as environment evidence until confirmed on physical hardware.

Tier 1 only launches the app, but granting permissions up front avoids
ambiguous prompts during later media-focused tiers.

Tier 2 uses a debug-only Tauri compatibility command. Build a debug macOS app
when running `--tiers t2`; release builds intentionally reject the probe command.
Tier 3 extends the same debug probe with macOS media capability status and a
best-effort TCC database dump.

## Running

Build a macOS app first, then run from the repository root:

```sh
python tools/compat/compat-run.py --app path/to/Wavis.app
```

Run one target:

```sh
python tools/compat/compat-run.py --app path/to/Wavis.app --machine mac-bigsur-intel
```

Run the IPC bridge probe:

```sh
python tools/compat/compat-run.py --app path/to/Wavis.app --machine mac-bigsur-intel --tiers t0,t1,t2
```

Run the media capability probe and TCC dump:

```sh
python tools/compat/compat-run.py --app path/to/Wavis.app --machine mac-bigsur-intel --tiers t0,t1,t2,t3
```

Run without connecting to machines:

```sh
python tools/compat/compat-run.py --app path/to/Wavis.app --dry-run
```

## Manual GitHub Workflow

`.github/workflows/macos-compat.yml` is manual-only and must be dispatched from
a release tag. It runs on one self-hosted macOS runner, builds or uses a
prebuilt `Wavis.app`, and executes only Tier 0 + Tier 1:

```sh
python3 tools/compat/compat-run.py --app "$APP_PATH" --config tools/compat/machines.local.toml --tiers t0,t1
```

The self-hosted runner must have `tools/compat/machines.local.toml` in the
checked-out workspace before the compatibility step runs. The workflow checkout
uses `clean: false` so that local, gitignored inventory file is not removed.

The runner writes:

- `compat-results/<run-id>/_local/` for Tier 0 package validation.
- `compat-results/<run-id>/<machine>/` for Tier 1 remote logs, Tier 2/Tier 3 IPC output, TCC state, and crash reports.
- `compat-results/<run-id>/compat-report.json` for structured results.
- `compat-results/<run-id>/compat-report.md` for a short human summary.

On non-macOS controller machines, Tier 0 records clear warnings when `otool` or
`codesign` are unavailable. Run Tier 0 on macOS before a release when deployment
target and entitlement validation must be authoritative.
