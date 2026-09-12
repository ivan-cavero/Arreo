#!/usr/bin/env bash
# ROADMAP §6 Phase 2 exit criterion: "a stranger pairs a second machine in < 5 min
# without docs help." This follows only the steps the daemon's own message and the
# docs name, from empty directories.
set -u
B=/tmp/exit-crit; rm -rf $B; mkdir -p $B
A=/home/dev/dev/Arreo/target/debug/arreo
S=/home/dev/dev/Arreo/target/debug/arreo-server
RELAY=/home/dev/dev/Arreo/target/debug/arreo-relay
step(){ printf '\n--- %s\n' "$*"; }
START=$(date +%s)

step "relay (the operator's own)"
$RELAY serve --listen 127.0.0.1:19771 --state-dir $B/rs --pairing-tcp 127.0.0.1:19772 > $B/relay.log 2>&1 &
sleep 1

step "machine A generates its root key; the account is registered with it"
mkdir -p $B/a/identity/devices
export ARREO_IDENTITY_DIR=$B/a
# The account's root PUBLIC key, which is what `devices list` prints and what the
# relay verifies certificates against. `identity/root.key` holds the *secret* seed
# — registering that instead is the trap this script fell into first, and it fails
# later with "certificate signature does not verify under the root key".
ROOTPUB=$($A devices list --json 2>/dev/null | python3 -c "import json,sys;print(json.load(sys.stdin)['root'])")
echo "account root (public): ${ROOTPUB:0:16}..."
$RELAY account add --state-dir $B/rs --account acct-exit --root-key "$ROOTPUB"
cat > $B/a/arreo.toml <<EOF
[relay]
enabled = true
addr = "127.0.0.1:19771"
account = "acct-exit"
name = "machine-a"
EOF

step "machine A admits ITSELF (step 1: print a code)"
$A pair --mailbox 127.0.0.1:19772 --config $B/a/arreo.toml --json > $B/a-inv.json 2>&1 &
PAIR_A=$!
sleep 2
CODE=$(python3 -c "import json;print(json.load(open('$B/a-inv.json'))['code'])")
URI=$(python3 -c "import json;print(json.load(open('$B/a-inv.json'))['uri'])")
echo "code: $CODE"
step "machine A admits itself (step 2: join with its own code)"
$A pair --join "$CODE" --uri "$URI" --name machine-a 2>&1 | tail -2
# Only the admitting process: a bare `wait` would also wait on the relay, which
# never exits — that is how the first run of this script stalled at 400 s.
wait $PAIR_A 2>/dev/null || true

step "machine A starts its daemon and asserts its directory row"
$S --socket $B/a/a.sock --config $B/a/arreo.toml > $B/a.log 2>&1 &
sleep 3
grep -o "directory: this machine is .*" $B/a.log | head -1

step "machine A prints a code for the SECOND machine"
$A pair --mailbox 127.0.0.1:19772 --config $B/a/arreo.toml --json > $B/b-inv.json 2>&1 &
PAIR_B=$!
sleep 2
CODE2=$(python3 -c "import json;print(json.load(open('$B/b-inv.json'))['code'])")
URI2=$(python3 -c "import json;print(json.load(open('$B/b-inv.json'))['uri'])")
echo "code: $CODE2"

step "machine B joins — it has NO config and NO identity dir"
mkdir -p $B/b
export ARREO_IDENTITY_DIR=$B/b
$A machines add "$CODE2" --uri "$URI2" --name machine-b 2>&1 | tail -3

wait $PAIR_B 2>/dev/null || true

step "the account lists both machines"
export ARREO_IDENTITY_DIR=$B/a
$A machines list --config $B/a/arreo.toml 2>&1 | tail -5
END=$(date +%s)
printf '\n=== TOTAL: %s seconds (budget 300) ===\n' "$((END-START))"
