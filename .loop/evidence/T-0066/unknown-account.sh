set -u
PB=$(( 26000 + ($$ % 400) * 3 )); LISTEN="127.0.0.1:$PB"; MB="127.0.0.1:$((PB+1))"
B=/tmp/unknown2-$$; rm -rf $B; mkdir -p $B
T=/home/dev/dev/Arreo/target/debug
$T/arreo-relay serve --listen "$LISTEN" --state-dir $B/rs --pairing-tcp "$MB" > $B/relay.log 2>&1 & RP=$!
trap 'kill $RP 2>/dev/null' EXIT
mkdir -p $B/id/identity/devices; export ARREO_IDENTITY_DIR=$B/id
$T/arreo devices list >/dev/null 2>&1
# Self-admit (pairing needs no account), so the machine has a certificate.
$T/arreo pair --mailbox "$MB" --json > $B/inv.json 2>&1 &
until python3 -c "import json;json.load(open('$B/inv.json'))" 2>/dev/null; do sleep 0.1; done
CODE=$(python3 -c "import json;print(json.load(open('$B/inv.json'))['code'])")
URI=$(python3 -c "import json;print(json.load(open('$B/inv.json'))['uri'])")
$T/arreo pair --join "$CODE" --uri "$URI" --name machine-a >/dev/null 2>&1
cat > $B/arreo.toml <<EOF
[relay]
enabled = true
addr = "$LISTEN"
account = "never-registered"
name = "machine-a"
EOF
$T/arreo-server --socket $B/id/a.sock --config $B/arreo.toml > $B/s.log 2>&1 & SP=$!
for _ in $(seq 1 80); do grep -q "registration failed" $B/s.log && break; sleep 0.1; done
echo "=== the machine's operator sees ==="; grep -m1 "registration failed" $B/s.log | fold -s -w 96
echo; echo "=== the relay's operator sees ==="; grep -m1 "unknown account" $B/relay.log | fold -s -w 96
kill $SP 2>/dev/null
