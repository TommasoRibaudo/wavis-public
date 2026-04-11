# Wavis macOS Compatibility Gates — Implementation Tasks

## Phase 1: High Priority (Must Implement First)

### 1. Binary Architecture & Rosetta Detection
- [x] 1.1 Update `tools/compat/agent/run-agent.sh` → `write_machine_info()` to include `sysctl sysctl.proc_translated` in `machine-info.json` (value 0 or 1; default 0 if sysctl fails).
- [x] 1.2 Update `tools/compat/compat-run.py` → `run_tier0()` to run `lipo -info <binary>` and store output in `lipo-info.txt`. Extract arch string (e.g., `"arm64"`, `"x86_64 arm64"`).
- [x] 1.3 In `run_tier0()`: cross-validate `binary_arch` against each machine's `arch` field from `machines.local.toml`. If machine is `x86_64` and binary does not contain `x86_64` → failure with code `ARCH_MISMATCH`.
- [x] 1.4 In `run_remote_machine()`: after fetching `machine-info.json`, check `proc_translated`. If machine arch is `x86_64` and `proc_translated == 1` → failure with code `ROSETTA_DETECTED`.

### 2. Notarization Staple Check
- [x] 2.1 Add `--debug` flag to `tools/compat/compat-run.py` `parse_args()`. Store as `args.debug` (bool, default False).
- [x] 2.2 In `run_tier0()`: run `xcrun stapler validate <app_path>`. Write output to `stapler-validate.txt`.
- [x] 2.3 If `args.debug` is True: skip the stapler check entirely (record note "skipped: --debug"). If False: non-zero exit → failure with code `NOTARIZATION_MISSING`.
- [x] 2.4 In `run_tier0()`: promote existing codesign `--verify` failure from note to failure when `args.debug` is False. Keep as note when `args.debug` is True.

### 3. SCK Weak-Link Verification
- [x] 3.1 In `run_tier0()`: parse already-captured `otool-l.txt` for load commands referencing `ScreenCaptureKit`. Classify as `"weak"` (LC_LOAD_WEAK_DYLIB), `"strong"` (LC_LOAD_DYLIB), or `"absent"`. Store `sck_link_type` in t0 result.
- [x] 3.2 If `sck_link_type == "strong"` → failure with code `SCK_HARD_LINKED`.

### 4. Info.plist & Version Cross-checks
- [x] 4.1 In `run_tier0()`: use `plistlib.load()` (already imported) to read `Contents/Info.plist`. Extract `LSMinimumSystemVersion`.
- [x] 4.2 Compare `LSMinimumSystemVersion` against `tauri.conf.json` `minimumSystemVersion`. Mismatch → failure with code `PLIST_VERSION_MISMATCH`.
- [x] 4.3 Extract `CFBundleShortVersionString` and `CFBundleVersion` from Info.plist. Include as `app_version` and `app_build_id` in t0 result (report-only, no pass/fail).

---

## Phase 2: Runtime Enhancements (Second Pass)

### 5. t1 Log Predicate & Error Count
- [x] 5.1 In `tools/compat/agent/run-agent.sh` → `run_t1()`: update the `log show` predicate to add `OR subsystem CONTAINS "coreaudio" OR subsystem CONTAINS "ScreenCapture"` (currently only in `run_t3()`).
- [x] 5.2 After capturing `system.log` in `run_t1()`: count lines containing `<Error>` or `<Fault>` from the app process. Write count to t1 result as `log_error_count`. Surface as note (not hard failure).

### 6. t3 TCC Parse & Audio Device Validation
- [x] 6.1 In `run_t3()`: after the existing TCC dump, parse `tcc-dump.txt` to extract `auth_value` for `kTCCServiceMicrophone` and `kTCCServiceScreenCapture`. Set `mic_tcc_granted` (auth_value == 2) and `screen_tcc_granted` (auth_value == 2) in the t3 result JSON.
- [x] 6.2 In `run_t3()`: parse `ipc-result.json` to count `audio_devices` array length. Store as `audio_devices_found` in t3 result.
- [x] 6.3 If `audio_devices_found == 0` AND `mic_tcc_granted == true` → failure with code `AUDIO_DEVICES_EMPTY`.

### 7. Virtual Audio Driver Check (Big Sur)
- [x] 7.1 In `clients/wavis-gui/src-tauri/src/main.rs` → `__compat_check`: add `virtual_audio_driver` field to `CompatCheckResult`. On macOS < 12.3, call the existing `check_audio_driver` Tauri command logic and include the result. On ≥ 12.3 or non-macOS, set to `"not_applicable"`.
- [x] 7.2 In `clients/wavis-gui/src/compat-probe.ts`: add `virtual_audio_driver` to the `CompatCheckResult` type.
- [x] 7.3 In `run_t3()`: parse `ipc-result.json#virtual_audio_driver` on Big Sur machines. If `false` and SCK is absent → note (not failure, since driver install is a user action).
- [x] 7.4 Include `virtual_audio_driver_found` in the t3 result JSON.

### 8. Structured Failure Codes & Report Enhancements
- [x] 8.1 In `tools/compat/compat-run.py`: change all `failures.append("message")` calls to `failures.append({"code": "CODE", "message": "message"})` throughout `run_tier0()` and `run_remote_machine()`.
- [x] 8.2 In `tools/compat/agent/run-agent.sh`: update failure lines in `run_t1()`, `run_t2()`, `run_t3()` to prefix each line with a code: `CODE: human message` (e.g., `LAUNCH_CRASH: captured 2 crash report(s)`). The merge step parses the prefix.
- [x] 8.3 In `tools/compat/report/merge-report.py` → `build_report()`: compute `first_failing_phase` (lowest-numbered tier with `pass: false`) and `likely_failure_category` (mapped from the first structured failure code in that tier).
- [x] 8.4 In `merge-report.py` → `markdown_summary()`: include `first_failing_phase` and `likely_failure_category` in the per-machine Markdown output.
- [x] 8.5 Add all new report fields to the JSON output: `machine.rosetta_active`, `app.binary_arch`, `app.plist_min_version`, `app.sck_link_type`, `app.notarization_stapled`, `app.version`, `app.build_id`, `tiers.t1.log_error_count`, `tiers.t3.audio_devices_found`, `tiers.t3.mic_tcc_granted`, `tiers.t3.screen_tcc_granted`, `tiers.t3.virtual_audio_driver_found`.

### 9. --debug Flag Propagation to Agent
- [x] 9.1 In `tools/compat/compat-run.py` → `run_remote_machine()`: pass `--debug` to the agent SSH command when `args.debug` is True.
- [x] 9.2 In `tools/compat/agent/run-agent.sh`: accept `--debug` flag in the arg parser. Store as `DEBUG_BUILD` variable.
- [x] 9.3 In `run_t2()` and `run_t3()`: if `DEBUG_BUILD` is not set, skip the tier and record note "skipped: t2/t3 require debug builds (pass --debug)". This enforces the invariant that t2/t3 only run against debug builds.

---
TODO TOMORROW
## Phase 3: Manual Verification & Documentation

### 10. Manual Acceptance (t4)
- [ ] 10.1 Conduct manual t4 on Big Sur (11.5.2) per the plan.md runbook (5 steps). Record results in `compat-results/<run-id>/t4-bigsur-manual.md` with date, tester name, and per-step pass/fail.
- [ ] 10.2 Conduct manual t4 on Ventura (13.x) per the plan.md runbook (4 steps). Record results in `compat-results/<run-id>/t4-ventura-manual.md` with date, tester name, and per-step pass/fail.
- [ ] 10.3 Both t4 result files must be committed alongside the compat-report for the release. A release claiming older-macOS support requires both files present and all steps marked pass.
