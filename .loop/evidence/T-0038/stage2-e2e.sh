#!/bin/bash
# T-0038 stage 2, operated by hand: eight panes with output in flight across a real
# handoff. The criterion is contiguity — no tick lost, duplicated or reordered — and
# that every pane keeps the pid it had, which is what "the agent was not restarted"
# means observably.
set -u
T=/home/dev/dev/Arreo/target/debug
D=$(mktemp -d /tmp/arreo-s2-XXXXXX); S=$D/a.sock
export ARREO_STATE_DIR=$D/state; mkdir -p "$ARREO_STATE_DIR"
cp "$T/arreo" "$D/arreo"; cp "$T/arreo-server" "$D/arreo-server"
cp "$T/arreo-server" "$D/arreo-server-new"; printf '\n' >> "$D/arreo-server-new"
"$D/arreo-server" --socket "$S" >"$D/srv.log" 2>&1 & SRV=$!
trap 'kill $SRV 2>/dev/null' EXIT
for _ in $(seq 1 60); do [ -S "$S" ] && break; sleep 0.1; done
OLD=$(pgrep -f "arreo-server --socket $S" | head -1)
echo "old daemon pid: $OLD"

for i in 1 2 3 4 5 6 7 8; do
  "$D/arreo" spawn "p$i" /bin/sh -c \
    'echo pid=$$; n=0; while true; do n=$((n+1)); echo tick-$n pid=$$; sleep 0.05; done' \
    --socket "$S" >/dev/null 2>&1
done
sleep 2

echo "--- before the cut: last tick per pane ---"
for i in 1 2 3 4 5 6 7 8; do
  printf "p%s: %s\n" "$i" "$("$D/arreo" read "p$i" --socket "$S" 2>/dev/null | grep '^tick-' | tail -1)"
done

echo "--- the cut: arreo update --server, eight panes emitting ---"
"$D/arreo" update --server --from "$D/arreo-server-new" --socket "$S"; echo "exit=$?"
NEW=$(pgrep -f -- "--handoff-from $S" | head -1)
echo "new daemon pid: ${NEW:-none}; old still alive: $(kill -0 "$OLD" 2>/dev/null && echo YES || echo no)"

echo "--- after the cut: contiguity and pid stability per pane ---"
ok=1
for i in 1 2 3 4 5 6 7 8; do
  out=$("$D/arreo" read "p$i" --socket "$S" 2>/dev/null | grep '^tick-')
  first=$(echo "$out" | head -1 | sed 's/tick-\([0-9]*\).*/\1/')
  last=$(echo "$out" | tail -1 | sed 's/tick-\([0-9]*\).*/\1/')
  count=$(echo "$out" | wc -l)
  expected=$(( last - first + 1 ))
  uniq_pids=$(echo "$out" | sed 's/.*pid=//' | sort -u | wc -l)
  verdict="contiguous"
  [ "$count" -eq "$expected" ] || verdict="GAP(count=$count expected=$expected)"
  [ "$uniq_pids" -eq 1 ] || verdict="$verdict RESTARTED($uniq_pids pids)"
  [ "$verdict" = "contiguous" ] || ok=0
  echo "p$i: tick-$first..$last ($count lines) $verdict"
done
echo
echo "EVERY PANE CONTIGUOUS FROM 1 AND NEVER RESTARTED: $([ $ok = 1 ] && echo YES || echo NO)"
kill "$NEW" 2>/dev/null
