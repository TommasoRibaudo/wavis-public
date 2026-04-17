#!/usr/bin/env bash
# run-capacity-test.sh — Orchestrate a capacity load test against the LiveKit SFU.
#
# Automates: start benchmark instance, run ramp test + server monitor via SSM,
# collect results, display summary, stop instance.
#
# Usage:
#   bash deploy/run-capacity-test.sh [OPTIONS]
#   bash deploy/run-capacity-test.sh --peers 3 --step 1 --max-rooms 8 --duration 300s

set -euo pipefail

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------
BENCHMARK_ID="i-0123456789abcdef0"
LIVEKIT_ID="i-0123456789abcdef0"
REGION="${AWS_REGION:-us-east-2}"
RESULTS_DIR="doc/testing/results"
TIMESTAMP=$(date +%Y%m%d-%H%M%S)

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------
PEERS=4
STEP=1
MAX_ROOMS=8
DURATION="300s"
INTERVAL=90
SKIP_STOP=false

# ---------------------------------------------------------------------------
# CLI argument parsing
# ---------------------------------------------------------------------------
usage() {
  cat <<'EOF'
Usage: bash deploy/run-capacity-test.sh [OPTIONS]

Orchestrate a capacity load test against the LiveKit SFU via AWS SSM.

Options:
  --peers N        Peers per room (default: 4)
  --step N         Rooms added per ramp step (default: 1)
  --max-rooms N    Maximum number of rooms (default: 8)
  --duration T     Duration each room stays active, e.g. 300s (default: 300s)
  --interval N     Seconds between ramp steps (default: 90)
  --skip-stop      Don't stop benchmark instance after test
  --help           Show this help message

Examples:
  # Quick 3-room test
  bash deploy/run-capacity-test.sh --peers 3 --step 1 --max-rooms 3 --duration 120s --interval 60

  # Full ramp to 8 rooms
  bash deploy/run-capacity-test.sh --peers 4 --step 1 --max-rooms 8 --duration 300s
EOF
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --peers)     PEERS="$2";     shift 2 ;;
    --step)      STEP="$2";      shift 2 ;;
    --max-rooms) MAX_ROOMS="$2"; shift 2 ;;
    --duration)  DURATION="$2";  shift 2 ;;
    --interval)  INTERVAL="$2";  shift 2 ;;
    --skip-stop) SKIP_STOP=true; shift   ;;
    --help)      usage ;;
    *)
      echo "ERROR: Unknown option: $1" >&2
      echo "Run with --help for usage." >&2
      exit 1
      ;;
  esac
done

# Strip trailing 's' from duration for arithmetic
DURATION_SECS="${DURATION%s}"

# ---------------------------------------------------------------------------
# SSM helper functions
# ---------------------------------------------------------------------------

# ssm_run <instance_id> <timeout> <command>
# Sends an SSM command and prints the command ID.
ssm_run() {
  local instance_id="$1"
  local timeout="$2"
  local command="$3"

  aws ssm send-command \
    --instance-ids "$instance_id" \
    --document-name "AWS-RunShellScript" \
    --timeout-seconds "$timeout" \
    --parameters "{\"commands\":[\"$command\"]}" \
    --region "$REGION" \
    --output text \
    --query 'Command.CommandId'
}

# ssm_wait <cmd_id> <instance_id> <max_wait> <label>
# Polls command status every 15s until terminal state or timeout.
ssm_wait() {
  local cmd_id="$1"
  local instance_id="$2"
  local max_wait="$3"
  local label="$4"
  local elapsed=0

  echo "  Waiting for: $label (timeout: ${max_wait}s)"
  while [[ $elapsed -lt $max_wait ]]; do
    local status
    status=$(aws ssm get-command-invocation \
      --command-id "$cmd_id" \
      --instance-id "$instance_id" \
      --region "$REGION" \
      --output text \
      --query 'Status' 2>/dev/null || echo "Pending")

    case "$status" in
      Success)
        echo "  $label: completed (${elapsed}s)"
        return 0
        ;;
      Failed|TimedOut|Cancelled|Cancelling)
        echo "  ERROR: $label status: $status (${elapsed}s)" >&2
        return 1
        ;;
      *)
        printf "  [%3ds] %s: %s\r" "$elapsed" "$label" "$status"
        sleep 15
        elapsed=$((elapsed + 15))
        ;;
    esac
  done

  echo ""
  echo "  ERROR: $label timed out after ${max_wait}s" >&2
  return 1
}

# ssm_output <cmd_id> <instance_id>
# Returns the StandardOutputContent of a completed command.
ssm_output() {
  local cmd_id="$1"
  local instance_id="$2"

  aws ssm get-command-invocation \
    --command-id "$cmd_id" \
    --instance-id "$instance_id" \
    --region "$REGION" \
    --output text \
    --query 'StandardOutputContent'
}

# wait_ssm_ready <instance_id> <timeout>
# Polls describe-instance-information until the instance is Online.
wait_ssm_ready() {
  local instance_id="$1"
  local timeout="$2"
  local elapsed=0

  echo "  Waiting for SSM agent on $instance_id..."
  while [[ $elapsed -lt $timeout ]]; do
    local ping
    ping=$(aws ssm describe-instance-information \
      --filters "Key=InstanceIds,Values=$instance_id" \
      --region "$REGION" \
      --output text \
      --query 'InstanceInformationList[0].PingStatus' 2>/dev/null || echo "Offline")

    if [[ "$ping" == "Online" ]]; then
      echo "  SSM agent online (${elapsed}s)"
      return 0
    fi

    printf "  [%3ds] SSM status: %s\r" "$elapsed" "$ping"
    sleep 15
    elapsed=$((elapsed + 15))
  done

  echo ""
  echo "  ERROR: SSM agent not online after ${timeout}s" >&2
  return 1
}

# ---------------------------------------------------------------------------
# Trap cleanup handler
# ---------------------------------------------------------------------------
cleanup() {
  local exit_code=$?
  echo ""
  if [[ "$SKIP_STOP" == "false" ]]; then
    echo "--- Cleanup: stopping benchmark instance $BENCHMARK_ID ---"
    aws ec2 stop-instances --instance-ids "$BENCHMARK_ID" --region "$REGION" \
      > /dev/null 2>&1 || true
    echo "  Stop request sent."
  else
    echo "--- Cleanup: --skip-stop set, benchmark instance left running ---"
  fi
  if [[ $exit_code -ne 0 ]]; then
    echo "Script exited with error (code $exit_code). Check output above."
  fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Main orchestration
# ---------------------------------------------------------------------------
echo "=== Wavis Capacity Load Test ==="
echo ""
echo "Configuration:"
echo "  Peers/room: $PEERS | Step: +$STEP | Max rooms: $MAX_ROOMS"
echo "  Duration: $DURATION | Interval: ${INTERVAL}s"
echo "  Benchmark: $BENCHMARK_ID | LiveKit: $LIVEKIT_ID"
echo "  Region: $REGION"
echo ""

# --- 1. Validate AWS CLI ---------------------------------------------------
echo "--- 1. Validating AWS CLI ---"
if ! command -v aws &>/dev/null; then
  echo "ERROR: aws CLI not found. Install it first." >&2
  exit 1
fi
aws sts get-caller-identity --region "$REGION" > /dev/null
echo "  AWS credentials OK"

# --- 2. Start benchmark instance if stopped --------------------------------
echo "--- 2. Starting benchmark instance ---"
BENCH_STATE=$(aws ec2 describe-instances \
  --instance-ids "$BENCHMARK_ID" \
  --region "$REGION" \
  --output text \
  --query 'Reservations[0].Instances[0].State.Name')

if [[ "$BENCH_STATE" == "running" ]]; then
  echo "  Already running"
elif [[ "$BENCH_STATE" == "stopped" ]]; then
  echo "  Instance is stopped, starting..."
  aws ec2 start-instances --instance-ids "$BENCHMARK_ID" --region "$REGION" > /dev/null
  echo "  Waiting for instance to reach running state..."
  aws ec2 wait instance-running --instance-ids "$BENCHMARK_ID" --region "$REGION"
  echo "  Instance running"
else
  echo "  Instance state: $BENCH_STATE — waiting for it to settle..."
  sleep 30
fi

wait_ssm_ready "$BENCHMARK_ID" 120

# --- 3. Verify LiveKit instance SSM reachable ------------------------------
echo "--- 3. Verifying LiveKit instance ---"
LK_PING=$(aws ssm describe-instance-information \
  --filters "Key=InstanceIds,Values=$LIVEKIT_ID" \
  --region "$REGION" \
  --output text \
  --query 'InstanceInformationList[0].PingStatus' 2>/dev/null || echo "Offline")

if [[ "$LK_PING" != "Online" ]]; then
  echo "  ERROR: LiveKit instance $LIVEKIT_ID SSM is not online (status: $LK_PING)" >&2
  echo "  Make sure the instance is running." >&2
  exit 1
fi
echo "  LiveKit SSM agent online"

# --- 4. Calculate durations ------------------------------------------------
echo "--- 4. Calculating test durations ---"
# Total ramp test time: rooms ramp up over (max_rooms/step - 1) * interval seconds,
# plus the duration of each room. The last room starts at the end of ramp.
NUM_STEPS=$(( (MAX_ROOMS + STEP - 1) / STEP ))
RAMP_TIME=$(( (NUM_STEPS - 1) * INTERVAL ))
TOTAL_TEST_TIME=$(( RAMP_TIME + DURATION_SECS ))
# The first room starts immediately and must still be alive through the fixed
# 3-minute soak phase, otherwise completed generators will be miscounted as
# failed processes during the later ramp/soak checks.
MIN_VALID_DURATION=$(( RAMP_TIME + 180 ))
# Monitor should run longer than the test
MONITOR_DURATION=$(( TOTAL_TEST_TIME + 120 ))
# SSM timeout needs generous headroom for lk load-test startup/teardown per room
SSM_TIMEOUT_RAMP=$(( TOTAL_TEST_TIME + 300 ))
SSM_TIMEOUT_MONITOR=$(( MONITOR_DURATION + 300 ))

echo "  Ramp steps: $NUM_STEPS | Ramp time: ${RAMP_TIME}s"
echo "  Total test time: ~${TOTAL_TEST_TIME}s | Monitor: ${MONITOR_DURATION}s"

if [[ "$DURATION_SECS" -lt "$MIN_VALID_DURATION" ]]; then
  echo "ERROR: --duration ${DURATION_SECS}s is too short for this ramp." >&2
  echo "The first room would end before the ramp + soak phase completes." >&2
  echo "Use at least ${MIN_VALID_DURATION}s for a valid result." >&2
  exit 1
fi

# --- 5. Start monitor on LiveKit instance (fire-and-forget) ----------------
echo "--- 5. Starting server monitor on LiveKit instance ---"
MONITOR_CMD="/opt/wavis-loadtest/monitor.sh 10 $MONITOR_DURATION 2>&1"
MONITOR_CMD_ID=$(ssm_run "$LIVEKIT_ID" "$SSM_TIMEOUT_MONITOR" "$MONITOR_CMD")
echo "  Monitor command ID: $MONITOR_CMD_ID"

# --- 6. Run ramp test on benchmark instance --------------------------------
echo "--- 6. Running ramp test on benchmark instance ---"
RAMP_CMD="/opt/wavis-loadtest/ramp-test.sh $PEERS $STEP $MAX_ROOMS $DURATION $INTERVAL 2>&1"
RAMP_CMD_ID=$(ssm_run "$BENCHMARK_ID" "$SSM_TIMEOUT_RAMP" "$RAMP_CMD")
echo "  Ramp command ID: $RAMP_CMD_ID"

ssm_wait "$RAMP_CMD_ID" "$BENCHMARK_ID" "$SSM_TIMEOUT_RAMP" "Ramp test"

# --- 7. Wait for monitor to finish ----------------------------------------
echo "--- 7. Waiting for monitor to complete ---"
# Give the monitor some time to finish after the ramp test
MONITOR_REMAINING=$(( MONITOR_DURATION - TOTAL_TEST_TIME ))
if [[ $MONITOR_REMAINING -gt 0 ]]; then
  echo "  Monitor has ~${MONITOR_REMAINING}s remaining..."
fi
ssm_wait "$MONITOR_CMD_ID" "$LIVEKIT_ID" "$SSM_TIMEOUT_MONITOR" "Server monitor"

# --- 8. Collect outputs ----------------------------------------------------
echo "--- 8. Collecting results ---"

# Fetch the actual benchmark artifacts from disk instead of relying on SSM
# stdout, which can truncate the detailed lk load-test tables we need later.
echo "  Fetching ramp test log..."
RAMP_LOG_FETCH_CMD_ID=$(ssm_run "$BENCHMARK_ID" 60 "latest_log=\$(ls -1t /opt/wavis-loadtest/results/ramp-*.log 2>/dev/null | head -n1); if [[ -n \"\$latest_log\" ]]; then cat \"\$latest_log\"; else echo 'no ramp log'; fi")
sleep 5
ssm_wait "$RAMP_LOG_FETCH_CMD_ID" "$BENCHMARK_ID" 60 "Ramp log fetch"
RAMP_LOG=$(ssm_output "$RAMP_LOG_FETCH_CMD_ID" "$BENCHMARK_ID")

# Benchmark CSVs
echo "  Fetching benchmark CSVs..."
BENCH_CSV_CMD_ID=$(ssm_run "$BENCHMARK_ID" 60 "latest_csv=\$(ls -1t /opt/wavis-loadtest/results/ramp-*.csv 2>/dev/null | head -n1); if [[ -n \"\$latest_csv\" ]]; then cat \"\$latest_csv\"; else echo 'no CSV files'; fi")
sleep 5
ssm_wait "$BENCH_CSV_CMD_ID" "$BENCHMARK_ID" 60 "Benchmark CSV fetch"
BENCH_CSV=$(ssm_output "$BENCH_CSV_CMD_ID" "$BENCHMARK_ID")

# Monitor CSV from LiveKit instance
echo "  Fetching monitor CSV..."
MONITOR_CSV_CMD_ID=$(ssm_run "$LIVEKIT_ID" 60 "latest_csv=\$(ls -1t /opt/wavis-loadtest/results/*.csv 2>/dev/null | head -n1); if [[ -n \"\$latest_csv\" ]]; then cat \"\$latest_csv\"; else echo 'no CSV files'; fi")
sleep 5
ssm_wait "$MONITOR_CSV_CMD_ID" "$LIVEKIT_ID" 60 "Monitor CSV fetch"
MONITOR_CSV=$(ssm_output "$MONITOR_CSV_CMD_ID" "$LIVEKIT_ID")

# --- 9. Save raw results locally ------------------------------------------
echo "--- 9. Saving results ---"
mkdir -p "$RESULTS_DIR"

RAMP_LOG_FILE="$RESULTS_DIR/capacity-${TIMESTAMP}-ramp.log"
MONITOR_CSV_FILE="$RESULTS_DIR/capacity-${TIMESTAMP}-monitor.csv"
SUMMARY_FILE="$RESULTS_DIR/capacity-${TIMESTAMP}-summary.md"

echo "$RAMP_LOG" > "$RAMP_LOG_FILE"
echo "$MONITOR_CSV" > "$MONITOR_CSV_FILE"
echo "  Saved: $RAMP_LOG_FILE"
echo "  Saved: $MONITOR_CSV_FILE"

# --- 10. Parse and display summary ----------------------------------------
echo "--- 10. Results Summary ---"
echo ""

# Parse per-room results from ramp log
# The lk load-test output has table rows like:
#   │ Total  │ 18/18    │ 14.1mbps (2.0mbps avg) │ 42 (0.332%)      │ 0     │
parse_room_results() {
  local log="$1"
  local room_num=0
  local max_ok_rooms=0
  local results=""

  # Extract "Total" lines from each room's load test output
  while IFS= read -r line; do
    # Match lines containing "Total" with track/bitrate/loss data
    if echo "$line" | grep -qE '│\s*Total\s*│'; then
      room_num=$((room_num + 1))

      # Extract tracks (e.g., "18/18")
      local tracks
      tracks=$(echo "$line" | awk -F'│' '{print $3}' | xargs)

      # Extract bitrate (e.g., "14.1mbps")
      local bitrate
      bitrate=$(echo "$line" | awk -F'│' '{print $4}' | sed 's/(.*//' | xargs)

      # Extract packet loss percentage (e.g., "0.332%")
      local loss_pct
      loss_pct=$(echo "$line" | awk -F'│' '{print $5}' | grep -oE '[0-9]+\.[0-9]+%' | head -1)
      if [[ -z "$loss_pct" ]]; then
        loss_pct="0.000%"
      fi

      # Extract error count
      local errors
      errors=$(echo "$line" | awk -F'│' '{print $6}' | xargs)

      # Determine status based on packet loss
      local loss_num
      loss_num=$(echo "$loss_pct" | tr -d '%')
      local status="OK"

      if echo "$line" | grep -qiE 'error|fail'; then
        status="FAILED"
      elif awk "BEGIN {exit !($loss_num > 5)}"; then
        status="FAILED"
      elif awk "BEGIN {exit !($loss_num > 1)}"; then
        status="DEGRADED"
      else
        max_ok_rooms=$room_num
      fi

      results+="  Room $room_num: $tracks tracks | $bitrate | ${loss_pct} loss | $status"$'\n'
    fi
  done <<< "$log"

  echo "$results"
  # Store max_ok_rooms for conclusion
  echo "MAX_OK_ROOMS=$max_ok_rooms" > /tmp/capacity_test_max_rooms
}

# Parse monitor CSV for peak docker CPU and memory
parse_monitor_peaks() {
  local csv="$1"

  if [[ "$csv" == "no CSV files" || -z "$csv" ]]; then
    echo "  (no monitor data available)"
    return
  fi

  # Expect CSV with columns like: timestamp,docker_cpu,docker_mem_used,docker_mem_limit,...
  # Try to extract peak values
  local peak_cpu peak_mem mem_limit

  # Skip header, find peak docker CPU (column 2) and memory (column 3)
  peak_cpu=$(echo "$csv" | tail -n +2 | awk -F',' '{if ($2+0 > max) max=$2+0} END {printf "%.2f%%", max}' 2>/dev/null || echo "N/A")
  peak_mem=$(echo "$csv" | tail -n +2 | awk -F',' '{if ($3+0 > max) max=$3+0} END {printf "%.0f MB", max}' 2>/dev/null || echo "N/A")
  mem_limit=$(echo "$csv" | tail -n +2 | awk -F',' 'NR==1 {printf "%.0f MB", $4+0}' 2>/dev/null || echo "N/A")

  echo "  Docker CPU: $peak_cpu | Docker Memory: $peak_mem / $mem_limit"
}

ROOM_RESULTS=$(parse_room_results "$RAMP_LOG")

# Read back max_ok_rooms
MAX_OK_ROOMS=0
if [[ -f /tmp/capacity_test_max_rooms ]]; then
  source /tmp/capacity_test_max_rooms
  rm -f /tmp/capacity_test_max_rooms
fi

cat <<EOF

=== Capacity Test Results ===

Configuration:
  Peers/room: $PEERS | Step: +$STEP | Max: $MAX_ROOMS | Duration: $DURATION | Interval: ${INTERVAL}s
  LiveKit instance: t3.small ($LIVEKIT_ID)

Per-Room Results:
$ROOM_RESULTS
Server Metrics (peak):
$(parse_monitor_peaks "$MONITOR_CSV")

Conclusion:
  Max rooms at <1% packet loss: $MAX_OK_ROOMS
  (Check ramp log for detailed per-room analysis)

EOF

# --- 11. Clean up rooms ---------------------------------------------------
echo "--- 11. Cleaning up rooms ---"
CLEANUP_CMD_ID=$(ssm_run "$BENCHMARK_ID" 120 "/opt/wavis-loadtest/cleanup.sh 2>&1")
ssm_wait "$CLEANUP_CMD_ID" "$BENCHMARK_ID" 120 "Room cleanup"
echo "  Rooms cleaned up"

# --- 12. Save summary markdown -------------------------------------------
echo "--- 12. Saving summary ---"

cat > "$SUMMARY_FILE" <<EOF
# Capacity Test Results — $(date '+%Y-%m-%d %H:%M:%S')

## Configuration

| Parameter | Value |
|-----------|-------|
| Peers/room | $PEERS |
| Step | +$STEP |
| Max rooms | $MAX_ROOMS |
| Duration | $DURATION |
| Interval | ${INTERVAL}s |
| LiveKit instance | t3.small ($LIVEKIT_ID) |
| Benchmark instance | c7i.2xlarge ($BENCHMARK_ID) |

## Per-Room Results

\`\`\`
$ROOM_RESULTS
\`\`\`

## Server Metrics (peak)

\`\`\`
$(parse_monitor_peaks "$MONITOR_CSV")
\`\`\`

## Conclusion

- Max rooms at <1% packet loss: $MAX_OK_ROOMS

## Raw Data

- Ramp log: \`capacity-${TIMESTAMP}-ramp.log\`
- Monitor CSV: \`capacity-${TIMESTAMP}-monitor.csv\`
EOF

echo "  Saved: $SUMMARY_FILE"
echo ""

# --- 13. Stop benchmark instance (handled by trap) ------------------------
echo "--- 13. Done ---"
echo "Results saved to $RESULTS_DIR/capacity-${TIMESTAMP}-*"
echo ""
# The EXIT trap handles stopping the benchmark instance
