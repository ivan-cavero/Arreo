set -u
PB=$(( 24000 + ($$ % 500) * 3 )); LISTEN="127.0.0.1:$PB"; MAILBOX="127.0.0.1:$((PB+1))"
B=/tmp/docverify-$$; rm -rf $B; mkdir -p $B
T=/home/dev/dev/Arreo/target/debug
A=$T/arreo; S=$T/arreo-server; RELAY=$T/arreo-relay
$RELAY serve --listen "$LISTEN" --state-dir $B/rs --pairing-tcp "$MAILBOX" > $B/relay.log 2>&1 & RP=$!
trap 'kill $RP 2>/dev/null' EXIT
mkdir -p $B/id/identity/devices; export ARREO_IDENTITY_DIR=$B/id
say(){ printf '\n$ %s\n' "$*"; }

say "arreo devices list        # step 1: makes the key, prints the public half"
$A devices list 2>&1 | head -2
ROOT=$($A devices list --json 2>/dev/null | python3 -c "import json,sys;print(json.load(sys.stdin)['root'])")
echo "   (json .root matches: $([ ${#ROOT} -eq 64 ] && echo yes || echo NO))"

say "arreo-relay account add --root-key <that>"
$RELAY account add --state-dir $B/rs --account my-account --root-key "$ROOT"
cat > $B/arreo.toml <<EOF
[relay]
enabled = true
addr = "$LISTEN"
account = "my-account"
name = "workbox"
EOF

say 'arreo pair --role owner        # step 3, with the owner role'
$A pair --role owner --mailbox "$MAILBOX" --config $B/arreo.toml --json > $B/inv.json 2>&1 &
until python3 -c "import json;json.load(open('$B/inv.json'))" 2>/dev/null; do sleep 0.1; done
CODE=$(python3 -c "import json;print(json.load(open('$B/inv.json'))['code'])")
URI=$(python3 -c "import json;print(json.load(open('$B/inv.json'))['uri'])")
say "arreo pair --join \"$CODE\" --uri ... --name workbox"
$A pair --join "$CODE" --uri "$URI" --name workbox 2>&1 | tail -2

say "arreo-server --socket ... --config ...    # step 4"
$S --socket $B/id/arreo.sock --config $B/arreo.toml > $B/s.log 2>&1 & DP=$!
for _ in $(seq 1 100); do grep -q "directory: this machine is" $B/s.log && break; sleep 0.1; done
grep -o "directory: this machine is .*" $B/s.log | head -1
grep -o "relay session up.*" $B/s.log | head -1
say "the role the account recorded"
$A devices list 2>&1 | grep workbox
kill $DP 2>/dev/null
