set -u
B=/tmp/stranger2
rm -rf $B && mkdir -p $B
export ARREO_RELAY_BIN=/home/dev/dev/Arreo/target/debug/arreo-relay
A=/home/dev/dev/Arreo/target/debug/arreo
S=/home/dev/dev/Arreo/target/debug/arreo-server
RELAY=/home/dev/dev/Arreo/target/debug/arreo-relay
say(){ printf '\n=== %s ===\n' "$*"; }
START=$(date +%s)

say "1. relay + account (the operator's own relay)"
$RELAY serve --listen 127.0.0.1:19471 --state-dir $B/relay-state --pairing-tcp 127.0.0.1:19472 > $B/relay.log 2>&1 &
sleep 1
ROOT=$($A devices --help >/dev/null 2>&1; echo)
# the account root key comes from the machine that will own the account
mkdir -p $B/machine-a/identity/devices
export ARREO_IDENTITY_DIR=$B/machine-a
$A devices list >/dev/null 2>&1   # generates the machine's root key
ROOTHEX=$(python3 -c "
import pathlib,sys
p=pathlib.Path('$B/machine-a/identity/root.key')
print(p.read_text().strip() if p.exists() else '')
")
echo "machine A root key: ${ROOTHEX:0:16}..."
$RELAY account add --state-dir $B/relay-state --account acct-stranger --root-key "$ROOTHEX"
cat > $B/machine-a/arreo.toml <<EOF
[relay]
enabled = true
addr = "127.0.0.1:19471"
account = "acct-stranger"
name = "machine-a"
EOF
$S --socket $B/machine-a/a.sock --config $B/machine-a/arreo.toml > $B/a.log 2>&1 &
sleep 2
grep -c "directory: this machine is" $B/a.log

say "2. machine A prints a four-word code and an invite"
$A pair --mailbox 127.0.0.1:19472 --config $B/machine-a/arreo.toml --json > $B/pair.json 2>$B/pair.err
cat $B/pair.json
CODE=$(python3 -c "
import json;d=json.load(open('$B/pair.json'));print(d.get('code') or d.get('words') or '')" 2>/dev/null)
URI=$(python3 -c "
import json;d=json.load(open('$B/pair.json'));print(d.get('uri') or d.get('invite') or '')" 2>/dev/null)
echo "code=[$CODE] uri=[${URI:0:60}...]"

say "3. machine B joins with the code (no config on B at all)"
mkdir -p $B/machine-b
export ARREO_IDENTITY_DIR=$B/machine-b
$A machines add "$CODE" --uri "$URI" --name machine-b 2>&1 | tail -5

say "4. does the account now list two machines?"
export ARREO_IDENTITY_DIR=$B/machine-a
$A machines list --config $B/machine-a/arreo.toml 2>&1 | tail -8
END=$(date +%s)
say "TOTAL: $((END-START)) seconds"
