#!/usr/bin/env bash
# End-to-end check for milestone 2: carson-server (dev mode) + glenley-agent.
#   pair by browser sign-in (curl plays the browser: grant → agent's 127.0.0.1 hand-off)
#   → agent online → agent stops → offline.
# Usage: scripts/e2e-pairing.sh   (needs go, cargo, curl; runs on macOS and Linux)
set -euo pipefail

ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
PORT=${PORT:-18080}
URL="http://127.0.0.1:$PORT"
JAR="$WORK/cookies"

cleanup() {
  local pids
  pids=$(jobs -p)
  [ -n "$pids" ] && kill $pids 2>/dev/null || true
  wait 2>/dev/null || true
  rm -rf "$WORK"
}
trap cleanup EXIT

fail() {
  echo "FAIL: $*" >&2
  for f in server.log pair.out agent.log; do
    [ -f "$WORK/$f" ] && { echo "--- $f" >&2; tail -20 "$WORK/$f" >&2; }
  done
  exit 1
}

# wait_for <seconds> <command...>: retry a command until it succeeds.
wait_for() {
  local deadline=$((SECONDS + $1)); shift
  until "$@" >/dev/null 2>&1; do
    [ $SECONDS -ge $deadline ] && return 1
    sleep 0.2
  done
}

echo "building…"
(cd "$ROOT/server" && go build -o "$WORK/carson-server" ./cmd/carson-server)
(cd "$ROOT/agent" && cargo build -q -p carson-agent)
AGENT="$ROOT/agent/target/debug/glenley-agent"

CARSON_DEV=true CARSON_ADDR="127.0.0.1:$PORT" CARSON_PUBLIC_URL="$URL" CARSON_DB_PATH="$WORK/carson.db" \
  "$WORK/carson-server" >"$WORK/server.log" 2>&1 &
wait_for 15 curl -fsS "$URL/readyz" || fail "server did not become ready"
echo "server up at $URL"

export CARSON_AGENT_DIR="$WORK/agent" CARSON_AGENT_KEYSTORE=file
"$AGENT" pair --server "$URL" --name "e2e host" >"$WORK/pair.out" 2>&1 &
PAIR_PID=$!
wait_for 15 grep -q '#/connect?' "$WORK/pair.out" || fail "agent did not print a sign-in link"
LINK=$(grep -o "$URL/#/connect?[^ ]*" "$WORK/pair.out")
param() { printf '%s' "$LINK" | sed -n "s/.*[?&]$1=\([^&]*\).*/\1/p"; }
PAIRING=$(param pairing) LOOP_PORT=$(param port) STATE=$(param state)
echo "sign-in link for $PAIRING (loopback port $LOOP_PORT)"

api() { # api <path> <json>: POST as the signed-in browser user
  curl -fsS -b "$JAR" -c "$JAR" -H "Origin: $URL" -H 'Content-Type: application/json' -d "$2" "$URL$1"
}
api /auth/dev/login '{"email":"e2e@example.com"}' >/dev/null || fail "dev login"
# What the #/connect page does: get a one-time grant, hand it to the agent on 127.0.0.1.
GRANT=$(api /v1/pair/grant "{\"pairing_id\":\"$PAIRING\"}" | sed -n 's/.*"grant":"\([^"]*\)".*/\1/p')
[ -n "$GRANT" ] || fail "no grant"
curl -fsS -o /dev/null -w '%{http_code}' "http://127.0.0.1:$LOOP_PORT/done?state=$STATE&grant=$GRANT" | grep -q 302 ||
  fail "agent did not accept the hand-off"
wait $PAIR_PID || fail "agent pair exited non-zero"
grep -q '^Added "e2e host" to your account (dev_' "$WORK/pair.out" || fail "agent did not confirm pairing"
echo "paired (browser sign-in, no code)"

devices() { curl -fsS -b "$JAR" "$URL/v1/devices"; }
"$AGENT" run >"$WORK/agent.log" 2>&1 &
RUN_PID=$!
wait_for 15 sh -c "curl -fsS -b '$JAR' '$URL/v1/devices' | grep -q '\"online\":true'" || fail "device never came online"
echo "device online"

kill -INT $RUN_PID
wait $RUN_PID 2>/dev/null || true
wait_for 10 sh -c "curl -fsS -b '$JAR' '$URL/v1/devices' | grep -q '\"online\":false'" || fail "device never went offline"
echo "device offline after agent stopped"

"$AGENT" status | grep -q '^paired' || fail "status"

# Join with a code, "just this once", then remove it from the computer's side.
export CARSON_AGENT_DIR="$WORK/agent-code"
"$AGENT" pair --server "$URL" --name "code host" --code --once >"$WORK/pair-code.out" 2>&1 &
CODE_PID=$!
wait_for 15 grep -q 'Connection code:' "$WORK/pair-code.out" || fail "agent did not print a connection code"
CODE=$(sed -n 's/.*Connection code:  *\([A-Z-]*\).*/\1/p' "$WORK/pair-code.out")
api /v1/pair/lookup "{\"code\":\"$CODE\"}" | grep -q '"temporary":true' || fail "lookup did not show a one-time share"
api /v1/pair/approve "{\"code\":\"$CODE\"}" >/dev/null || fail "approve code"
wait $CODE_PID || fail "code pairing exited non-zero"
devices | grep -q '"name":"code host".*"temporary":true\|"temporary":true.*"name":"code host"' || fail "one-time device not listed"
echo "paired with a code (one-time)"
"$AGENT" leave >/dev/null || fail "leave"
devices | grep -q '"name":"code host"' && fail "device still listed after leave"
echo "removed from the computer's side"
echo "PASS"
