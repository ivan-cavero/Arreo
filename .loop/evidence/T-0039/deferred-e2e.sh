#!/bin/sh
# T-0039 — the deferred update, end to end, on the real binaries.
#
# Proves, in order: a cut that cannot happen defers instead of discarding; the
# pending state is reported and does not claim to be applied; `--apply-now`
# refuses while a pane is live and names it; the promotion happens at zero
# panes; the marker clears only once the installed binary reports the new
# version; and `.prev` holds what was replaced.
#
# Run from the repo root. Scratch lives under target/test-scratch (never /tmp).
set -u
ROOT=$PWD
E=$ROOT/target/test-scratch/T-0039/e2e
BIN=$E/bin
SOCK=$E/daemon.sock
export ARREO_STATE_DIR=$E/state

rm -rf "$E"
mkdir -p "$BIN" "$E/state"
cp "$ROOT/target/debug/arreo" "$BIN/arreo"

# The installed server and the candidate are stand-ins that answer `--version`:
# enough for the whole deferred path, and they keep the proof about the state
# machine rather than about a real daemon build.
cat > "$BIN/arreo-server" <<'EOF'
#!/bin/sh
case "$1" in
  --version) echo "arreo-server 0.2.0" ;;
esac
exit 0
EOF
cat > "$E/new-server" <<'EOF'
#!/bin/sh
case "$1" in
  --version) echo "arreo-server 0.3.0" ;;
esac
exit 0
EOF
chmod +x "$BIN/arreo-server" "$E/new-server"

# A real daemon serving the scratch socket: it is what makes `find_daemon_pid`
# find something to hand over to, and what owns the panes.
"$ROOT/target/debug/arreo-server" --socket "$SOCK" > "$E/daemon.log" 2>&1 &
DAEMON=$!
trap 'kill "$DAEMON" 2>/dev/null' EXIT
i=0
while [ ! -S "$SOCK" ] && [ "$i" -lt 100 ]; do sleep 0.1; i=$((i + 1)); done
echo "daemon: pid $DAEMON, socket $( [ -S "$SOCK" ] && echo present || echo MISSING )"
echo

echo "=== 1. a cut that cannot happen defers instead of discarding ==="
echo "\$ arreo update --server --from new-server --socket daemon.sock"
"$BIN/arreo" update --server --from "$E/new-server" --socket "$SOCK"
echo "exit=$?  (1: the cut did not happen — and the verified artifact was kept)"
echo "on disk: $(ls "$BIN" | tr '\n' ' ')"
echo "marker:  $(cat "$E/state/update-pending.json" | tr -d '\n' )"
echo

echo "=== 2. --status reports it, and does not claim it was applied ==="
echo "\$ arreo update --status"
"$BIN/arreo" update --status
echo "exit=$?"
echo

echo "=== 3. a live pane shuts the window: --apply-now refuses and names it ==="
echo "\$ arreo spawn agent /bin/sleep 300 --socket daemon.sock"
"$BIN/arreo" spawn agent /bin/sleep 300 --socket "$SOCK"
echo "\$ arreo update --apply-now --socket daemon.sock"
"$BIN/arreo" update --apply-now --socket "$SOCK"
echo "exit=$?  (3: the update is waiting, not broken)"
echo "stage still present: $( [ -f "$BIN/arreo-server.next" ] && echo yes || echo NO )"
echo

echo "=== 4. the agent finishes: the window opens and the promotion happens ==="
pkill -f "sleep 300" 2>/dev/null
sleep 1
echo "\$ arreo panes --socket daemon.sock"
"$BIN/arreo" panes --socket "$SOCK"
echo "\$ arreo update --apply-now --socket daemon.sock"
"$BIN/arreo" update --apply-now --socket "$SOCK"
echo "exit=$?"
echo "arreo-server now reports: $("$BIN/arreo-server" --version)"
echo "arreo-server.prev reports: $("$BIN/arreo-server.prev" --version)"
echo

echo "=== 5. the marker clears only on the version confirmation ==="
echo "\$ arreo update --status"
"$BIN/arreo" update --status
echo "exit=$?"
echo

echo "=== 6. nothing pending is not a failure ==="
echo "\$ arreo update --apply-now --socket daemon.sock"
"$BIN/arreo" update --apply-now --socket "$SOCK"
echo "exit=$?"
echo

echo "=== 7. the flags that describe a new install are refused, not ignored ==="
echo "\$ arreo update --status --from new-server"
"$BIN/arreo" update --status --from "$E/new-server"
echo "exit=$?  (2: usage)"
echo "\$ arreo update --apply-now --check"
"$BIN/arreo" update --apply-now --check
echo "exit=$?  (2: usage)"
