set -u
B=/tmp/inst; rm -rf $B; mkdir -p $B
A=/home/dev/dev/Arreo/target/debug/arreo
S=/home/dev/dev/Arreo/target/debug/arreo-server
RELAY=/home/dev/dev/Arreo/target/debug/arreo-relay
show(){ printf '%-28s root=%s\n' "$1" "$(cat $B/a/identity/root.key 2>/dev/null | head -c 16)"; }
$RELAY serve --listen 127.0.0.1:19971 --state-dir $B/rs --pairing-tcp 127.0.0.1:19972 > $B/relay.log 2>&1 &
sleep 1
mkdir -p $B/a/identity/devices; export ARREO_IDENTITY_DIR=$B/a
$A devices list >/dev/null 2>&1; show "after devices list"
R1=$(cat $B/a/identity/root.key)
$RELAY account add --state-dir $B/rs --account acct-inst --root-key "$R1" 2>&1 | tail -1
cat > $B/a/arreo.toml <<EOF
[relay]
enabled = true
addr = "127.0.0.1:19971"
account = "acct-inst"
name = "machine-a"
EOF
$A pair --mailbox 127.0.0.1:19972 --config $B/a/arreo.toml --json > $B/inv.json 2>&1 &
sleep 2
CODE=$(python3 -c "import json;print(json.load(open('$B/inv.json'))['code'])")
URI=$(python3 -c "import json;print(json.load(open('$B/inv.json'))['uri'])")
$A pair --join "$CODE" --uri "$URI" --name machine-a >/dev/null 2>&1; show "after self-join"
$S --socket $B/a/a.sock --config $B/a/arreo.toml > $B/a.log 2>&1 &
sleep 3; show "after daemon start"
echo "--- what the daemon says its root is ---"
grep -o "authority ready (root [0-9a-f]*" $B/a.log | head -1
grep -oE "registration failed.*" $B/a.log | head -1
