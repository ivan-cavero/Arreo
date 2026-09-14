#!/bin/sh
# T-0082 evidence — the extension-family long tail: presence, installability, and
# whether a live turn can run. Verbatim. Run from the repo root.
set -u
S=target/test-scratch/T-0081
BIN=$S/install/node_modules/.bin
ISO=$S/iso-home
mkdir -p "$ISO"

echo "=== 1. is any long-tail harness already on this box? ==="
for b in grok qwen qoder droid cursor-agent copilot devin kimi antigravity mastracode kilo hermes; do
  p=$(command -v "$b" 2>/dev/null)
  printf "PATH  %-14s %s\n" "$b" "${p:-ABSENT}"
done

echo
echo "=== 2. does a package exist on npm? (candidate names, identity checked) ==="
for pkg in @xai/grok-cli grok-cli @qwen-code/qwen-code @qoder/qoder-cli @factory-ai/droid cursor-agent @github/copilot @cognition-ai/devin @moonshot-ai/kimi-code @google/antigravity @mastra/code @kilocode/cli; do
  v=$(timeout 25 npm view "$pkg" version 2>/dev/null | tail -1)
  printf "npm   %-28s %s\n" "$pkg" "${v:-NOT-FOUND}"
done

echo
echo "=== 3. identity of the names that resolved (a name is not a product) ==="
for pkg in grok-cli cursor-agent @github/copilot @qwen-code/qwen-code @moonshot-ai/kimi-code @kilocode/cli; do
  d=$(timeout 25 npm view "$pkg" description 2>/dev/null | tail -1 | cut -c1-80)
  r=$(timeout 25 npm view "$pkg" repository.url 2>/dev/null | tail -1 | cut -c1-60)
  printf "%-28s %s | %s\n" "$pkg" "$d" "$r"
done

echo
echo "=== 4. install the four that are the real product, into scratch ==="
npm install --no-audit --no-fund --prefix "$PWD/$S/install" @github/copilot @qwen-code/qwen-code @moonshot-ai/kimi-code @kilocode/cli 2>&1 | tail -3

echo
echo "=== 5. do they run, and what version? (isolated HOME) ==="
for n in copilot qwen kimi kilo; do
  printf '$ %s --version\n' "$n"
  HOME=$PWD/$ISO XDG_CONFIG_HOME=$PWD/$ISO timeout 90 "$PWD/$BIN/$n" --version </dev/null 2>&1 | head -2
done

echo
echo "=== 6. can a live turn run? (isolated HOME, no tty) ==="
printf '$ copilot -p "say hi"\n'
HOME=$PWD/$ISO XDG_CONFIG_HOME=$PWD/$ISO timeout 60 "$PWD/$BIN/copilot" -p "say hi" </dev/null 2>&1 | head -6
printf '$ qwen -p "say hi"\n'
HOME=$PWD/$ISO XDG_CONFIG_HOME=$PWD/$ISO timeout 60 "$PWD/$BIN/qwen" -p "say hi" </dev/null 2>&1 | head -6
printf '$ kimi -p "say hi"\n'
HOME=$PWD/$ISO XDG_CONFIG_HOME=$PWD/$ISO timeout 60 "$PWD/$BIN/kimi" -p "say hi" </dev/null 2>&1 | head -6
printf '$ kilo run "say hi"\n'
HOME=$PWD/$ISO XDG_CONFIG_HOME=$PWD/$ISO timeout 60 "$PWD/$BIN/kilo" run "say hi" </dev/null 2>&1 | head -6

echo
echo "=== 7. the fixture suite today (unchanged — no TOML written, no fixture synthesized) ==="
printf '$ cargo xtask adapters --check | tail -1\n'
cargo xtask adapters --check 2>&1 | tail -1
