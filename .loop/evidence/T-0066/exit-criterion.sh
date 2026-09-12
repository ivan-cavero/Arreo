#!/usr/bin/env bash
# ROADMAP §6, Phase 2 exit criterion:
#   "a stranger pairs a second machine in < 5 min without docs help"
#
# This script is the criterion, executed. It follows only steps the product's own
# messages and docs name, from empty directories, and times itself.
#
# Lessons baked in (each one cost a debugging session):
#  - **Ports are derived from the shell's pid**, not fixed. A leaked relay from an
#    earlier run holding a fixed port makes the next run talk to a *stale* relay
#    with a stale account — which presents as "certificate signature does not
#    verify under the root key", a product-looking error from a harness bug.
#  - **The relay is killed on every exit path** (trap), so runs cannot leak.
#  - **Readiness is polled, never slept for.** `sleep 2` after starting `pair` was
#    a race: if the invite is not on disk yet the next step sees an empty code.
#  - **The account root is registered from `devices list --json`**, because the
#    human-readable line truncates it (`root 17d0a47fbfc70f63…`) and
#    `identity/root.key` holds the *secret* seed. Registering the seed is accepted
#    by the relay and fails much later, which is T-0066's finding.
set -u

PORT_BASE=$(( 21000 + ($$ % 1000) * 3 ))
LISTEN="127.0.0.1:$PORT_BASE"
MAILBOX="127.0.0.1:$((PORT_BASE + 1))"
B=${B:-/tmp/arreo-exit-criterion-$$}
TARGET=${TARGET:-/home/dev/dev/Arreo/target/debug}
A="$TARGET/arreo"
S="$TARGET/arreo-server"
RELAY="$TARGET/arreo-relay"

rm -rf "$B"; mkdir -p "$B"
RELAY_PID=""
cleanup() { [ -n "$RELAY_PID" ] && kill "$RELAY_PID" 2>/dev/null; }
trap cleanup EXIT

step() { printf '\n--- %s\n' "$*"; }

# Poll until `condition` holds or the deadline passes. No bare sleeps: a fixed
# sleep asserts a latency nobody promised and is exactly how a flaky criterion
# gets written.
await() {
  local what=$1 deadline=$(( $(date +%s) + 20 ))
  shift
  while ! "$@"; do
    if [ "$(date +%s)" -ge "$deadline" ]; then
      echo "TIMED OUT waiting for $what" >&2
      return 1
    fi
    sleep 0.1
  done
}

has_json() { python3 -c "import json,sys;json.load(open('$1'))" 2>/dev/null; }
is_file() { [ -s "$1" ]; }

START=$(date +%s)

step "0. a relay, and an account registered with this machine's root public key"
"$RELAY" serve --listen "$LISTEN" --state-dir "$B/rs" --pairing-tcp "$MAILBOX" \
  > "$B/relay.log" 2>&1 &
RELAY_PID=$!
mkdir -p "$B/a/identity/devices"
export ARREO_IDENTITY_DIR="$B/a"
"$A" devices list >/dev/null 2>&1
ROOTPUB=$("$A" devices list --json 2>/dev/null \
  | python3 -c "import json,sys;print(json.load(sys.stdin)['root'])")
[ ${#ROOTPUB} -eq 64 ] || { echo "FAIL: no root public key (got ${#ROOTPUB} chars)"; exit 1; }
echo "account root (public): ${ROOTPUB:0:16}…"
"$RELAY" account add --state-dir "$B/rs" --account acct-exit --root-key "$ROOTPUB" >/dev/null
cat > "$B/a/arreo.toml" <<EOF
[relay]
enabled = true
addr = "$LISTEN"
account = "acct-exit"
name = "machine-a"
EOF

step "1. machine A admits ITSELF — step 1 of 2: print a code"
"$A" pair --mailbox "$MAILBOX" --config "$B/a/arreo.toml" --json > "$B/a-inv.json" 2>&1 &
PAIR_A=$!
await "machine A's invite" has_json "$B/a-inv.json" || { cat "$B/a-inv.json"; exit 1; }
CODE=$(python3 -c "import json;print(json.load(open('$B/a-inv.json'))['code'])")
URI=$(python3 -c "import json;print(json.load(open('$B/a-inv.json'))['uri'])")
echo "code: $CODE"

step "2. machine A admits itself — step 2 of 2: join with its own code"
"$A" pair --join "$CODE" --uri "$URI" --name machine-a 2>&1 | tail -2
wait "$PAIR_A" 2>/dev/null || true

step "3. machine A's daemon registers with the account's relay"
"$S" --socket "$B/a/a.sock" --config "$B/a/arreo.toml" > "$B/a.log" 2>&1 &
DAEMON_A=$!
await "A's directory row" grep -q "this machine is" "$B/a.log" || {
  echo "A never registered; its log:"; cat "$B/a.log"; exit 1; }
grep -o "directory: this machine is .*" "$B/a.log" | head -1

step "4. machine A prints a code for the SECOND machine"
"$A" pair --mailbox "$MAILBOX" --config "$B/a/arreo.toml" --json > "$B/b-inv.json" 2>&1 &
PAIR_B=$!
await "machine B's invite" has_json "$B/b-inv.json" || { cat "$B/b-inv.json"; exit 1; }
CODE2=$(python3 -c "import json;print(json.load(open('$B/b-inv.json'))['code'])")
URI2=$(python3 -c "import json;print(json.load(open('$B/b-inv.json'))['uri'])")
echo "code: $CODE2"

step "5. machine B joins — it has NO config and NO identity of its own"
mkdir -p "$B/b"
export ARREO_IDENTITY_DIR="$B/b"
"$A" machines add "$CODE2" --uri "$URI2" --name machine-b 2>&1 | tail -3
wait "$PAIR_B" 2>/dev/null || true

step "6. the account lists both machines"
export ARREO_IDENTITY_DIR="$B/a"
"$A" machines list --config "$B/a/arreo.toml" 2>&1 | tail -6

END=$(date +%s)
ELAPSED=$((END-START))
printf '\n=== TOTAL: %ss (budget 300) ===\n' "$ELAPSED"
[ "$ELAPSED" -lt 300 ] || exit 1
