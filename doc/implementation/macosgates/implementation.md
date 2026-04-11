# macOS Compatibility Gates — Implementation Log

## Task 1: Binary Architecture & Rosetta Detection

- Added `lipo -info` capture to `tools/compat/compat-run.py` `run_tier0()`, stores `lipo-info.txt`.
- Extracts `binary_arch` and validates against machine inventory arch values. Mismatches fail with `ARCH_MISMATCH`.
- Added `sysctl sysctl.proc_translated` to remote `machine-info.json` in `run-agent.sh` `write_machine_info()`.
- Added remote Rosetta validation in `compat-run.py` `apply_rosetta_gate()`. Intel machines with `proc_translated == 1` fail with `ROSETTA_DETECTED`.
- Updated report formatting for coded failure objects in `merge-report.py`.

## Task 2: Notarization Staple Check

- Added `--debug` CLI flag to `compat-run.py`.
- Tier 0 runs `xcrun stapler validate` for non-debug builds, writes `stapler-validate.txt`.
- Stapler failure emits `NOTARIZATION_MISSING`. `--debug` skips stapler entirely.
- `codesign --verify` failures promoted to failure for release runs, remain notes for `--debug`.
- Added `notarization_stapled` to Tier 0 result JSON.

## Task 3: SCK Weak-Link Verification

- Added `parse_sck_link_type()` to classify ScreenCaptureKit load commands as weak, strong, or absent.
- Tier 0 parses captured `otool-l.txt` and writes `sck_link_type` into result.
- `LC_LOAD_DYLIB` for SCK fails with `SCK_HARD_LINKED`.

## Task 4: Info.plist & Version Cross-checks

- Tier 0 loads `Contents/Info.plist` via `plistlib`.
- Compares `LSMinimumSystemVersion` against `tauri.conf.json` `minimumSystemVersion`. Mismatch fails with `PLIST_VERSION_MISMATCH`.
- Extracts `CFBundleShortVersionString` and `CFBundleVersion` as `app_version` and `app_build_id`.

## Task 5: t1 Log Predicate & Error Count

- Updated `run_t1()` log predicate to include `coreaudio` and `ScreenCapture` subsystems.
- Added `log_error_count` to t1 result JSON (count of `<Error>`/`<Fault>` lines from app process).
- Non-zero counts surfaced as notes without failing the tier.

## Task 6: t3 TCC Parse & Audio Device Validation

- Added `tcc_auth_value()` parsing for `kTCCServiceMicrophone` and `kTCCServiceScreenCapture`.
- Added `mic_tcc_granted` and `screen_tcc_granted` to t3 result JSON.
- Added `json_array_length()` for `ipc-result.json#audio_devices` count.
- `audio_devices_found == 0` with `mic_tcc_granted == true` fails with `AUDIO_DEVICES_EMPTY`.

## Task 7: Virtual Audio Driver Check (Big Sur)

- Added `virtual_audio_driver` field to `__compat_check` as `CompatCapabilityStatus`. On macOS < 12.3, calls existing `check_audio_driver`; returns `available_by_os` if found, `skipped` if not. On >= 12.3 or non-macOS, returns `not_applicable`.
- Agent parses `virtual_audio_driver.status` via `json_field_status` on pre-12.3 machines.
- Missing driver reported as note, not failure.

## Task 8: Structured Failure Codes & Report Enhancements

- All Tier 0 failures in `compat-run.py` emit `{code, message}` dicts.
- Agent t1/t2/t3 failure lines prefixed with codes (`LAUNCH_CRASH`, `IPC_TIMEOUT`, `STORE_FAILED`, etc.).
- `merge-report.py` parses both dict and `CODE: message` failures, computes `first_failing_phase` and `likely_failure_category`, includes both in JSON and Markdown.
- Added `machine.rosetta_active` and app summary fields (`binary_arch`, `plist_min_version`, `sck_link_type`, `notarization_stapled`, `version`, `build_id`) to merged report.

## Task 9: --debug Flag Propagation to Agent

- `args.debug` threaded into `run_remote_machine()` and appended as `--debug` to SSH agent command.
- Agent parses `--debug`, stores as `DEBUG_BUILD`.
- `run_t2()` and `run_t3()` skip with note when `DEBUG_BUILD != true`.

## Verification Notes

All Python files pass `py_compile`. Dry-run orchestration verified for `--debug` and non-debug paths. `bash -n` could not run on this Windows host (WSL blocked with E_ACCESSDENIED). `cargo check` for macOS targets could not run on Windows (missing cross-compiler). Real macOS validation requires a target machine.
