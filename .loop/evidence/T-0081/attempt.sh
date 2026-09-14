#!/bin/sh
# T-0081 evidence — the attempt to obtain a live Codex / [CC] CLI, verbatim.
# Run from the repo root. Scratch install lives under target/test-scratch/T-0081.
set -u
S=target/test-scratch/T-0081
BIN=$S/install/node_modules/.bin
ISO=$S/iso-home
mkdir -p "$ISO"

echo "=== 1. are the CLIs already on this box? ==="
echo '$ which codex claude gemini copilot'
which codex claude gemini copilot 2>&1
echo "exit=$?"

echo
echo "=== 2. the installed agent CLIs on PATH ==="
echo '$ ls ~/.bun/bin ~/.local/bin'
ls ~/.bun/bin ~/.local/bin 2>&1

echo
echo "=== 3. obtainable from npm? ==="
echo '$ npm view @openai/codex version'
npm view @openai/codex version 2>&1
echo '$ npm view @anthropic-ai/claude-code version'
npm view @anthropic-ai/claude-code version 2>&1

echo
echo "=== 4. install both into scratch ==="
echo '$ npm install --prefix <scratch>/install @openai/codex @anthropic-ai/claude-code'
npm install --no-audit --no-fund --prefix "$PWD/$S/install" @openai/codex @anthropic-ai/claude-code 2>&1
echo "exit=$?"

echo
echo "=== 5. both binaries run ==="
echo '$ codex --version'
HOME=$PWD/$ISO XDG_CONFIG_HOME=$PWD/$ISO "$PWD/$BIN/codex" --version 2>&1
echo '$ claude --version'
HOME=$PWD/$ISO XDG_CONFIG_HOME=$PWD/$ISO "$PWD/$BIN/claude" --version 2>&1

echo
echo "=== 6. is there a credential? (codex asks its own doctor) ==="
echo '$ codex doctor        # isolated HOME'
HOME=$PWD/$ISO XDG_CONFIG_HOME=$PWD/$ISO timeout 120 "$PWD/$BIN/codex" doctor 2>&1 | head -12

echo
echo "=== 7. can a real turn run? codex ==="
echo '$ codex exec --skip-git-repo-check "say hi"        # isolated HOME'
HOME=$PWD/$ISO XDG_CONFIG_HOME=$PWD/$ISO timeout 90 "$PWD/$BIN/codex" exec --skip-git-repo-check "say hi" 2>&1 | tail -8

echo
echo "=== 8. can a real turn run? [CC] ==="
echo '$ claude -p "say hi"        # isolated HOME'
HOME=$PWD/$ISO XDG_CONFIG_HOME=$PWD/$ISO timeout 90 "$PWD/$BIN/claude" -p "say hi" 2>&1 | tail -4

echo
echo "=== 9. credential stores and env ==="
echo '$ ls -a ~/.codex ~/.claude'
ls -a ~/.codex ~/.cl* 2>&1
echo '$ printenv | cut -d= -f1 | grep -E "^(OPENAI|ANTHROPIC|GEMINI|GOOGLE|XAI|GROK|QWEN|DASHSCOPE|MOONSHOT|DEEPSEEK|OPENROUTER|AZURE)"'
printenv | cut -d= -f1 | grep -E "^(OPENAI|ANTHROPIC|GEMINI|GOOGLE|XAI|GROK|QWEN|DASHSCOPE|MOONSHOT|DEEPSEEK|OPENROUTER|AZURE)" 2>&1
echo "exit=$? (1 = none set)"

echo
echo "=== 10. resume argv, live from the binaries ==="
echo '$ codex resume --help        # first 4 lines'
HOME=$PWD/$ISO XDG_CONFIG_HOME=$PWD/$ISO "$PWD/$BIN/codex" resume --help 2>&1 | head -4
echo '$ claude --help | grep -E "resume|continue|fork-session"'
HOME=$PWD/$ISO XDG_CONFIG_HOME=$PWD/$ISO "$PWD/$BIN/claude" --help 2>&1 | grep -E "\-\-resume|\-\-continue|\-\-fork-session"

echo
echo "=== 11. hooks.state.*.trusted_hash — the real config.toml Orca writes ==="
echo '$ head -6 ~/.config/orca/codex-runtime-home/home/config.toml'
head -6 ~/.config/orca/codex-runtime-home/home/config.toml 2>&1
echo '$ grep -c trusted_hash ~/.config/orca/codex-runtime-home/home/config.toml'
grep -c trusted_hash ~/.config/orca/codex-runtime-home/home/config.toml 2>&1

echo
echo "=== 12. the fixture suite today (unchanged — no TOML written) ==="
echo '$ cargo xtask adapters --check | tail -2'
cargo xtask adapters --check 2>&1 | tail -2
