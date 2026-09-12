set -u
MODE=${1:-unix}
B=/tmp/bisect-$MODE; rm -rf $B; mkdir -p $B
A=/home/dev/dev/Arreo/target/debug/arreo
RELAY=/home/dev/dev/Arreo/target/debug/arreo-relay
if [ "$MODE" = tcp ]; then
  $RELAY --pairing-tcp 127.0.0.1:21001 > $B/relay.log 2>&1 &
  MAIL="127.0.0.1:21001"
else
  $RELAY --pairing-socket $B/mb.sock > $B/relay.log 2>&1 &
  MAIL="$B/mb.sock"
fi
sleep 1
mkdir -p $B/a/identity/devices; export ARREO_IDENTITY_DIR=$B/a
$A devices list >/dev/null 2>&1
$A pair --mailbox "$MAIL" --json > $B/inv.json 2>$B/inv.err &
sleep 2
CODE=$(python3 -c "import json;print(json.load(open('$B/inv.json'))['code'])" 2>/dev/null)
URI=$(python3 -c "import json;print(json.load(open('$B/inv.json'))['uri'])" 2>/dev/null)
echo "$A pair --join \"$CODE\" --uri <uri>" >/dev/null
$A pair --join "$CODE" --uri "$URI" --name machine-a 2>&1 | tail -2
kill %1 2>/dev/null
echo "--- does the authority index it? (a fresh socket, so an empty store) ---"
$A devices list --socket $B/a/diag.sock --json 2>&1 | python3 -c "
import json,sys
t=sys.stdin.read()
try:
    d=json.loads(t); print('devices:', [(x['id'][:16], x['name']) for x in d['devices']])
except Exception: print('raw:', t[:200])
"
echo "--- certs on disk ---"; ls $B/a/identity/devices/ 2>/dev/null
