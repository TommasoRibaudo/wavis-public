#!/usr/bin/env bash
# Unit tests for pure helper functions in tools/compat/agent/run-agent.sh
#
# Run on macOS: bash tools/compat/tests/test_agent.sh
# Tests are self-contained — they do NOT connect to any remote machine.
#
# Exit status: 0 if all tests pass, non-zero otherwise.
# Each failing test prints "FAIL: <test name>" and the reason.

set -eu

PASS=0
FAIL=0

# Resolve the agent script path relative to this file.
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AGENT="$SCRIPT_DIR/../agent/run-agent.sh"

if [ ! -f "$AGENT" ]; then
  echo "ERROR: agent script not found at $AGENT" >&2
  exit 1
fi

# ---------------------------------------------------------------------------
# Test harness helpers
# ---------------------------------------------------------------------------

_pass() {
  PASS=$((PASS + 1))
  printf '  ok  %s\n' "$1"
}

_fail() {
  FAIL=$((FAIL + 1))
  printf ' FAIL %s\n' "$1"
  if [ -n "${2:-}" ]; then
    printf '      %s\n' "$2"
  fi
}

assert_eq() {
  local test_name="$1" got="$2" want="$3"
  if [ "$got" = "$want" ]; then
    _pass "$test_name"
  else
    _fail "$test_name" "got='$got' want='$want'"
  fi
}

assert_true() {
  local test_name="$1"
  shift
  if "$@"; then
    _pass "$test_name"
  else
    _fail "$test_name" "command returned non-zero: $*"
  fi
}

assert_false() {
  local test_name="$1"
  shift
  if ! "$@"; then
    _pass "$test_name"
  else
    _fail "$test_name" "command unexpectedly succeeded: $*"
  fi
}

# ---------------------------------------------------------------------------
# Extract and test macos_before()
#
# We source the agent script in a subshell with a sw_vers stub so that the
# version comparisons can be exercised without a real Mac and without running
# any of the agent's main code (APP_PATH / OUT_DIR / MACHINE_NAME are unset
# so the argument-validation block fires — we redirect its stderr and exit).
# ---------------------------------------------------------------------------

# Source the function definitions only; the argument-check at the bottom will
# exit 2 because required args are unset, so we must tolerate that.
_load_agent_fns() {
  local fake_sw_vers_version="$1"
  # shellcheck disable=SC1090
  (
    # Stub sw_vers so macos_before reads our version, not the host's.
    sw_vers() {
      case "$1" in
        -productVersion) printf '%s\n' "$fake_sw_vers_version" ;;
        *) command sw_vers "$@" ;;
      esac
    }
    export -f sw_vers 2>/dev/null || true

    # Source the agent script; tolerate the arg-validation exit(2).
    # shellcheck source=/dev/null
    . "$AGENT" 2>/dev/null || true

    # Now call macos_before with the arguments passed to this subshell.
    macos_before "$2" "$3"
  )
  return $?
}

run_macos_before_tests() {
  printf '\nmacos_before()\n'

  # Helper: assert macos_before returns 0 (true) for a given version + boundary.
  _mb_true() {
    local label="$1" version="$2" maj="$3" min="$4"
    if _load_agent_fns "$version" "$maj" "$min"; then
      _pass "$label"
    else
      _fail "$label" "expected true (return 0) for version=$version boundary=$maj.$min"
    fi
  }

  # Helper: assert macos_before returns 1 (false).
  _mb_false() {
    local label="$1" version="$2" maj="$3" min="$4"
    if ! _load_agent_fns "$version" "$maj" "$min"; then
      _pass "$label"
    else
      _fail "$label" "expected false (return 1) for version=$version boundary=$maj.$min"
    fi
  }

  # --- before the 12.3 SCK boundary ---
  _mb_true  "11.5.2 is before 12.3"          "11.5.2" 12 3
  _mb_true  "11.0 is before 12.3"            "11.0"   12 3
  _mb_true  "12.2 is before 12.3"            "12.2"   12 3
  _mb_true  "10.15 is before 12.3"           "10.15"  12 3

  # --- at or after the 12.3 SCK boundary ---
  _mb_false "12.3 is NOT before 12.3"        "12.3"   12 3
  _mb_false "12.4 is NOT before 12.3"        "12.4"   12 3
  _mb_false "13.0 is NOT before 12.3"        "13.0"   12 3
  _mb_false "13.6 is NOT before 12.3"        "13.6"   12 3
  _mb_false "26.0 is NOT before 12.3"        "26.0"   12 3

  # --- before the 14.2 process-tap boundary ---
  _mb_true  "13.6 is before 14.2"            "13.6"   14 2
  _mb_true  "14.1 is before 14.2"            "14.1"   14 2

  # --- at or after the 14.2 boundary ---
  _mb_false "14.2 is NOT before 14.2"        "14.2"   14 2
  _mb_false "15.0 is NOT before 14.2"        "15.0"   14 2
}

# ---------------------------------------------------------------------------
# Test tcc_auth_value()
#
# This function parses a pipe-separated TCC dump produced by:
#   sqlite3 TCC.db "SELECT service,client,auth_value FROM access WHERE ..."
# ---------------------------------------------------------------------------

run_tcc_auth_value_tests() {
  printf '\ntcc_auth_value()\n'

  # Source the function definitions (tolerating arg-validation exit).
  _tcc_val() {
    local tcc_file="$1" service="$2"
    (
      # shellcheck source=/dev/null
      . "$AGENT" 2>/dev/null || true
      tcc_auth_value "$tcc_file" "$service"
    )
  }

  local tmp
  tmp="$(mktemp)"

  # Microphone granted (auth_value=2)
  printf 'kTCCServiceMicrophone|com.wavis.desktop|2\n' > "$tmp"
  assert_eq "mic granted → 2" \
    "$(_tcc_val "$tmp" "kTCCServiceMicrophone")" "2"

  # Screen capture denied (auth_value=0)
  printf 'kTCCServiceScreenCapture|com.wavis.desktop|0\n' > "$tmp"
  assert_eq "screen denied → 0" \
    "$(_tcc_val "$tmp" "kTCCServiceScreenCapture")" "0"

  # Multiple rows — only the matching service extracted
  printf 'kTCCServiceMicrophone|com.wavis.desktop|2\nkTCCServiceScreenCapture|com.wavis.desktop|0\n' > "$tmp"
  assert_eq "multi-row: mic → 2" \
    "$(_tcc_val "$tmp" "kTCCServiceMicrophone")" "2"
  assert_eq "multi-row: screen → 0" \
    "$(_tcc_val "$tmp" "kTCCServiceScreenCapture")" "0"

  # Service not present → empty string
  printf 'kTCCServiceMicrophone|com.wavis.desktop|2\n' > "$tmp"
  assert_eq "absent service → empty" \
    "$(_tcc_val "$tmp" "kTCCServiceScreenCapture")" ""

  # Empty file → empty string
  : > "$tmp"
  assert_eq "empty file → empty" \
    "$(_tcc_val "$tmp" "kTCCServiceMicrophone")" ""

  rm -f "$tmp"
}

# ---------------------------------------------------------------------------
# Test json_field_status()
#
# Parses {"screen_capture_kit": {"status": "skipped"}} from an ipc-result.json.
# ---------------------------------------------------------------------------

run_json_field_status_tests() {
  printf '\njson_field_status()\n'

  _jfs() {
    local file="$1" field="$2"
    (
      # shellcheck source=/dev/null
      . "$AGENT" 2>/dev/null || true
      json_field_status "$file" "$field"
    )
  }

  local tmp
  tmp="$(mktemp)"

  # Basic case: skipped
  cat > "$tmp" <<'JSON'
{
  "ipc_ok": true,
  "screen_capture_kit": { "status": "skipped" },
  "audio_process_tap": { "status": "available_by_os" }
}
JSON
  assert_eq "sck status skipped" \
    "$(_jfs "$tmp" "screen_capture_kit")" "skipped"

  assert_eq "tap status available_by_os" \
    "$(_jfs "$tmp" "audio_process_tap")" "available_by_os"

  # Status with different value
  cat > "$tmp" <<'JSON'
{ "screen_capture_kit": { "status": "not_applicable" } }
JSON
  assert_eq "sck status not_applicable" \
    "$(_jfs "$tmp" "screen_capture_kit")" "not_applicable"

  # Field absent → empty string
  cat > "$tmp" <<'JSON'
{ "ipc_ok": true }
JSON
  assert_eq "absent field → empty" \
    "$(_jfs "$tmp" "screen_capture_kit")" ""

  # Multiple fields — correct one selected, not the first "status" match
  cat > "$tmp" <<'JSON'
{
  "audio_process_tap": { "status": "skipped" },
  "screen_capture_kit": { "status": "available_by_os" }
}
JSON
  assert_eq "correct field selected among multiple" \
    "$(_jfs "$tmp" "screen_capture_kit")" "available_by_os"

  rm -f "$tmp"
}

# ---------------------------------------------------------------------------
# Test result.json is valid JSON after each tier
#
# We run the agent with a fake (non-existent) app path. The agent will set
# APP_BUNDLE_MISSING and still write result.json. We then validate the JSON.
# ---------------------------------------------------------------------------

run_result_json_validity_tests() {
  printf '\nresult.json is valid JSON\n'

  if ! command -v python3 >/dev/null 2>&1; then
    printf '  skip  (python3 not available)\n'
    return
  fi

  local tmp_out
  tmp_out="$(mktemp -d)"

  # Run the agent for t1 only; app does not exist → APP_BUNDLE_MISSING failure.
  bash "$AGENT" \
    --app "/nonexistent/Wavis.app" \
    --out "$tmp_out" \
    --machine "test-machine" \
    --tiers "t1" \
    --timeout "5" \
    2>/dev/null || true

  if [ ! -f "$tmp_out/result.json" ]; then
    _fail "result.json written" "file was not created"
  else
    _pass "result.json written"
    if python3 -m json.tool "$tmp_out/result.json" > /dev/null 2>&1; then
      _pass "result.json is valid JSON (t1 failure path)"
    else
      _fail "result.json is valid JSON (t1 failure path)" \
        "file content: $(cat "$tmp_out/result.json")"
    fi
  fi

  rm -rf "$tmp_out"
}

# ---------------------------------------------------------------------------
# Run all test groups
# ---------------------------------------------------------------------------

printf 'tools/compat/agent/run-agent.sh unit tests\n'
printf '==========================================\n'

run_macos_before_tests
run_tcc_auth_value_tests
run_json_field_status_tests
run_result_json_validity_tests

printf '\n==========================================\n'
printf 'Results: %d passed, %d failed\n' "$PASS" "$FAIL"

if [ "$FAIL" -gt 0 ]; then
  exit 1
fi
