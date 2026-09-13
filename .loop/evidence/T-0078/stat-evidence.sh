#!/bin/sh
# Post-fix evidence run for T-0078 (mirrors the task's Verification stat line).
set -u
S="$1"
( umask 0002
  exec target/debug/arreo-server --socket "$S"
) >/dev/null 2>&1 &
DAEMON=$!
i=0
while [ ! -S "$S" ] && [ $i -lt 100 ]; do
  i=$((i+1))
  sleep 0.1
done
sleep 0.3
stat -c '%a %n' "$S" "$S.db" "$S.lock"
echo "-- (socket, db, lock are the daemon's own persistent files; wal/shm are"
echo "   recreated per store open and asserted by crates/arreo-server/tests/file_modes.rs)"
kill "$DAEMON" 2>/dev/null
wait "$DAEMON" 2>/dev/null
exit 0