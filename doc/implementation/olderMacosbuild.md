 Plan: Local macOS Compatibility Test Runner for Wavis Desktop

 Context

 Wavis uses macOS-specific APIs with hard version boundaries:
 - ScreenCaptureKit (objc2-screen-capture-kit in src-tauri/Cargo.toml) — introduced macOS 12.3; absent on 11.5.2     
 - AudioHardwareCreateProcessTap — entitlement in Entitlements.plist; not available on 11.5.2
 - macOSPrivateApi: true in tauri.conf.json — undocumented; unknown stability across versions
 - Declared minimumSystemVersion: 10.15 is aspirational — untested; no macOS build exists in CI (workspace-ci.yml    
 runs on ubuntu-latest only)

 Goal: one local command that ships a built .app to configured macOS targets via SSH, runs tiered smoke tests, and   
 produces a merged diagnostic report.

 ---
 1. Problem Framing

 Why local orchestration first, not CI:
 - macOS runner minutes are expensive; compatibility bugs are one-time discoveries (missing API, SCK absent on       
 11.5.2), not per-commit regressions
 - Team owns a fixed set of physical Macs + VMs — inventory is small; on-demand before releases is the right trigger 

 Questions the system must answer beyond "did it build":
 1. Does the .app launch without crashing on this OS version?
 2. Does Tauri/WKWebView initialize?
 3. Does the IPC bridge respond?
 4. Do OS permission prompts (TCC) appear correctly?
 5. Does audio enumeration succeed?
 6. Does SCK code path crash or degrade gracefully on 11.5.2?
 7. What is the proximate API causing the failure (from logs, not just "crashed")?

 ---
 2. Test Model

 Remote agent via SSH — controller SSHes into each target, SCPs the .app, runs a bash agent script, fetches a JSON   
 result file. No daemon, no persistent installation.

 Machine inventory (tools/compat/machines.toml):

 [[machines]]
 name    = "mac-ventura-intel"
 host    = "192.168.1.42"
 user    = "ci"
 ssh_key = "~/.ssh/compat_rsa"
 arch    = "x86_64"
 macos   = "13.6"           # sw_vers -productVersion value
 notes   = "MacBook Pro 2019, physical"
 tiers   = ["t0", "t1", "t2", "t3"]

 [[machines]]
 name    = "mac-bigsur-intel"
 host    = "192.168.1.55"
 user    = "ci"
 ssh_key = "~/.ssh/compat_rsa"
 arch    = "x86_64"
 macos   = "11.5.2"
 notes   = "VMware Fusion VM, Intel"
 tiers   = ["t0", "t1", "t2"]   # t3 skipped: SCK absent, expected crash

 [[machines]]
 name    = "mac-tahoe-arm"
 host    = "192.168.1.10"
 user    = "dev"
 ssh_key = "~/.ssh/compat_rsa"
 arch    = "arm64"
 macos   = "26.0"               # sw_vers value on Tahoe (marketing: macOS 26)
 notes   = "MacBook Pro M3, baseline reference"
 tiers   = ["t0", "t1", "t2", "t3"]

 Target machine requirements:
 - SSH key-based auth, no password
 - Microphone + screen-recording TCC pre-granted for the ci user (manual once per machine; document in
 doc/testing/macos-compat.md)
 - codesign and log CLI available (standard macOS)

 Note on VMs: Microphone TCC is unreliable in VMs; document that Tier 3 audio tests may report expected failure on   
 VM targets.

 ---
 3. Architecture

 Folder:
 tools/compat/
 ├── compat-run.py            # controller
 ├── machines.example.toml   # committed; machines.local.toml gitignored
 ├── agent/
 │   └── run-agent.sh         # SCPd to target, runs tier checks
 ├── checks/
 │   ├── t0-build.sh          # local: otool, codesign, dylib scan
 │   ├── t1-launch.sh         # remote: launch, crash, quit
 │   ├── t2-ipc.sh            # remote: IPC bridge ping
 │   └── t3-media.sh          # remote: audio enum, SCK probe
 └── report/
     └── merge-report.py      # JSON merge → Markdown summary

 Controller (compat-run.py) responsibilities:
 1. Parse CLI args
 2. Run Tier 0 locally (build artifact validation)
 3. For each target (up to --parallel N, default 1): SCP app + agent, SSH run agent, SCP back results
 4. Merge results and print summary

 SSH/SCP reliability: All SSH/SCP calls use ConnectTimeout=10, ServerAliveInterval=5, ServerAliveCountMax=3. On      
 connection failure the machine is marked "status": "unreachable" in result.json — not a Python exception. Remote    
 commands have a configurable --timeout (default 120s) enforced via SSH timeout wrapper. Machines waking from sleep  
 are handled by a single retry with 5s backoff.

 Parallelism: --parallel N flag (default 1, max 4). At N=3 with a 150MB .app, upstream is ~450MB concurrent — warn   
 in docs. Sequential is safe default.

 Mode B (build-on-target) is CUT from this plan. It requires matching Rust toolchain + Xcode + npm on each target    
 machine, which is a separate setup problem. Add it in a future increment if needed.

 ---
 4. Smoke-Test Tiers

 Tier 0 — Package Validation (local, no SSH)

 - otool -l <binary> → verify LC_BUILD_VERSION deployment target matches tauri.conf.json (10.15)
 - otool -L <binary> → flag any dylib linked against macOS > 10.15
 - codesign --verify --deep Wavis.app (skip if no signing identity; log as warning)
 - codesign --display --entitlements → verify JIT + audio-input + library-validation present

 Collected: otool-l.txt, otool-L.txt, entitlements.xml, build.log

 Tier 1 — App Launch (remote)

 - open -W -a Wavis.app with 10s timeout; capture exit code
 - Check ~/Library/Logs/DiagnosticReports/Wavis*.ips — any crash report created in the window?
 - log show --predicate 'process == "Wavis"' --last 30s captured to file
 - Primary SCK check: if sw_vers < 12.3, app must still launch (SCK code path must not cause dlopen crash)

 Collected: exit code + timing, any .ips crash report, system.log, sw_vers + uname -a + sysctl hw.model

 Tier 2 — Tauri IPC Bridge (remote)

 Design for the IPC test driver:
 Add a __compat_check Tauri command (gated #[cfg(debug_assertions)]) that:
 1. Returns a JSON struct: { "ipc_ok": true, "audio_devices": [...], "store_ok": bool }
 2. Is invoked by a minimal HTML page (compat-probe.html) bundled with the test agent
 3. The agent launches Wavis with WAVIS_COMPAT_PROBE_PATH=/tmp/compat-probe.html env var; the app opens that page in 
  a hidden window, the page calls invoke('__compat_check'), and writes the JSON result to
 /tmp/wavis-compat/ipc-result.json via the shell plugin
 4. Agent polls /tmp/wavis-compat/ipc-result.json with 15s timeout, then quits the app

 This is the most complex tier. Slice 2 implements it. The env-var + file-write approach avoids needing a WebSocket  
 or HTTP server on the agent side.

 Collected: ipc-result.json, Tauri log output, log show for WebKit + com.wavis.desktop subsystem

 Tier 3 — Media / System Integration (remote)

 - Calls __compat_check extended result: SCK enumeration result or graceful-error string, audio process-tap init     
 status
 - On macOS < 12.3: SCK fields must be "skipped" not a crash
 - TCC DB read: sqlite3 "$HOME/Library/Application Support/com.apple.TCC/TCC.db" "SELECT service,client,auth_value   
 FROM access WHERE client='com.wavis.desktop'" → log grant state

 Collected: extended ipc-result.json, TCC dump, log show filtered to com.apple.ScreenCapture + com.apple.coreaudio   

 ---
 5. Diagnostic Outputs

 Per-machine bundle at compat-results/<machine-name>/:

 ┌────────────────────────────────────────────┬───────┬─────────────────────────────────────────────────────┐        
 │                    File                    │ Tier  │                       Purpose                       │        
 ├────────────────────────────────────────────┼───────┼─────────────────────────────────────────────────────┤        
 │ result.json                                │ All   │ Structured pass/fail                                │        
 ├────────────────────────────────────────────┼───────┼─────────────────────────────────────────────────────┤        
 │ otool-l.txt, otool-L.txt, entitlements.xml │ T0    │ Build artifact static checks                        │        
 ├────────────────────────────────────────────┼───────┼─────────────────────────────────────────────────────┤        
 │ crash-reports/*.ips                        │ T1    │ Any crash report created during test                │        
 ├────────────────────────────────────────────┼───────┼─────────────────────────────────────────────────────┤        
 │ system.log                                 │ T1-T3 │ log show filtered to Wavis + WebKit + ScreenCapture │        
 ├────────────────────────────────────────────┼───────┼─────────────────────────────────────────────────────┤        
 │ machine-info.json                          │ All   │ sw_vers, uname -a, sysctl hw.model                  │        
 ├────────────────────────────────────────────┼───────┼─────────────────────────────────────────────────────┤        
 │ ipc-result.json                            │ T2-T3 │ IPC bridge + audio + SCK probe                      │        
 ├────────────────────────────────────────────┼───────┼─────────────────────────────────────────────────────┤        
 │ tcc-dump.txt                               │ T3    │ Wavis TCC grant state                               │        
 └────────────────────────────────────────────┴───────┴─────────────────────────────────────────────────────┘        

 Merged report (compat-report-<timestamp>.json):
 {
   "generated_at": "...", "app_sha": "...",
   "machines": [
     { "name": "mac-bigsur-intel", "macos": "11.5.2", "arch": "x86_64",
       "tiers": { "t0": { "pass": true }, "t1": { "pass": false, "notes": ["SCK dlopen crash — see
 crash-reports/Wavis_2026-04-10.ips"] } },
       "log_bundle": "compat-results/mac-bigsur-intel/" }
   ],
   "summary": { "total": 3, "passed_all_tiers": 2, "failed": 1, "failure_machines": ["mac-bigsur-intel"] }
 }

 Failures surface the crash frame + relevant log lines in the Markdown summary so the developer can act without      
 opening raw logs first.

 ---
 6. Rollout Plan

 Slice 1 (implement first):
 - tools/compat/compat-run.py — SSH orchestrator, Tier 0 local + Tier 1 remote, --parallel N, timeout/retry handling 
 - tools/compat/machines.example.toml + .gitignore entry for machines.local.toml
 - tools/compat/agent/run-agent.sh — Tier 1 checks + JSON result output
 - tools/compat/report/merge-report.py — console summary + JSON
 - doc/testing/macos-compat.md — TCC pre-grant runbook

 Value: catches Tier 1 launch crashes immediately (the current 11.5.2 failure) with zero Tauri code changes.

 Slice 2: __compat_check Tauri command + compat-probe.html + Tier 2 check script

 Slice 3: Extended __compat_check with SCK/audio probe + Tier 3 check script + TCC dump

 Slice 4 (optional, later): workflow_dispatch-only GitHub Actions workflow on one self-hosted macOS runner, Tier 0 + 
  Tier 1 only, triggered manually on release tags — not a PR gate

 Remains manual:
 - TCC grant setup per machine
 - First-launch permission dialogs on new machines
 - Intel-vs-ARM behavioral comparison (documented, not automated)

 ---
 7. Risks and Unknowns

 ┌────────────────────────────────────────────────────┬─────────────────────────────────────────────────────────┐    
 │                        Risk                        │                       Mitigation                        │    
 ├────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────┤    
 │ SCK objc2-screen-capture-kit crashes at dlopen on  │ Highest priority — Tier 1 will surface immediately; fix │    
 │ 11.5.2 (not compile-time guarded)                  │  is runtime #[cfg] + availability check in              │    
 │                                                    │ screen_capture.rs                                       │    
 ├────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────┤    
 │ TCC cannot be pre-granted without MDM or user      │ Document manual procedure; exclude affected tests from  │    
 │ interaction                                        │ unattended runs                                         │    
 ├────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────┤    
 │ VMs report no audio devices                        │ Mark as expected failure in machine config via          │    
 │                                                    │ expected_failures: ["audio_enum"]                       │    
 ├────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────┤    
 │ AudioHardwareCreateProcessTap absent on 11.5.2     │ Must degrade gracefully; Tier 3 verifies graceful path  │    
 ├────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────┤    
 │ disable-library-validation entitlement requires    │ Tier 3 audio tests unreliable on ad-hoc signed builds;  │    
 │ Apple Developer signing                            │ document                                                │    
 ├────────────────────────────────────────────────────┼─────────────────────────────────────────────────────────┤    
 │ Machine asleep / unreachable                       │ Single retry with 5s backoff; then "status":            │    
 │                                                    │ "unreachable" in result                                 │    
 └────────────────────────────────────────────────────┴─────────────────────────────────────────────────────────┘    

 Ventura vs. 11.5.2 key differences:

 ┌───────────────────────────────┬────────────────────────────┬───────────────────────────────┐
 │                               │        Ventura 13.x        │        Big Sur 11.5.2         │
 ├───────────────────────────────┼────────────────────────────┼───────────────────────────────┤
 │ ScreenCaptureKit              │ Available                  │ Absent                        │
 ├───────────────────────────────┼────────────────────────────┼───────────────────────────────┤
 │ AudioHardwareCreateProcessTap │ Available                  │ Absent                        │
 ├───────────────────────────────┼────────────────────────────┼───────────────────────────────┤
 │ CGWindowListCreateImage       │ Works (deprecated Sonoma+) │ Works                         │
 ├───────────────────────────────┼────────────────────────────┼───────────────────────────────┤
 │ WKWebView                     │ Modern                     │ Older; CSS/JS quirks possible │
 └───────────────────────────────┴────────────────────────────┴───────────────────────────────┘

 ---
 8. CLI Shape

 # Full run against all machines in config
 python tools/compat/compat-run.py --app path/to/Wavis.app

 # Specific machine, specific tiers
 python tools/compat/compat-run.py --app path/to/Wavis.app \
   --machine mac-bigsur-intel --tiers t0,t1

 # Parallel (2 machines simultaneously)
 python tools/compat/compat-run.py --app path/to/Wavis.app --parallel 2

 # Dry run
 python tools/compat/compat-run.py --app path/to/Wavis.app --dry-run

 # Alternate config
 python tools/compat/compat-run.py --app path/to/Wavis.app \
   --config tools/compat/machines.local.toml

 ---
 Acceptance Criteria

 - --dry-run prints SSH/SCP commands without connecting
 - Unreachable machine → "status": "unreachable" in JSON, not a Python crash
 - SSH timeout respected; hung remote command killed after --timeout seconds
 - Tier 1 on healthy Tahoe machine → "pass": true in result.json
 - Tier 1 on 11.5.2 → crash report captured and surfaced in report if SCK is unguarded
 - Tier 0 with wrong deployment target binary → otool check fails with clear message
 - machines.local.toml is gitignored; machines.example.toml committed
 - Markdown report shows each machine, each tier, one-line failure note without opening raw logs

 ---
 Repo Gaps

 1. No SCK runtime availability guard verified in clients/wavis-gui/src-tauri/src/screen_capture.rs — check before   
 Slice 1 ships
 2. No macOS build in CI — Tier 0 must run locally until self-hosted runner added
 3. No TCC pre-grant runbook — create doc/testing/macos-compat.md in Slice 1
 4. __compat_check Tauri command doesn't exist — Slice 2 work
 5. .gitignore needs tools/compat/machines.local.toml entry

 Critical Files

 ┌───────────────────────────────────────────────────┬──────────────────────────────────────────────────────┐        
 │                       File                        │                      Relevance                       │        
 ├───────────────────────────────────────────────────┼──────────────────────────────────────────────────────┤        
 │ clients/wavis-gui/src-tauri/tauri.conf.json       │ Deployment target, bundle config                     │        
 ├───────────────────────────────────────────────────┼──────────────────────────────────────────────────────┤        
 │ clients/wavis-gui/src-tauri/Entitlements.plist    │ Signing entitlements                                 │        
 ├───────────────────────────────────────────────────┼──────────────────────────────────────────────────────┤        
 │ clients/wavis-gui/src-tauri/Cargo.toml            │ objc2-screen-capture-kit, platform deps              │        
 ├───────────────────────────────────────────────────┼──────────────────────────────────────────────────────┤        
 │ clients/wavis-gui/src-tauri/src/screen_capture.rs │ SCK availability guard — verify runtime check exists │        
 ├───────────────────────────────────────────────────┼──────────────────────────────────────────────────────┤        
 │ tools/smoke/smoke.py                              │ Reference pattern for controller style               │        
 ├───────────────────────────────────────────────────┼──────────────────────────────────────────────────────┤        
 │ tools/compat/                                     │ New directory (does not exist yet)                   │        
 └───────────────────────────────────────────────────┴──────────────────────────────────────────────────────┘


 # Implementation
 • Implemented Slice 1 from doc/implementation/olderMacosbuild.md.
                                                                                                                     
  Added the local macOS compatibility runner in tools/compat/compat-run.py:204 with Tier 0 package validation, Tier 1  SSH/SCP orchestration, retry/timeout handling, dry-run mode, parallel machine runs, and nonzero exit on real tier  
  failures. Added the remote launch agent in tools/compat/agent/run-agent.sh:64, report merger in tools/compat/      
  report/merge-report.py:53, sample inventory in tools/compat/machines.example.toml:5, and the TCC/setup runbook in  
  doc/testing/macos-compat.md:1. Also gitignored the local inventory at .gitignore:29.                               
                                                                                                                     
  Verified:                                                                                                          
                                                                                                                     
  - python -m py_compile tools\compat\compat-run.py tools\compat\report\merge-report.py                              
  - python tools\compat\compat-run.py --app path\to\Wavis.app --tiers t1 --dry-run --machine mac-bigsur-intel        
    --timeout 5                                                                                                      
                                                                                                                     
  I could not run bash -n for the agent script because both WSL bash.exe and Git Bash failed with Windows access-    
  denied errors in this environment. I removed the generated dry-run compat-results and Python cache output          
  afterward. Existing untracked doc/implementation/olderMacosbuild.md was left untouched.                            
 
 • Implemented step 2 / Tier 2 support.
                                                                                                                     
  What changed:                                                                                                      
                                                                                                                     
  - Added a bundled compat probe entry:                                                                              
    clients/wavis-gui/compat-probe.html and clients/wavis-gui/src/compat-probe.ts:17                                 
  - Updated Vite multi-page build so the probe is emitted into dist:                                                 
    clients/wavis-gui/vite.config.ts:33                                                                              
  - Added debug-only Tauri IPC commands and hidden probe window activation via WAVIS_COMPAT_PROBE_PATH /             
    WAVIS_COMPAT_RESULT_PATH:                                                                                        
    clients/wavis-gui/src-tauri/src/main.rs:682                                                                      
  - Allowed the compat-probe window in capabilities:                                                                 
    clients/wavis-gui/src-tauri/capabilities/default.json:5                                                          
  - Extended the compatibility runner and agent to support t2:                                                       
    tools/compat/compat-run.py:30                                                                                    
    tools/compat/agent/run-agent.sh:151                                                                              
  - Updated machine examples and the macOS compat runbook for t0,t1,t2.                                              
                                                                                                                     
  Verification:                                                                                                      
                                                                                                                     
  - Passed: npm run build in clients/wavis-gui; it emitted dist/compat-probe.html.                                   
  - Passed: python -m py_compile tools\compat\compat-run.py tools\compat\report\merge-report.py                      
  - Passed: dry run for --tiers t1,t2.                                                                               
  - Blocked: Rust cargo check on Windows still fails in existing webrtc-sys C++ build due missing Abseil headers;    
    macOS cross-check fails because this Windows host has no cc configured for x86_64-apple-darwin.                  
  - Not run: bash -n for run-agent.sh; bash is still blocked by Windows access-denied errors in this environment.

  • Implemented the next slice: Tier 3 media/system integration probing.
                                                                                                                     
  Added debug-only media capability fields to the compat IPC result in clients/wavis-gui/src-tauri/src/main.rs:678:  
  screen_capture_kit and audio_process_tap now report available_by_os, skipped, unknown, or not_applicable without   
  starting capture or mutating audio routing.                                                                        
                                                                                                                     
  Extended the remote agent with t3 in tools/compat/agent/run-agent.sh:151. It now runs the compat probe, verifies   
  graceful SCK/process-tap status on older macOS, captures ipc-result.json, and writes a best-effort tcc-dump.txt.   
                                                                                                                     
  Also updated Tier support and docs:                                                                                
                                                                                                                     
  - tools/compat/compat-run.py:30 now supports t3                                                                    
  - tools/compat/machines.example.toml includes t3                                                                   
  - doc/testing/macos-compat.md:43 documents Tier 3 usage                                                            
                                                                                                                     
  Verification:                                                                                                      
                                                                                                                     
  - Passed npm run build                                                                                             
  - Passed python -m py_compile tools\compat\compat-run.py tools\compat\report\merge-report.py                       
  - Passed dry-run: --tiers t1,t2,t3                                                                                 
  - Attempted cargo check --target x86_64-apple-darwin, but this Windows host is missing the target C compiler:      
    failed to find tool "cc" in objc2-exception-helper.                                                              
                                                                                                                     
  I also cleaned the generated dry-run compat-results and Python cache output.     

  • Slice 4 is implemented.

  Added .github/workflows/macos-compat.yml:1 with a workflow_dispatch-only GitHub Actions workflow that:

  - requires dispatch from a release tag via a refs/tags/* guard
  - runs on [self-hosted, macOS]
  - builds Wavis.app or accepts an app_path input
  - preserves local tools/compat/machines.local.toml with checkout clean: false
  - runs only --tiers t0,t1
  - uploads compat-results/ as an artifact

  Updated the runbook at doc/testing/macos-compat.md:80 with the manual workflow instructions and the self-hosted    
  runner inventory requirement.

  Verification: I statically checked the workflow contents and confirmed there are no push or pull_request triggers. 
  I did not run the GitHub workflow because it requires a configured self-hosted macOS runner, and actionlint is not 
  installed in this environment.