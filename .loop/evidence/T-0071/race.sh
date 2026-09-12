set -u
T=/home/dev/dev/Arreo/target/debug
bad=0; runs=0
for round in $(seq 1 12); do
  D=$(mktemp -d /tmp/arreo-race2-XXXXXX); S=$D/a.sock
  export ARREO_STATE_DIR=$D/state; mkdir -p $ARREO_STATE_DIR
  pids=""
  for i in $(seq 1 8); do $T/arreo-server --socket $S > $D/$i.log 2>&1 & pids="$pids $!"; done
  sleep 2
  n=0; for p in $pids; do kill -0 $p 2>/dev/null && n=$((n+1)); done
  runs=$((runs+1))
  if [ "$n" -gt 1 ]; then bad=$((bad+1)); echo "round $round: $n DAEMONS ALIVE (expected 1)"; fi
  for p in $pids; do kill $p 2>/dev/null; done; sleep 0.2
  for p in $pids; do kill -9 $p 2>/dev/null; done
  rm -rf $D
done
echo "rounds=$runs rounds-with-more-than-one-daemon=$bad"
