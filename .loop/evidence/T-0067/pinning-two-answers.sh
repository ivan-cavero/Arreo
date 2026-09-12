set -u
PB=$(( 23000 + ($$ % 500) * 3 )); MB="127.0.0.1:$((PB+1))"
B=/tmp/indexcheck-$$; rm -rf $B; mkdir -p $B
A=/home/dev/dev/Arreo/target/debug/arreo
RELAY=/home/dev/dev/Arreo/target/debug/arreo-relay
$RELAY --pairing-tcp $MB > $B/r.log 2>&1 & RP=$!
trap 'kill $RP 2>/dev/null' EXIT
sleep 1
mkdir -p $B/a/identity/devices; export ARREO_IDENTITY_DIR=$B/a
$A devices list >/dev/null 2>&1
$A pair --mailbox $MB --json > $B/i.json 2>&1 &
until python3 -c "import json;json.load(open('$B/i.json'))" 2>/dev/null; do sleep 0.1; done
CODE=$(python3 -c "import json;print(json.load(open('$B/i.json'))['code'])")
URI=$(python3 -c "import json;print(json.load(open('$B/i.json'))['uri'])")
$A pair --join "$CODE" --uri "$URI" --name machine-a >/dev/null 2>&1
kill $RP 2>/dev/null
echo "--- devices list (human) ---"; $A devices list 2>&1 | head -5
echo "--- devices list --json ---"; $A devices list --json 2>&1 | head -c 400; echo
echo "--- certs ---"; ls $B/a/identity/devices/
