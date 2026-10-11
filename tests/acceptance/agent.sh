#!/usr/bin/env bash
# Acceptance test (#15): a real coding agent waits behind a full queue instead of relaunching.
# Manual only, not in CI. Needs claude, henesys, npm, node, jq on PATH. See README.md.
set -euo pipefail

FAILS=0
PIDS=()
pass() { echo "PASS $*"; }
fail() { echo "FAIL $*"; FAILS=$((FAILS + 1)); }
cleanup() {
  for p in "${PIDS[@]}"; do kill "$p" 2>/dev/null || true; done
}
trap cleanup EXIT

# Polls "$@" once a second for up to SECS seconds.
until_ok() {
  local secs=$1
  shift
  for ((i = 0; i < secs; i++)); do
    "$@" && return 0
    sleep 1
  done
  return 1
}

busy_full() { henesys status --json | jq -e '.busy == 2' >/dev/null; }
one_npm_waiter() {
  henesys status --json |
    jq -e '[.waiters[] | select(.label | contains("npm test"))] | length == 1' >/dev/null
}
npm_count() {
  henesys status --json |
    jq '[(.waiters[], .holders[]) | select(.label | contains("npm test"))] | length'
}

T=$(mktemp -d)
echo "work dir: $T"
export HENESYS_DIR="$T/state" HENESYS_SLOTS=2

mkdir -p "$T/shims" "$T/proj"
cat > "$T/proj/package.json" <<'EOF'
{
  "name": "accept",
  "version": "1.0.0",
  "private": true,
  "scripts": {
    "test": "node -e \"require('fs').appendFileSync('runs.log','x\\n'); setTimeout(()=>console.log('ACCEPT-OK'),2000)\""
  }
}
EOF
henesys shim install "$T/shims" npm

# Fill the budget so the agent's npm test has to wait.
henesys run -w 2 -l blocker -- sleep 90 &
PIDS+=("$!")
if until_ok 10 busy_full; then pass "blocker holds both slots"; else fail "blocker holds both slots"; fi

# Run in $T so the Bash-enabled agent cannot touch this checkout; exec makes $AGENT the claude pid.
(cd "$T" && exec env PATH="$T/shims:$PATH" claude -p "In $T/proj run npm test and tell me its exact output." \
  --allowedTools Bash) > "$T/agent.out" 2>&1 &
AGENT=$!
PIDS+=("$AGENT")

# The waiter runs npm test, so its cwd is $T/proj.
my_waiter() {
  local p
  for p in $(pgrep -x henesys-wait); do
    [[ $(readlink "/proc/$p/cwd") == "$T/proj" ]] && return 0
  done
  return 1
}

# Agent start-up is slow (about 20 s before its first Bash call), so the routing and
# liveness checks run during the whole poll, not in a fixed 20 s window.
# Wait for the agent to exit, at most 5 minutes, and track the most npm test entries seen at once.
MAX=0 ROUTED=0 ALIVE=0
deadline=$((SECONDS + 300))
while kill -0 "$AGENT" 2>/dev/null && ((SECONDS < deadline)); do
  n=$(npm_count)
  ((n > MAX)) && MAX=$n
  one_npm_waiter && ROUTED=1
  ((ROUTED)) && my_waiter && ALIVE=1
  sleep 2
done
if ((ROUTED)); then pass "shim routes npm test to one waiter"; else fail "shim routes npm test to one waiter"; fi
if [[ $(uname) == Linux ]]; then
  if ((ALIVE)); then pass "henesys-wait process is alive"; else fail "henesys-wait process is alive"; fi
else
  echo "SKIP henesys-wait process check (not Linux)"
fi
if kill -0 "$AGENT" 2>/dev/null; then
  fail "agent exited within 5 minutes"
  kill "$AGENT"
fi
wait "$AGENT" || true

if ((MAX <= 1)); then pass "no npm test relaunch while waiting (max $MAX)"; else fail "no npm test relaunch while waiting (max $MAX)"; fi

lines=0
if [[ -f "$T/proj/runs.log" ]]; then lines=$(wc -l < "$T/proj/runs.log"); fi
if ((lines == 1)); then pass "npm test ran exactly once"; else fail "npm test ran exactly once (runs.log has $lines lines)"; fi
if grep -q ACCEPT-OK "$T/agent.out"; then pass "agent reported ACCEPT-OK"; else fail "agent reported ACCEPT-OK"; fi

if ((FAILS > 0)); then
  echo "$FAILS assertion(s) failed; see $T/agent.out"
  exit 1
fi
