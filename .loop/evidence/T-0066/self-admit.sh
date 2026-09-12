set -u
B=/tmp/boot2; rm -rf $B; mkdir -p $B
A=/home/dev/dev/Arreo/target/debug/arreo
RELAY=/home/dev/dev/Arreo/target/debug/arreo-relay
$RELAY serve --listen 127.0.0.1:19671 --state-dir $B/rs --pairing-tcp 127.0.0.1:19672 > $B/relay.log 2>&1 &
sleep 1
mkdir -p $B/a/identity/devices
export ARREO_IDENTITY_DIR=$B/a
$A devices list >/dev/null 2>&1
$RELAY account add --state-dir $B/rs --account acct-self --root-key "$(cat $B/a/identity/root.key)"
cat > $B/a/arreo.toml <<EOF
[relay]
enabled = true
addr = "127.0.0.1:19671"
account = "acct-self"
name = "machine-a"
EOF
# A prints an invite, then A joins itself with it.
$A pair --mailbox 127.0.0.1:19672 --config $B/a/arreo.toml --json > $B/inv.json 2>&1 &
PAIRPID=$!
sleep 2
CODE=$(python3 -c "import json;print(json.load(open('$B/inv.json'))['code'])" 2>/dev/null)
URI=$(python3 -c "import json;print(json.load(open('$B/inv.json'))['uri'])" 2>/dev/null)
echo "code=[$CODE]"
echo "--- self-join ---"
$A pair --join "$CODE" --uri "$URI" --name machine-a 2>&1 | tail -4
wait $PAIRPID 2>/dev/null
echo "--- cert present now? ---"
ls $B/a/identity/devices/ 2>/dev/null | head -3
