#!/usr/bin/env bash
# collect-ec2-docker-stats.sh — Collect host + container metrics from an EC2
# instance via AWS SSM and save them locally as CSV.
#
# This is intended for sizing work such as issue #218, where we need backend
# and LiveKit measurements captured separately during a benchmark window.

set -euo pipefail

INSTANCE_ID=""
CONTAINER_NAME=""
REGION="${AWS_REGION:-us-east-2}"
INTERVAL_SECS=10
DURATION_SECS=300
LABEL=""
RESULTS_DIR="doc/testing/results"

usage() {
  cat <<'EOF'
Usage: bash deploy/collect-ec2-docker-stats.sh [OPTIONS]

Collect host + container metrics from a remote EC2 instance via AWS SSM.

Required:
  --instance-id ID      EC2 instance ID to sample
  --container NAME      Docker container name to sample

Optional:
  --label NAME          Output file label (default: container name)
  --interval SECS       Sampling interval in seconds (default: 10)
  --duration SECS       Total capture duration in seconds (default: 300)
  --region REGION       AWS region (default: AWS_REGION or us-east-2)
  --results-dir PATH    Local results directory (default: doc/testing/results)
  --help                Show this help text

Examples:
  bash deploy/collect-ec2-docker-stats.sh \
    --instance-id i-0123456789abcdef0 \
    --container wavis-backend \
    --label backend-idle \
    --duration 300

  bash deploy/collect-ec2-docker-stats.sh \
    --instance-id i-0123456789abcdef0 \
    --container wavis-livekit \
    --label livekit-ramp \
    --interval 5 \
    --duration 1200
EOF
  exit 0
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --instance-id)
      INSTANCE_ID="$2"
      shift 2
      ;;
    --container)
      CONTAINER_NAME="$2"
      shift 2
      ;;
    --label)
      LABEL="$2"
      shift 2
      ;;
    --interval)
      INTERVAL_SECS="$2"
      shift 2
      ;;
    --duration)
      DURATION_SECS="$2"
      shift 2
      ;;
    --region)
      REGION="$2"
      shift 2
      ;;
    --results-dir)
      RESULTS_DIR="$2"
      shift 2
      ;;
    --help)
      usage
      ;;
    *)
      echo "ERROR: Unknown option: $1" >&2
      echo "Run with --help for usage." >&2
      exit 1
      ;;
  esac
done

if [[ -z "$INSTANCE_ID" ]]; then
  echo "ERROR: --instance-id is required." >&2
  exit 1
fi

if [[ -z "$CONTAINER_NAME" ]]; then
  echo "ERROR: --container is required." >&2
  exit 1
fi

if ! [[ "$INTERVAL_SECS" =~ ^[0-9]+$ ]] || [[ "$INTERVAL_SECS" -le 0 ]]; then
  echo "ERROR: --interval must be a positive integer." >&2
  exit 1
fi

if ! [[ "$DURATION_SECS" =~ ^[0-9]+$ ]] || [[ "$DURATION_SECS" -le 0 ]]; then
  echo "ERROR: --duration must be a positive integer." >&2
  exit 1
fi

if ! command -v aws >/dev/null 2>&1; then
  echo "ERROR: aws CLI not found." >&2
  exit 1
fi

aws sts get-caller-identity --region "$REGION" >/dev/null

TIMESTAMP=$(date +%Y%m%d-%H%M%S)
SAFE_LABEL="${LABEL:-$CONTAINER_NAME}"
SAFE_LABEL="${SAFE_LABEL//[^a-zA-Z0-9._-]/-}"
REMOTE_FILE="/tmp/${SAFE_LABEL}-${TIMESTAMP}.csv"

mkdir -p "$RESULTS_DIR"
LOCAL_FILE="$RESULTS_DIR/${SAFE_LABEL}-${TIMESTAMP}.csv"
SUMMARY_FILE="$RESULTS_DIR/${SAFE_LABEL}-${TIMESTAMP}-summary.txt"

json_escape() {
  local s="$1"
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  s=${s//$'\n'/\\n}
  printf '%s' "$s"
}

ssm_run() {
  local instance_id="$1"
  local timeout_secs="$2"
  local command="$3"
  local payload

  payload=$(printf '{"commands":["%s"]}' "$(json_escape "$command")")

  aws ssm send-command \
    --instance-ids "$instance_id" \
    --document-name "AWS-RunShellScript" \
    --timeout-seconds "$timeout_secs" \
    --parameters "$payload" \
    --region "$REGION" \
    --output text \
    --query 'Command.CommandId'
}

ssm_wait() {
  local cmd_id="$1"
  local instance_id="$2"
  local timeout_secs="$3"
  local elapsed=0

  while [[ $elapsed -lt $timeout_secs ]]; do
    local status
    status=$(aws ssm get-command-invocation \
      --command-id "$cmd_id" \
      --instance-id "$instance_id" \
      --region "$REGION" \
      --output text \
      --query 'Status' 2>/dev/null || echo "Pending")

    case "$status" in
      Success)
        return 0
        ;;
      Failed|TimedOut|Cancelled|Cancelling)
        echo "ERROR: SSM command failed with status: $status" >&2
        return 1
        ;;
      *)
        sleep 5
        elapsed=$((elapsed + 5))
        ;;
    esac
  done

  echo "ERROR: Timed out waiting for SSM command completion." >&2
  return 1
}

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

read -r -d '' REMOTE_SCRIPT <<'EOF' || true
#!/usr/bin/env bash
set -euo pipefail

interval_secs="$1"
duration_secs="$2"
container_name="$3"
output_file="$4"

sample_host_cpu_pct() {
  local total1 idle1 total2 idle2 totald idled
  read -r _ u1 n1 s1 i1 w1 irq1 sirq1 st1 _ < /proc/stat
  total1=$((u1 + n1 + s1 + i1 + w1 + irq1 + sirq1 + st1))
  idle1=$((i1 + w1))
  sleep 1
  read -r _ u2 n2 s2 i2 w2 irq2 sirq2 st2 _ < /proc/stat
  total2=$((u2 + n2 + s2 + i2 + w2 + irq2 + sirq2 + st2))
  idle2=$((i2 + w2))
  totald=$((total2 - total1))
  idled=$((idle2 - idle1))

  if [[ "$totald" -le 0 ]]; then
    echo "0.00"
    return
  fi

  awk -v totald="$totald" -v idled="$idled" 'BEGIN { printf "%.2f", ((totald - idled) * 100) / totald }'
}

echo "timestamp,host_cpu_pct,host_mem_used_mb,host_mem_total_mb,load1,established_tcp,udp_sockets,container_cpu_pct,container_mem_pct,container_mem_usage,container_net_io,container_pids" > "$output_file"

iterations=$(( (duration_secs + interval_secs - 1) / interval_secs ))
if [[ "$iterations" -lt 1 ]]; then
  iterations=1
fi

for ((idx = 0; idx < iterations; idx++)); do
  timestamp=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  host_cpu_pct=$(sample_host_cpu_pct)
  read -r host_mem_used_mb host_mem_total_mb <<<"$(free -m | awk '/^Mem:/ {print $3, $2}')"
  load1=$(awk '{print $1}' /proc/loadavg)
  established_tcp=$(ss -tan state established | tail -n +2 | wc -l | xargs)
  udp_sockets=$(ss -uan | tail -n +2 | wc -l | xargs)

  stats=$(docker stats --no-stream --format '{{.CPUPerc}}|{{.MemPerc}}|{{.MemUsage}}|{{.NetIO}}|{{.PIDs}}' "$container_name" 2>/dev/null || true)
  if [[ -z "$stats" ]]; then
    stats="N/A|N/A|N/A|N/A|N/A"
  fi

  IFS='|' read -r container_cpu_pct container_mem_pct container_mem_usage container_net_io container_pids <<< "$stats"
  container_cpu_pct="${container_cpu_pct%\%}"
  container_mem_pct="${container_mem_pct%\%}"

  printf '%s,%s,%s,%s,%s,%s,%s,%s,%s,"%s","%s",%s\n' \
    "$timestamp" \
    "$host_cpu_pct" \
    "$host_mem_used_mb" \
    "$host_mem_total_mb" \
    "$load1" \
    "$established_tcp" \
    "$udp_sockets" \
    "$container_cpu_pct" \
    "$container_mem_pct" \
    "$container_mem_usage" \
    "$container_net_io" \
    "$container_pids" \
    >> "$output_file"

  if (( idx + 1 < iterations )); then
    sleep "$interval_secs"
  fi
done

cat "$output_file"
EOF

REMOTE_SCRIPT_B64=$(printf '%s' "$REMOTE_SCRIPT" | base64 -w0)
printf -v REMOTE_COMMAND \
  "echo %q | base64 -d >/tmp/wavis-collect-metrics.sh && bash /tmp/wavis-collect-metrics.sh %q %q %q %q" \
  "$REMOTE_SCRIPT_B64" \
  "$INTERVAL_SECS" \
  "$DURATION_SECS" \
  "$CONTAINER_NAME" \
  "$REMOTE_FILE"

SSM_TIMEOUT=$((DURATION_SECS + (DURATION_SECS / INTERVAL_SECS) + 180))

echo "=== Wavis EC2 Metrics Capture ==="
echo "Instance:   $INSTANCE_ID"
echo "Container:  $CONTAINER_NAME"
echo "Region:     $REGION"
echo "Label:      $SAFE_LABEL"
echo "Interval:   ${INTERVAL_SECS}s"
echo "Duration:   ${DURATION_SECS}s"
echo "Remote CSV: $REMOTE_FILE"
echo ""

CMD_ID=$(ssm_run "$INSTANCE_ID" "$SSM_TIMEOUT" "$REMOTE_COMMAND")
echo "SSM command ID: $CMD_ID"
ssm_wait "$CMD_ID" "$INSTANCE_ID" "$SSM_TIMEOUT"
ssm_output "$CMD_ID" "$INSTANCE_ID" > "$LOCAL_FILE"

{
  echo "Metrics capture summary"
  echo "Instance ID: $INSTANCE_ID"
  echo "Container: $CONTAINER_NAME"
  echo "Region: $REGION"
  echo "Local CSV: $LOCAL_FILE"
  echo "Samples: $(tail -n +2 "$LOCAL_FILE" | wc -l | xargs)"
  echo "Peak host CPU: $(tail -n +2 "$LOCAL_FILE" | awk -F',' 'BEGIN { max = 0 } ($2 + 0) > max { max = $2 + 0 } END { printf "%.2f%%", max }')"
  echo "Peak host memory: $(tail -n +2 "$LOCAL_FILE" | awk -F',' 'BEGIN { max_used = 0; total = 0 } ($3 + 0) > max_used { max_used = $3 + 0; total = $4 + 0 } END { printf "%.0f MB / %.0f MB", max_used, total }')"
  echo "Peak container CPU: $(tail -n +2 "$LOCAL_FILE" | awk -F',' 'BEGIN { max = 0 } ($8 + 0) > max { max = $8 + 0 } END { printf "%.2f%%", max }')"
  echo "Peak container memory: $(tail -n +2 "$LOCAL_FILE" | awk -F',' 'BEGIN { max = 0 } ($9 + 0) > max { max = $9 + 0 } END { printf "%.2f%%", max }')"
  echo "Peak established TCP sockets: $(tail -n +2 "$LOCAL_FILE" | awk -F',' 'BEGIN { max = 0 } ($6 + 0) > max { max = $6 + 0 } END { printf "%d", max }')"
  echo "Peak UDP sockets: $(tail -n +2 "$LOCAL_FILE" | awk -F',' 'BEGIN { max = 0 } ($7 + 0) > max { max = $7 + 0 } END { printf "%d", max }')"
} | tee "$SUMMARY_FILE"

echo ""
echo "Saved CSV:     $LOCAL_FILE"
echo "Saved summary: $SUMMARY_FILE"
