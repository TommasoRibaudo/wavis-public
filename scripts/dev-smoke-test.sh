#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${BASE_URL:-https://<dev-cloudfront-url>}"
WS_URL="${WS_URL:-${BASE_URL/https:/wss:}/ws}"
ROOM_ID="smoke-$(date +%s)"
HOST_NAME="SmokeHost"
GUEST_NAME="SmokeGuest"

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "Missing required command: $1" >&2
    exit 1
  }
}

need cargo
need curl
need jq
need timeout

host_log="$(mktemp)"
guest_log="$(mktemp)"

cleanup() {
  if [[ -n "${host_pid:-}" ]]; then
    kill "$host_pid" >/dev/null 2>&1 || true
    wait "$host_pid" >/dev/null 2>&1 || true
  fi
  rm -f "$host_log" "$guest_log"
}
trap cleanup EXIT

echo "Checking backend health at $BASE_URL/health"
health_json="$(curl --fail --silent --show-error --max-time 15 "$BASE_URL/health")"
echo "$health_json" | jq -e '.ok == true and .sfu.available == true' >/dev/null

echo "Starting host create_room smoke flow for room $ROOM_ID"
cargo run -q -p ws-sfu-test -- \
  --url "$WS_URL" \
  --send-json "{\"type\":\"create_room\",\"roomId\":\"$ROOM_ID\",\"roomType\":\"sfu\",\"displayName\":\"$HOST_NAME\"}" \
  --expect-type room_created \
  --expect-type media_token \
  --stay-open-secs 12 \
  --timeout-secs 20 \
  --json-output >"$host_log" 2>&1 &
host_pid=$!

invite_code=""
for _ in $(seq 1 20); do
  if invite_code="$(jq -r 'select(.type=="room_created") | .inviteCode // empty' "$host_log" | head -n1)"; then
    :
  fi
  if [[ -n "$invite_code" ]]; then
    break
  fi
  sleep 1
done

if [[ -z "$invite_code" ]]; then
  echo "Host smoke flow never produced an invite code" >&2
  cat "$host_log" >&2
  exit 1
fi

echo "Joining room with guest smoke flow"
cargo run -q -p ws-sfu-test -- \
  --url "$WS_URL" \
  --send-json "{\"type\":\"join\",\"roomId\":\"$ROOM_ID\",\"roomType\":\"sfu\",\"inviteCode\":\"$invite_code\",\"displayName\":\"$GUEST_NAME\"}" \
  --expect-type joined \
  --expect-type media_token \
  --timeout-secs 20 \
  --json-output >"$guest_log" 2>&1

joined_room="$(jq -r 'select(.type=="joined") | .roomId // empty' "$guest_log" | head -n1)"
guest_sfu_url="$(jq -r 'select(.type=="media_token") | .sfuUrl // empty' "$guest_log" | head -n1)"

if [[ "$joined_room" != "$ROOM_ID" ]]; then
  echo "Guest joined unexpected room: $joined_room" >&2
  cat "$guest_log" >&2
  exit 1
fi

if [[ -z "$guest_sfu_url" ]]; then
  echo "Guest smoke flow did not receive a media_token sfuUrl" >&2
  cat "$guest_log" >&2
  exit 1
fi

for _ in $(seq 1 10); do
  if jq -e 'select(.type=="participant_joined")' "$host_log" >/dev/null 2>&1; then
    break
  fi
  sleep 1
done

if ! jq -e 'select(.type=="participant_joined")' "$host_log" >/dev/null 2>&1; then
  echo "Host smoke flow never observed participant_joined" >&2
  cat "$host_log" >&2
  exit 1
fi

echo "Dev smoke test passed"
echo "room_id=$ROOM_ID"
echo "invite_code=$invite_code"
echo "guest_sfu_url=$guest_sfu_url"
