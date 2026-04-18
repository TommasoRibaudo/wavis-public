#!/usr/bin/env bash
set -eu

APP_PATH=""
OUT_DIR=""
MACHINE_NAME=""
TIERS="t1"
TIMEOUT_SECS="120"
TIER_RESULTS=""
DEBUG_BUILD=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --app) APP_PATH="$2"; shift 2 ;;
    --out) OUT_DIR="$2"; shift 2 ;;
    --machine) MACHINE_NAME="$2"; shift 2 ;;
    --tiers) TIERS="$2"; shift 2 ;;
    --timeout) TIMEOUT_SECS="$2"; shift 2 ;;
    --debug) DEBUG_BUILD=true; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ -z "$APP_PATH" ] || [ -z "$OUT_DIR" ] || [ -z "$MACHINE_NAME" ]; then
  echo "--app, --out, and --machine are required" >&2
  exit 2
fi

mkdir -p "$OUT_DIR" "$OUT_DIR/crash-reports"

json_escape() {
  printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g; s/	/\\t/g'
}

json_lines() {
  awk 'BEGIN { first=1; printf "[" } { gsub(/\\/,"\\\\"); gsub(/"/,"\\\""); if (!first) printf ","; printf "\"%s\"", $0; first=0 } END { printf "]" }' "$1"
}

append_tier_result() {
  if [ -z "$TIER_RESULTS" ]; then
    TIER_RESULTS="$1"
  else
    TIER_RESULTS="$TIER_RESULTS,
    $1"
  fi
}

# Extract the "status" value from a specific top-level JSON object field.
# Usage: json_field_status <file> <field_name>
# Returns the status string on stdout, or empty if not found.
# This avoids the fragile grep that could match the wrong field.
json_field_status() {
  local file="$1" field="$2"
  # Use awk to find the field block and extract its status value.
  awk -v field="\"$field\"" '
    $0 ~ field { found=1 }
    found && /"status"/ {
      gsub(/.*"status"[[:space:]]*:[[:space:]]*"/, "")
      gsub(/".*/, "")
      print
      exit
    }
  ' "$file"
}

# Parse auth_value for a TCC service from sqlite3 pipe-delimited output.
# Depends on sqlite3 default "list" output mode (pipe-delimited).
# Format: kTCCServiceMicrophone|com.wavis.desktop|2
tcc_auth_value() {
  local file="$1" service="$2"
  awk -F'|' -v service="$service" '
    $1 == service { value=$3 }
    END {
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", value)
      if (value != "") print value
    }
  ' "$file"
}

json_array_length() {
  local file="$1" field="$2"
  if command -v python3 >/dev/null 2>&1; then
    python3 -c 'import json, sys; value = json.load(open(sys.argv[1], encoding="utf-8")).get(sys.argv[2]); print(len(value) if isinstance(value, list) else -1)' "$file" "$field" 2>/dev/null || printf '%s\n' "-1"
  elif command -v ruby >/dev/null 2>&1; then
    ruby -rjson -e 'value = JSON.parse(File.read(ARGV[0]))[ARGV[1]]; puts(value.is_a?(Array) ? value.length : -1)' "$file" "$field" 2>/dev/null || printf '%s\n' "-1"
  else
    printf '%s\n' "-1"
  fi
}

plist_value() {
  local key="$1"
  local fallback="$2"
  local value
  value="$(/usr/libexec/PlistBuddy -c "Print :$key" "$APP_PATH/Contents/Info.plist" 2>/dev/null || true)"
  if [ -n "$value" ]; then
    printf '%s' "$value"
  else
    printf '%s' "$fallback"
  fi
}

macos_before() {
  local wanted_major="$1"
  local wanted_minor="$2"
  local version major minor
  version="$(sw_vers -productVersion 2>/dev/null || true)"
  IFS=. read -r major minor _ <<EOF
$version
EOF
  major="${major:-0}"
  minor="${minor:-0}"
  case "$major:$minor" in
    *[!0-9:]* | :)
      return 1
      ;;
  esac
  if [ "$major" -lt "$wanted_major" ]; then
    return 0
  fi
  if [ "$major" -eq "$wanted_major" ] && [ "$minor" -lt "$wanted_minor" ]; then
    return 0
  fi
  return 1
}

write_machine_info() {
  local sw_vers_value uname_value model_value arch_value proc_translated_value
  sw_vers_value="$(sw_vers -productVersion 2>/dev/null || true)"
  uname_value="$(uname -a 2>/dev/null || true)"
  model_value="$(sysctl -n hw.model 2>/dev/null || true)"
  arch_value="$(uname -m 2>/dev/null || true)"
  proc_translated_value="$(sysctl -n sysctl.proc_translated 2>/dev/null || true)"
  case "$proc_translated_value" in
    1) ;;
    *) proc_translated_value=0 ;;
  esac
  cat > "$OUT_DIR/machine-info.json" <<JSON
{
  "macos": "$(json_escape "$sw_vers_value")",
  "uname": "$(json_escape "$uname_value")",
  "model": "$(json_escape "$model_value")",
  "arch": "$(json_escape "$arch_value")",
  "proc_translated": $proc_translated_value
}
JSON
}

run_t1() {
  local app_name executable bundle_id marker launch_exit running pids crash_count log_error_count pass notes_json failures_json
  app_name="$(plist_value CFBundleName "$(basename "$APP_PATH" .app)")"
  executable="$(plist_value CFBundleExecutable "$app_name")"
  bundle_id="$(plist_value CFBundleIdentifier "")"
  marker="$OUT_DIR/launch-start.marker"
  launch_exit=0
  running=false
  crash_count=0
  log_error_count=0
  : > "$OUT_DIR/t1-notes.txt"
  : > "$OUT_DIR/t1-failures.txt"
  touch "$marker"

  if [ ! -d "$APP_PATH" ]; then
    printf 'APP_BUNDLE_MISSING: app bundle missing on remote target: %s\n' "$APP_PATH" >> "$OUT_DIR/t1-failures.txt"
  else
    open -n "$APP_PATH" > "$OUT_DIR/launch-open.out" 2> "$OUT_DIR/launch-open.err" || launch_exit=$?

    # Headless fallback: EC2 Mac instances have no GUI session, so `open` fails
    # with "Domain does not support specified action". Launch the binary directly
    # and check it doesn't crash (exit via missing display is acceptable).
    local headless=false
    if [ "$launch_exit" -ne 0 ] && grep -q "Domain does not support specified action\|OSLaunchdErrorDomain" "$OUT_DIR/launch-open.err" 2>/dev/null; then
      headless=true
      printf 'open failed (headless); falling back to direct binary launch\n' >> "$OUT_DIR/t1-notes.txt"
      launch_exit=0
      "$APP_PATH/Contents/MacOS/$executable" > "$OUT_DIR/launch-direct.out" 2> "$OUT_DIR/launch-direct.err" &
      local direct_pid=$!
      sleep 10
      # In headless mode, the process will likely exit because there is no
      # display. That is not a failure. We only care about crashes.
      if kill -0 "$direct_pid" 2>/dev/null; then
        running=true
      else
        wait "$direct_pid" 2>/dev/null || true
        local direct_exit=$?
        # Signals 4(SIGILL), 6(SIGABRT), 10(SIGBUS), 11(SIGSEGV) indicate a crash.
        # Normal exit or SIGTERM from missing display is acceptable.
        if [ "$direct_exit" -gt 128 ]; then
          local sig=$((direct_exit - 128))
          case "$sig" in
            4|6|10|11)
              printf 'HEADLESS_CRASH: binary exited with signal %s\n' "$sig" >> "$OUT_DIR/t1-failures.txt"
              ;;
            *)
              printf 'headless: binary exited with signal %s (non-crash, acceptable)\n' "$sig" >> "$OUT_DIR/t1-notes.txt"
              ;;
          esac
        else
          printf 'headless: binary exited with code %s (no display, acceptable)\n' "$direct_exit" >> "$OUT_DIR/t1-notes.txt"
        fi
        # Mark as "running" for headless — the process started and did not crash.
        if [ ! -s "$OUT_DIR/t1-failures.txt" ]; then
          running=true
        fi
      fi
    else
      sleep 10
    fi

    if [ "$headless" != "true" ]; then
      pids="$(pgrep -f "$APP_PATH/Contents/MacOS/$executable" 2>/dev/null || true)"
      if [ -n "$pids" ]; then
        running=true
      elif [ -n "$bundle_id" ]; then
        if [ "$(osascript -e "application id \"$bundle_id\" is running" 2>/dev/null || true)" = "true" ]; then
          running=true
        fi
      fi
    fi

    if [ -d "$HOME/Library/Logs/DiagnosticReports" ]; then
      find "$HOME/Library/Logs/DiagnosticReports" \
        \( -name "${app_name}*.ips" -o -name "${app_name}*.crash" -o -name "${executable}*.ips" -o -name "${executable}*.crash" \) \
        -newer "$marker" -exec cp {} "$OUT_DIR/crash-reports/" \; 2> "$OUT_DIR/crash-copy.err" || true
    fi
    crash_count="$(find "$OUT_DIR/crash-reports" -type f 2>/dev/null | wc -l | tr -d ' ')"

    log show --style syslog --predicate "process == \"$executable\" OR process == \"$app_name\" OR subsystem CONTAINS \"coreaudio\" OR subsystem CONTAINS \"ScreenCapture\"" --last 30s \
      > "$OUT_DIR/system.log" 2> "$OUT_DIR/system-log.err" || true
    log_error_count="$(awk -v executable="$executable" -v app_name="$app_name" '
      # Match error/fault lines where the process field (before the first [:])
      # matches the app executable or name. log show --style syslog format:
      # timestamp thread type activity pid ttl process[pid]: message
      (/<Error>/ || /<Fault>/) {
        # Extract the process field: word before the first [pid] bracket.
        # BSD awk does not support match() with array capture, so use sub().
        proc = $0
        sub(/.*[[:space:]]/, "", proc)
        sub(/\[[0-9]+\].*/, "", proc)
        if (proc == executable || proc == app_name) count++
      }
      END { print count + 0 }
    ' "$OUT_DIR/system.log")"

    if [ "$launch_exit" -ne 0 ]; then
      printf 'LAUNCH_OPEN_FAILED: open failed with exit code %s\n' "$launch_exit" >> "$OUT_DIR/t1-failures.txt"
    fi
    if [ "$running" != "true" ]; then
      printf 'LAUNCH_NOT_RUNNING: app process was not running after launch wait\n' >> "$OUT_DIR/t1-failures.txt"
    fi
    if [ "$crash_count" -gt 0 ]; then
      printf 'LAUNCH_CRASH: captured %s crash report(s)\n' "$crash_count" >> "$OUT_DIR/t1-failures.txt"
    fi
    if [ "$log_error_count" -gt 0 ]; then
      printf 'system.log contained %s app error/fault line(s)\n' "$log_error_count" >> "$OUT_DIR/t1-notes.txt"
    fi

    if [ "$running" = "true" ]; then
      if [ -n "$bundle_id" ]; then
        osascript -e "tell application id \"$bundle_id\" to quit" > "$OUT_DIR/quit.out" 2> "$OUT_DIR/quit.err" || true
      else
        osascript -e "tell application \"$app_name\" to quit" > "$OUT_DIR/quit.out" 2> "$OUT_DIR/quit.err" || true
      fi
      sleep 2
    fi
  fi

  if [ -s "$OUT_DIR/t1-failures.txt" ]; then
    pass=false
  else
    pass=true
  fi
  notes_json="$(json_lines "$OUT_DIR/t1-notes.txt")"
  failures_json="$(json_lines "$OUT_DIR/t1-failures.txt")"

  append_tier_result "\"t1\": {
      \"pass\": $pass,
      \"launch_exit_code\": $launch_exit,
      \"process_running_after_10s\": $running,
      \"crash_report_count\": $crash_count,
      \"log_error_count\": $log_error_count,
      \"notes\": $notes_json,
      \"failures\": $failures_json,
      \"artifacts\": [\"machine-info.json\", \"launch-open.out\", \"launch-open.err\", \"system.log\", \"crash-reports/\"]
    }"
}

run_t2() {
  # NOTE: T2/T3 launch the binary directly (not via `open`) so that
  # WAVIS_COMPAT_PROBE_PATH and WAVIS_COMPAT_RESULT_PATH env vars are
  # inherited. `open` strips environment variables. This means TCC context
  # may differ from a real user launch via Finder/Dock. T1 uses `open` for
  # a realistic launch-crash check; T2/T3 trade that for IPC testability.
  local app_name executable bundle_id result_path probe_path pid wait_count pass notes_json failures_json
  if [ "$DEBUG_BUILD" != "true" ]; then
    append_tier_result "\"t2\": {
      \"pass\": true,
      \"notes\": [\"skipped: t2/t3 require debug builds (pass --debug)\"],
      \"failures\": [],
      \"artifacts\": []
    }"
    return
  fi

  app_name="$(plist_value CFBundleName "$(basename "$APP_PATH" .app)")"
  executable="$(plist_value CFBundleExecutable "$app_name")"
  bundle_id="$(plist_value CFBundleIdentifier "")"
  result_path="$OUT_DIR/ipc-result.json"
  probe_path="$OUT_DIR/compat-probe.html"
  wait_count=0
  pass=false
  : > "$OUT_DIR/t2-notes.txt"
  : > "$OUT_DIR/t2-failures.txt"
  rm -f "$result_path"
  printf '<!doctype html><title>Wavis compat probe marker</title>\n' > "$probe_path"

  if [ ! -x "$APP_PATH/Contents/MacOS/$executable" ]; then
    printf 'BINARY_MISSING: bundle executable missing or not executable: %s\n' "$APP_PATH/Contents/MacOS/$executable" >> "$OUT_DIR/t2-failures.txt"
  else
    WAVIS_COMPAT_PROBE_PATH="$probe_path" \
    WAVIS_COMPAT_RESULT_PATH="$result_path" \
      "$APP_PATH/Contents/MacOS/$executable" > "$OUT_DIR/t2-app.out" 2> "$OUT_DIR/t2-app.err" &
    pid=$!

    while [ "$wait_count" -lt 15 ]; do
      if [ -s "$result_path" ]; then
        break
      fi
      if ! kill -0 "$pid" 2>/dev/null; then
        break
      fi
      wait_count=$((wait_count + 1))
      sleep 1
    done

    if [ ! -s "$result_path" ]; then
      printf 'IPC_TIMEOUT: ipc-result.json was not written within 15s\n' >> "$OUT_DIR/t2-failures.txt"
    elif ! grep -q '"ipc_ok"[[:space:]]*:[[:space:]]*true' "$result_path"; then
      printf 'IPC_FAILED: ipc_ok was not true in ipc-result.json\n' >> "$OUT_DIR/t2-failures.txt"
    elif ! grep -q '"store_ok"[[:space:]]*:[[:space:]]*true' "$result_path"; then
      printf 'STORE_FAILED: store_ok was not true in ipc-result.json\n' >> "$OUT_DIR/t2-failures.txt"
    fi

    log show --style syslog --predicate "process == \"$executable\" OR process == \"$app_name\" OR subsystem == \"com.wavis.desktop\"" --last 30s \
      > "$OUT_DIR/t2-system.log" 2> "$OUT_DIR/t2-system-log.err" || true

    if [ -n "$bundle_id" ]; then
      osascript -e "tell application id \"$bundle_id\" to quit" > "$OUT_DIR/t2-quit.out" 2> "$OUT_DIR/t2-quit.err" || true
    fi
    if kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
    fi
    wait "$pid" 2>/dev/null || true
  fi

  if [ ! -s "$OUT_DIR/t2-failures.txt" ]; then
    pass=true
  fi
  notes_json="$(json_lines "$OUT_DIR/t2-notes.txt")"
  failures_json="$(json_lines "$OUT_DIR/t2-failures.txt")"

  append_tier_result "\"t2\": {
      \"pass\": $pass,
      \"notes\": $notes_json,
      \"failures\": $failures_json,
      \"artifacts\": [\"ipc-result.json\", \"t2-app.out\", \"t2-app.err\", \"t2-system.log\"]
    }"
}

run_t3() {
  local app_name executable bundle_id result_path probe_path pid wait_count pass notes_json failures_json tcc_db
  local mic_auth_value screen_auth_value mic_tcc_granted screen_tcc_granted audio_devices_found
  local virtual_audio_driver_found
  if [ "$DEBUG_BUILD" != "true" ]; then
    append_tier_result "\"t3\": {
      \"pass\": true,
      \"notes\": [\"skipped: t2/t3 require debug builds (pass --debug)\"],
      \"failures\": [],
      \"artifacts\": []
    }"
    return
  fi

  app_name="$(plist_value CFBundleName "$(basename "$APP_PATH" .app)")"
  executable="$(plist_value CFBundleExecutable "$app_name")"
  bundle_id="$(plist_value CFBundleIdentifier "com.wavis.desktop")"
  result_path="$OUT_DIR/ipc-result.json"
  probe_path="$OUT_DIR/compat-probe.html"
  wait_count=0
  pass=false
  mic_tcc_granted=false
  screen_tcc_granted=false
  audio_devices_found=-1
  virtual_audio_driver_found=null
  : > "$OUT_DIR/t3-notes.txt"
  : > "$OUT_DIR/t3-failures.txt"
  rm -f "$result_path"
  printf '<!doctype html><title>Wavis compat probe marker</title>\n' > "$probe_path"

  tcc_db="$HOME/Library/Application Support/com.apple.TCC/TCC.db"
  if command -v sqlite3 >/dev/null 2>&1 && [ -f "$tcc_db" ]; then
    sqlite3 "$tcc_db" "SELECT service,client,auth_value FROM access WHERE client='$bundle_id';" \
      > "$OUT_DIR/tcc-dump.txt" 2> "$OUT_DIR/tcc-dump.err" || true
  else
    printf 'sqlite3 unavailable or TCC.db not readable at %s\n' "$tcc_db" > "$OUT_DIR/tcc-dump.txt"
  fi
  mic_auth_value="$(tcc_auth_value "$OUT_DIR/tcc-dump.txt" "kTCCServiceMicrophone")"
  screen_auth_value="$(tcc_auth_value "$OUT_DIR/tcc-dump.txt" "kTCCServiceScreenCapture")"
  if [ "$mic_auth_value" = "2" ]; then
    mic_tcc_granted=true
  fi
  if [ "$screen_auth_value" = "2" ]; then
    screen_tcc_granted=true
  fi

  if [ ! -x "$APP_PATH/Contents/MacOS/$executable" ]; then
    printf 'BINARY_MISSING: bundle executable missing or not executable: %s\n' "$APP_PATH/Contents/MacOS/$executable" >> "$OUT_DIR/t3-failures.txt"
  else
    WAVIS_COMPAT_PROBE_PATH="$probe_path" \
    WAVIS_COMPAT_RESULT_PATH="$result_path" \
      "$APP_PATH/Contents/MacOS/$executable" > "$OUT_DIR/t3-app.out" 2> "$OUT_DIR/t3-app.err" &
    pid=$!

    while [ "$wait_count" -lt 15 ]; do
      if [ -s "$result_path" ]; then
        break
      fi
      if ! kill -0 "$pid" 2>/dev/null; then
        break
      fi
      wait_count=$((wait_count + 1))
      sleep 1
    done

    if [ ! -s "$result_path" ]; then
      printf 'IPC_TIMEOUT: ipc-result.json was not written within 15s\n' >> "$OUT_DIR/t3-failures.txt"
    elif ! grep -q '"ipc_ok"[[:space:]]*:[[:space:]]*true' "$result_path"; then
      printf 'IPC_FAILED: ipc_ok was not true in ipc-result.json\n' >> "$OUT_DIR/t3-failures.txt"
    fi

    if [ -s "$result_path" ]; then
      audio_devices_found="$(json_array_length "$result_path" "audio_devices")"
      if [ "$audio_devices_found" -lt 0 ]; then
        printf 'could not parse audio_devices length from ipc-result.json\n' >> "$OUT_DIR/t3-notes.txt"
      elif [ "$audio_devices_found" -eq 0 ] && [ "$mic_tcc_granted" = "true" ]; then
        printf 'AUDIO_DEVICES_EMPTY: audio_devices was empty while microphone TCC is granted\n' >> "$OUT_DIR/t3-failures.txt"
      fi
    fi

    if [ -s "$result_path" ] && macos_before 12 3; then
      local vad_status
      vad_status="$(json_field_status "$result_path" "virtual_audio_driver")"
      case "$vad_status" in
        available_by_os)
          virtual_audio_driver_found=true
          ;;
        skipped)
          virtual_audio_driver_found=false
          printf 'virtual audio driver not found on pre-12.3 macOS; system audio capture requires BlackHole or Wavis Audio Tap\n' >> "$OUT_DIR/t3-notes.txt"
          ;;
        *)
          printf 'could not parse virtual_audio_driver status from ipc-result.json on pre-12.3 macOS: %s\n' "$vad_status" >> "$OUT_DIR/t3-notes.txt"
          ;;
      esac
    fi

    if [ -s "$result_path" ] && macos_before 12 3; then
      local sck_status
      sck_status="$(json_field_status "$result_path" "screen_capture_kit")"
      if [ "$sck_status" != "skipped" ]; then
        printf 'SCK_VERSION_WRONG: ScreenCaptureKit status was "%s" (expected "skipped") on pre-12.3 macOS\n' "$sck_status" >> "$OUT_DIR/t3-failures.txt"
      fi
    fi

    if [ -s "$result_path" ] && macos_before 14 2; then
      local tap_status
      tap_status="$(json_field_status "$result_path" "audio_process_tap")"
      if [ "$tap_status" != "skipped" ]; then
        printf 'TAP_VERSION_WRONG: AudioHardwareCreateProcessTap status was "%s" (expected "skipped") on pre-14.2 macOS\n' "$tap_status" >> "$OUT_DIR/t3-failures.txt"
      fi
    fi

    log show --style syslog --predicate "process == \"$executable\" OR process == \"$app_name\" OR subsystem == \"com.wavis.desktop\" OR subsystem CONTAINS \"ScreenCapture\" OR subsystem CONTAINS \"coreaudio\"" --last 30s \
      > "$OUT_DIR/t3-system.log" 2> "$OUT_DIR/t3-system-log.err" || true

    if [ -n "$bundle_id" ]; then
      osascript -e "tell application id \"$bundle_id\" to quit" > "$OUT_DIR/t3-quit.out" 2> "$OUT_DIR/t3-quit.err" || true
    fi
    if kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
    fi
    wait "$pid" 2>/dev/null || true
  fi

  if [ ! -s "$OUT_DIR/t3-failures.txt" ]; then
    pass=true
  fi
  notes_json="$(json_lines "$OUT_DIR/t3-notes.txt")"
  failures_json="$(json_lines "$OUT_DIR/t3-failures.txt")"

  append_tier_result "\"t3\": {
      \"pass\": $pass,
      \"audio_devices_found\": $audio_devices_found,
      \"mic_tcc_granted\": $mic_tcc_granted,
      \"screen_tcc_granted\": $screen_tcc_granted,
      \"virtual_audio_driver_found\": $virtual_audio_driver_found,
      \"notes\": $notes_json,
      \"failures\": $failures_json,
      \"artifacts\": [\"ipc-result.json\", \"t3-app.out\", \"t3-app.err\", \"t3-system.log\", \"tcc-dump.txt\"]
    }"
}

write_result() {
  if [ -z "$TIER_RESULTS" ]; then
    cat > "$OUT_DIR/result.json" <<JSON
{
  "machine": { "name": "$(json_escape "$MACHINE_NAME")" },
  "status": "skipped",
  "generated_at": "$(date -u '+%Y-%m-%dT%H:%M:%SZ')",
  "tiers": {}
}
JSON
  else
    cat > "$OUT_DIR/result.json" <<JSON
{
  "machine": { "name": "$(json_escape "$MACHINE_NAME")" },
  "status": "ok",
  "generated_at": "$(date -u '+%Y-%m-%dT%H:%M:%SZ')",
  "tiers": {
    $TIER_RESULTS
  }
}
JSON
  fi
}

write_machine_info

case ",$TIERS," in
  *,t1,*) run_t1 ;;
esac

case ",$TIERS," in
  *,t2,*) run_t2 ;;
esac

case ",$TIERS," in
  *,t3,*) run_t3 ;;
esac

write_result
