#!/usr/bin/env bash
# T-0037 channel simulation — the *accept* path through a real product binary.
#
# The product binary only ever trusts the key compiled into it
# (supply-chain/arreo.pub, via include_str!). The release key's secret is a CI
# secret store and nowhere else, so no test can produce a signature the shipped
# binary will accept — the same call T-0036 made, and the same remedy: build a
# copy of the workspace with a **throwaway pinned key** and drive the verb for
# real. This script runs that copy against a file:// channel with a signed index
# and a signed artifact, proving `--check` reports the newest version and the
# anonymous `arreo update` fetches, verifies and installs it — plus the tamper
# and unsigned refusals, which carry the verifier's own sentences.
#
# Prereqs (documented, not scripted — rebuilding the workspace is the slow, rare,
# deliberate step this evidence exists to make unnecessary):
#
#     SIM=/tmp/arreo-channel-sim            # a copy of this checkout
#     cd "$SIM"
#     minisign -G -W -p keygen/sim.pub -s keygen/sim.key
#     cp keygen/sim.pub supply-chain/arreo.pub        # the throwaway pinned key
#     CARGO_TARGET_DIR="$SIM/target" cargo build -p arreo-cli
#
# Then this script needs `minisign` on PATH (the workspace ships no signer), and
# TARGET may point at a build directory on the root filesystem when /tmp is small
# (a 12 GB tmpfs on the dev box):
#
#     TARGET=/home/you/.cache/arreo-sim-target bash simulate.sh
set -euo pipefail

SIM="${SIM:-/tmp/arreo-channel-sim}"
TARGET="${TARGET:-$SIM/target}"
RUN="$SIM/run"
CHANNEL="$RUN/channel"
KEY="$SIM/keygen/sim.key"

rm -rf "$RUN"
mkdir -p "$CHANNEL" "$CHANNEL-tampered" "$CHANNEL-unsigned" "$CHANNEL-empty" \
    "$RUN/state" "$RUN/state-t" "$RUN/state-u" "$RUN/state-e"

cp "$TARGET/debug/arreo" "$RUN/arreo"          # the installed client
cp "$TARGET/debug/arreo" "$CHANNEL/arreo-sim"  # the release's artifact
printf '\n' >> "$CHANNEL/arreo-sim"            # distinct bytes, still runs (an ELF ignores trailing bytes)
chmod +x "$CHANNEL/arreo-sim"

cat > "$CHANNEL/arreo-index.json" <<'EOF'
{"version":"0.1.0","artifacts":{"x86_64-unknown-linux-gnu":"arreo-sim","aarch64-apple-darwin":"arreo-aarch64-apple-darwin"}}
EOF
# The release job's own signing commands, with the throwaway key.
printf '\n' | minisign -S -s "$KEY" -m "$CHANNEL/arreo-index.json"
printf '\n' | minisign -S -s "$KEY" -m "$CHANNEL/arreo-sim"

step() { printf '\n########## %s\n' "$*"; }
# Run the client and print its exit code, on success and on refusal alike.
client() {
    local state="$1"
    shift
    local code=0
    ARREO_STATE_DIR="$state" "$RUN/arreo" "$@" || code=$?
    echo "exit=$code"
}

step "1. --check against the signed channel"
client "$RUN/state" update --check --channel "file://$CHANNEL/"

step "2. --check --json"
client "$RUN/state" update --check --channel "file://$CHANNEL/" --json

step "3. --check with ARREO_CHANNEL_URL in the environment and no flag"
ARREO_STATE_DIR="$RUN/state" ARREO_CHANNEL_URL="file://$CHANNEL/" "$RUN/arreo" update --check
echo "exit=$?"

step "4. the anonymous update: fetch, verify, install"
client "$RUN/state" update --channel "file://$CHANNEL/" --no-reexec
cmp "$RUN/arreo" "$CHANNEL/arreo-sim" && echo "installed == channel artifact (byte-identical)"
test -f "$RUN/arreo.prev" && echo ".prev keeps the replaced binary: yes"
"$RUN/arreo" --version && echo "the installed binary runs"

step "5. --rollback puts the replaced binary back"
client "$RUN/state" update --rollback

step "6. a tampered index: one flipped byte, the valid signature carried over"
cp "$CHANNEL/arreo-index.json" "$CHANNEL-tampered/arreo-index.json"
python3 - "$CHANNEL-tampered/arreo-index.json" <<'PY'
import sys
data = bytearray(open(sys.argv[1], "rb").read())
data[5] ^= 0x01
open(sys.argv[1], "wb").write(data)
PY
cp "$CHANNEL/arreo-index.json.minisig" "$CHANNEL-tampered/arreo-index.json.minisig"
client "$RUN/state-t" update --check --channel "file://$CHANNEL-tampered/"

step "7. the anonymous update against the tampered channel is refused before installing"
client "$RUN/state-t" update --channel "file://$CHANNEL-tampered/"
"$RUN/arreo" --version && echo "the binary was not replaced"

step "8. an unsigned index is the verifier's MissingSignature, not an empty channel"
cp "$CHANNEL/arreo-index.json" "$CHANNEL-unsigned/"
client "$RUN/state-u" update --check --channel "file://$CHANNEL-unsigned/"

step "9. an empty channel is 'no releases yet', exit 0"
client "$RUN/state-e" update --check --channel "file://$CHANNEL-empty/"
client "$RUN/state-e" update --check --channel "file://$CHANNEL-empty/" --json