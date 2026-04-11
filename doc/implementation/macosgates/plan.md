 Wavis macOS Compatibility Gates — Analysis & Specification                                                          
  
 Branch: 235-older-macos-versions-cannot-run-wavis-build
 Scope: clients/wavis-gui/, clients/wavis-gui/src-tauri/, tools/compat/
 Focus: Ventura (13.x) and Big Sur (11.5.2) on Intel; Tahoe/Sequoia on ARM baseline

 ---
 Context

 Wavis already has a 4-tier compatibility runner (tools/compat/), AWS EC2 Mac
 Dedicated Host Terraform config, and a __compat_check Tauri IPC command (debug
 only). This document audits what each tier actually verifies, identifies
 concrete gaps that cause "works on M2, fails on Intel Ventura" failures, and
 specifies the checks that must exist before claiming older-macOS support.

 Deployment target: tauri.conf.json → minimumSystemVersion = "10.15"
 ScreenCaptureKit: weak-linked via build.rs (-Wl,-weak_framework,ScreenCaptureKit)
 Version gates in Rust:
 - SCK available on ≥ 12.3 (supports_screen_capture_kit)
 - Process tap available on ≥ 14.2 (supports_process_tap)
 - Virtual-device fallback (BlackHole / Wavis Audio Tap HAL driver) for < 12.3

 Build note: Current build-mac.sh outputs only aarch64.dmg. Intel (x86_64)
 builds must be produced separately for Intel machine testing.

 Release vs. debug detection: The runner has no reliable way to inspect build
 profile from a .app bundle after the fact. Use a --debug CLI flag passed by
 the caller (compat-run.py --debug). When --debug is set, the runner skips
 notarization staple checks and treats unsigned builds as warns. When omitted,
 these are failures.

 ---
 1. Top Compatibility Gates

 Gate A — Deployment Target + Info.plist Min Version

 Why: LC_BUILD_VERSION minos controls whether dyld loads the binary at all.
 LSMinimumSystemVersion in Info.plist controls the Finder's "requires macOS X.Y"
 dialog that appears when a user double-clicks the app on an incompatible machine.
 Both must match tauri.conf.json minimumSystemVersion.
 Failure on older macOS: Wrong minos → dyld refuses to exec. Wrong
 LSMinimumSystemVersion → confusing Finder dialog, app won't open via double-click.
 Check: otool -l for minos; plistlib.load(Info.plist)["LSMinimumSystemVersion"].
 Status: ✅ minos check implemented. ❌ LSMinimumSystemVersion not cross-checked.

 ---
 Gate B — Binary Architecture + Rosetta Detection

 Why: An aarch64-only binary on Intel runs under Rosetta 2. Rosetta masks
 genuine Intel failures — JIT entitlement behavior, CoreAudio device numbering,
 and dyld symbol resolution all differ under translation.
 Failure on older macOS: Intel test passes under Rosetta; real Intel-path bugs
 go undetected.
 Check (static): lipo -info <binary> — expected x86_64 or x86_64 arm64
 for Intel targets.
 Check (runtime): sysctl sysctl.proc_translated in write_machine_info —
 value 1 = Rosetta active → fail the run.
 Status: ✅ uname -m captured. ❌ lipo -info not run. ❌ Rosetta not detected. Most dangerous gap.

 ---
 Gate C — Notarization Staple

 Why: On Ventura+, Gatekeeper requires the notarization ticket to be stapled
 to the bundle. Without it, the app is blocked at launch with a modal dialog; no
 crash report is generated.
 Failure on older macOS: open -n exits 0 but process never appears.
 Check: xcrun stapler validate Wavis.app — fail if exit code ≠ 0 (release
 builds only, controlled by --debug flag on runner).
 Status: ❌ Not implemented. #1 cause of "works in dev, fails on clean Ventura".

 ---
 Gate D — Required Entitlements

 Why: device.audio-input → mic permission prompt. disable-library-validation
 → AudioHardwareCreateProcessTap + system audio TCC. allow-jit → WebKit JIT.
 Missing any of these causes silent or misleading failures.
 Check: codesign --display --entitlements :- Wavis.app — verify three keys.
 Status: ✅ Implemented.

 ---
 Gate E — ScreenCaptureKit Weak-Link

 Why: SCK is weak-linked so the binary loads on < 12.3. If the weak-link flag
 is accidentally dropped by a build change, the binary hard-links SCK and crashes
 at dyld load time on any Mac running < 12.3.
 Check: Parse existing otool-l.txt (already captured) for LC_LOAD_WEAK_DYLIB
 with SCK path. If SCK appears as LC_LOAD_DYLIB → fail.
 Status: otool -L captured but ❌ never analyzed for weak/strong distinction.

 ---
 Gate F — Tauri IPC Bridge

 Why: On older macOS/WebKit, the injected IPC script can fail to initialize
 before the page calls window.__TAURI_INTERNALS__. Result: white window, no UI,
 no crash report.
 Check: __compat_check command returns ipc_ok: true (t2).
 Status: ✅ Implemented.

 ---
 Gate G — Plugin Store

 Why: tauri-plugin-store writes to the app support directory. Sandbox path
 or filesystem permission changes on older macOS can cause the store to fail.
 Check: compat-probe.ts → probeStore() — write/read/delete round trip (t2).
 Status: ✅ Implemented.

 ---
 Gate H — SCK Graceful Degradation (pre-12.3)

 Why: On < 12.3, SCK symbol handles are null. Any dereference without a version
 gate crashes. The version gate code must be correct.
 Check: t3 screen_capture_kit.status == "skipped" on < 12.3.
 Status: ✅ Implemented.

 ---
 Gate I — Process Tap Graceful Degradation (pre-14.2)

 Why: AudioHardwareCreateProcessTap returns error on Ventura (< 14.2). The
 gate must route to SCK fallback, not crash.
 Check: t3 audio_process_tap.status == "skipped" on < 14.2.
 Status: ✅ Implemented.

 ---
 Gate J — Audio Device Enumeration

 Why: If CoreAudio init fails or mic TCC is denied, get_default_audio_monitor
 returns an empty list and audio share silently fails.
 Check: t3 audio_devices length > 0 when mic TCC is confirmed granted.
 Status: ❌ audio_devices collected but count never validated.

 ---
 Gate K — Virtual Audio Device Path (pre-12.3)

 Why: On < 12.3, SCK is absent. The fallback is virtual-device routing via
 BlackHole or the bundled Wavis Audio Tap HAL driver. If neither is installed,
 system audio capture fails silently.
 Check: check_audio_driver Tauri command (already exists) — inspect result
 in t3 on Big Sur. Report whether a virtual loopback device is detected.
 Note: This is the one system-audio path on Big Sur. If check_audio_driver
 returns false and SCK is absent, system audio is unavailable — this should be
 surfaced as a warn (not fail) since driver install is a user action.
 Status: ❌ Not included in any compat tier today.

 ---
 2. Ranked Recommendations

 Top 3 checks that catch the most real-world older-macOS failures

 ┌──────┬────────────────────┬──────────────────────────────────────────────────────────────────────────────────┐    
 │ Rank │        Gate        │                                       Why                                        │    
 ├──────┼────────────────────┼──────────────────────────────────────────────────────────────────────────────────┤    
 │ 1    │ C — Notarization   │ Silently blocks launch on Ventura+ clean machines. No crash report. Undetectable │    
 │      │ staple             │  without stapler validate.                                                       │    
 ├──────┼────────────────────┼──────────────────────────────────────────────────────────────────────────────────┤    
 │ 2    │ B — Binary arch +  │ aarch64 binary on Intel Rosetta passes every test while hiding all Intel-native  │    
 │      │ Rosetta            │ failures. Invalidates all existing Intel results retroactively.                  │    
 ├──────┼────────────────────┼──────────────────────────────────────────────────────────────────────────────────┤    
 │ 3    │ E — SCK weak-link  │ A single accidental build change makes the binary crash at dyld load on every    │    
 │      │                    │ pre-12.3 Mac. Undetectable without inspecting otool -l.                          │    
 └──────┴────────────────────┴──────────────────────────────────────────────────────────────────────────────────┘    

 Mandatory for every release run

 Gates A, B, C, D, E, F (t0+t2 minimum)

 Second-tier / debug-build only

 Gates G, H, I, J, K (t2+t3, require debug binary + TCC pre-grant)

 ---
 3. Tier-to-Gate Mapping

 All t0 checks run locally in compat-run.py → run_tier0. macOS toolchain
 (otool, codesign, lipo, xcrun) is required. On non-macOS controllers,
 record note for unavailable tools. Authoritative t0 requires a macOS controller.

 t0 (static, local): Gates A, B, C, D, E + app version extraction
 t1 (remote, launch): Validates Gates C/E at runtime (crash = gate failed)
 t2 (remote, debug): Gates F, G
 t3 (remote, debug): Gates H, I, J, K

 Gaps per tier (what needs to be added):
 - t0: Gate A plist cross-check, Gate B lipo -info, Gate C stapler validate,
   Gate E otool-l.txt weak-link parse, codesign promoted to failure for release
 - t1: Log predicate missing coreaudio/ScreenCapture subsystems; no error/fault
   count in report
 - t3: Audio device count not validated (Gate J); TCC dump not parsed for
   validation; check_audio_driver not included (Gate K)

 ---
 t4 — Manual Graceful Degradation (runbook, no automation)

 Run this once per OS version before release. No CI equivalent exists.

 Big Sur (11.5.2) — system audio on first launch:
 1. Open Wavis. Grant mic in TCC prompt.
 2. Start a voice room (confirm mic audio works).
 3. Attempt to start a screen share: expected → permission prompt or "not supported" message. Fail if crash.
 4. Attempt to share system audio (screen share audio toggle): expected → "install BlackHole" prompt or graceful     
 "unavailable" state. Fail if crash.
 5. Confirm app is still running after all attempts. Record outcome.

 Ventura (13.x) — system audio fallback:
 1. Grant mic and Screen Recording in System Settings before launch.
 2. Open Wavis. Start a voice room.
 3. Attempt system audio share: expected → SCK captures (no process tap). Fail if crash or silent failure.
 4. Revoke Screen Recording permission mid-session. Re-attempt share: expected → graceful error. Fail if crash.      

 ---
 5. Native-Risk Checks for Wavis Specifically

 Risk: SCK null pointer dereference
 macOS Versions: < 12.3
 Gate: E + H
 Older-macOS Specifics: Weak-link guard is the only protection. Gate E (static) must pass before t1 is meaningful on 

   Big Sur.
 ────────────────────────────────────────
 Risk: Process tap returning OSStatus error
 macOS Versions: < 14.2
 Gate: I
 Older-macOS Specifics: On Ventura, tap API returns kAudioHardwareIllegalOperationError. Gate I verifies the code    
   skips it.
 ────────────────────────────────────────
 Risk: AudioHardwareCreateProcessTap Tahoe regression
 macOS Versions: 26.x (beta)
 Gate: I
 Older-macOS Specifics: Returns OSStatus 560947818 = '!obj' — tap is nominally available but broken. t3
   audio_process_tap status should reflect this.
 ────────────────────────────────────────
 Risk: Mic TCC not granted before first CoreAudio call
 macOS Versions: All, especially Ventura
 Gate: J
 Older-macOS Specifics: Pre-grant TCC before t3. Empty device list with TCC granted = CoreAudio failure, not TCC     
   failure.
 ────────────────────────────────────────
 Risk: Virtual device absent on Big Sur
 macOS Versions: < 12.3
 Gate: K
 Older-macOS Specifics: BlackHole or Wavis Audio Tap required for system audio. check_audio_driver exposes this;     
   include in t3 report on Big Sur machines.
 ────────────────────────────────────────
 Risk: Screen share getDisplayMedia prompt not appearing
 macOS Versions: All non-interactive
 Gate: t4 (manual)
 Older-macOS Specifics: t2/t3 launches are non-interactive (direct binary, not open); TCC prompts won't appear.      
   Pre-grant all permissions before any t3 run.
 ────────────────────────────────────────
 Risk: TCC DB schema
 macOS Versions: Big Sur vs. newer
 Gate: J/t3
 Older-macOS Specifics: SELECT service, auth_value FROM access WHERE client='com.wavis.desktop' is valid on Big Sur  
   (Mojave+). The auth_reason and flags columns are absent on Big Sur but are not queried.

 ---
 6. Report Fields Per Machine

 machine:
   name, macos_version, hardware_arch (uname -m), model (hw.model)
   rosetta_active: bool              ← NEW (sysctl sysctl.proc_translated)

 app:
   version, build_id                 ← NEW (from Info.plist)
   binary_sha256
   binary_arch                       ← NEW (lipo -info)
   deployment_target                 ← existing
   plist_min_version                 ← NEW (LSMinimumSystemVersion)
   sck_link_type: "weak"|"strong"|"absent"   ← NEW
   signing_status: "valid"|"invalid"|"unsigned"
   notarization_stapled: bool        ← NEW

 tiers:
   t0: pass, failures[], notes[], artifacts[]
     + plist_min_version_match       ← NEW
     + binary_arch                   ← NEW
     + arch_matches_expected         ← NEW
     + notarization_stapled          ← NEW
     + sck_link_type                 ← NEW

   t1: pass, launch_exit_code, process_running_after_10s,
       crash_report_count, log_error_count  ← NEW (count of error/fault lines)
       failures[], artifacts[]

   t2: pass, ipc_ok, store_ok, failures[], artifacts[]

   t3: pass, audio_devices_found,   ← NEW (count)
       mic_tcc_granted,              ← NEW (parsed from TCC dump)
       screen_tcc_granted,           ← NEW
       screen_capture_kit_status, audio_process_tap_status,
       virtual_audio_driver_found,   ← NEW (from check_audio_driver on Big Sur)
       failures[], artifacts[]

 first_failing_phase: "t0"|"t1"|"t2"|"t3"|null
 likely_failure_category             ← see below
 log_bundle_path

 likely_failure_category — use structured failure codes, not free-form text
 pattern matching. Each check in the runner should emit a machine-readable code
 alongside the human message (e.g., ARCH_MISMATCH, ROSETTA_DETECTED,
 NOTARIZATION_MISSING, ENTITLEMENT_MISSING, SCK_HARD_LINKED,
 LAUNCH_CRASH, IPC_TIMEOUT, STORE_FAILED, SCK_VERSION_WRONG,
 TAP_VERSION_WRONG, AUDIO_DEVICES_EMPTY, TCC_DENIED). The report merger
 maps the first failing code to a likely_failure_category.

 ---
 7. Acceptance Criteria

 t0

 - lipo -info run; binary_arch in report; arch mismatch vs. machine expectation is failure
 - Rosetta detection in write_machine_info; active Rosetta on Intel is failure
 - xcrun stapler validate run for release builds (--debug skips); failure = failure
 - otool-l.txt parsed for SCK link type; LC_LOAD_DYLIB for SCK = failure
 - LSMinimumSystemVersion from Info.plist compared to config; mismatch = failure
 - CFBundleShortVersionString + CFBundleVersion extracted and in report
 - codesign failure is failure for release builds (not note)
 - All failures carry a structured code (ARCH_MISMATCH, NOTARIZATION_MISSING, etc.)

 t1

 - Log predicate includes coreaudio and ScreenCapture subsystems
 - log_error_count reported (error/fault entries from app process)
 - Crash report count > 0 = failure

 t2

 - ipc_ok: false = failure with code IPC_TIMEOUT or IPC_FAILED
 - store_ok: false = failure with code STORE_FAILED

 t3

 - TCC dump parsed; mic_tcc_granted and screen_tcc_granted in report
 - audio_devices_found == 0 when mic_tcc_granted == true = failure (AUDIO_DEVICES_EMPTY)
 - check_audio_driver result included for Big Sur machines (virtual_audio_driver_found)
 - SCK/tap status mismatch vs. OS version = failure with structured code

 Report

 - first_failing_phase set to lowest-numbered failing tier
 - likely_failure_category mapped from first structured failure code
 - JSON + Markdown outputs written

 CI

 - Workflow exits non-zero if t0 or t1 fails

 ---
 8. Recommended Path

 Must implement first — non-negotiable for Intel older-macOS claim

 1. Binary arch + Rosetta detection
 - compat-run.py run_tier0: add lipo -info; cross-validate against machine arch field in machines.local.toml
 - run-agent.sh write_machine_info: add sysctl sysctl.proc_translated
 - Rosetta = 1 on Intel machine → structured failure ROSETTA_DETECTED
 - This invalidates all existing Intel test results without this fix.

 2. Notarization staple check
 - compat-run.py run_tier0: add xcrun stapler validate — skip if args.debug
 - Add --debug flag to compat-run.py arg parser (caller sets it for debug builds)
 - Failure code: NOTARIZATION_MISSING

 3. SCK weak-link verification
 - compat-run.py run_tier0: parse already-captured otool-l.txt for LC_LOAD_WEAK_DYLIB vs LC_LOAD_DYLIB for
 ScreenCaptureKit
 - No new subprocess needed
 - Failure code: SCK_HARD_LINKED

 4. LSMinimumSystemVersion cross-check
 - compat-run.py run_tier0: plistlib already imported — read Info.plist, compare
 - Failure code: PLIST_VERSION_MISMATCH

 Can wait — second pass

 5. Audio device count validation (t3, run-agent.sh)
 - Parse ipc-result.json#audio_devices length; fail if 0 when mic_tcc_granted == true

 6. TCC parse (t3, run-agent.sh)
 - Parse TCC dump, extract auth_value for mic and screen; set mic_tcc_granted / screen_tcc_granted in report
 - Query: SELECT service, auth_value FROM access WHERE client='com.wavis.desktop' (works Big Sur+)

 7. Virtual audio driver check on Big Sur (t3, run-agent.sh)
 - Add check_audio_driver to the compat probe result fields on pre-12.3 machines

 8. Structured failure codes + likely_failure_category (compat-run.py + merge-report.py)
 - Extend failure dict to include code field; map codes in merger

 9. Log error count (t1, run-agent.sh)
 - Scan system.log for error/fault entries; report count

 ---
 Minimum Compatibility Bar Before Saying Wavis Supports Older macOS

 All items must be green for each claimed version before shipping.

 Once per build — on macOS controller:
 - minos == 10.15 in binary
 - LSMinimumSystemVersion == 10.15 in Info.plist
 - Binary contains x86_64 (Intel targets) or arm64 (ARM targets)
 - Notarization ticket stapled (xcrun stapler validate exits 0)
 - Code signature valid
 - Three entitlements present
 - SCK is LC_LOAD_WEAK_DYLIB (not LC_LOAD_DYLIB)

 On each target machine — native arch (not Rosetta):
 - sysctl.proc_translated == 0 confirmed
 - App launches without crash (t1 pass)
 - Zero crash reports after 10s
 - IPC bridge functional (t2 ipc_ok == true)
 - Plugin store functional (t2 store_ok == true)
 - SCK status correct for OS version (t3)
 - Process tap status correct for OS version (t3)

 Big Sur (11.5.2) additionally:
 - App launches and does NOT crash despite SCK absent (verified by t1 + Gate E)
 - Manual t4: system audio returns graceful error, not crash

 Ventura (13.x) additionally:
 - Audio device list non-empty with mic TCC granted (t3)
 - Manual t4: system audio via SCK works, process tap skips gracefully