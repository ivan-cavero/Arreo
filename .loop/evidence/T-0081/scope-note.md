T-0081 — scope note: the blocker is credentials, not the binary
=================================================================

STATUS: done (scope-note branch of criterion 1). No adapter TOML written, no
fixture synthesized — criterion 2 holds.

THE EXACT BLOCKER
-----------------
Codex and Claude Code **can be obtained and run** on this box; neither can be
recorded, because there is **no credential** to drive a live turn:

- `@openai/codex` 0.154.0 and `@anthropic-ai/claude-code` 2.1.270 install from
  npm into scratch and both binaries execute:
      codex-cli 0.154.0          (codex --version)
      2.1.270 (Claude Code)      (claude --version)
- `codex doctor` (isolated HOME): `✗ auth  no Codex credentials were found -
  Run codex login or provide an API key through a supported auth env var.`
- `codex exec --skip-git-repo-check "say hi"` (isolated HOME): the CLI starts a
  session, connects to `wss://api.openai.com/v1/responses`, and is refused:
  `401 Unauthorized: Missing bearer or basic authentication in header`.
- `claude -p "say hi"` (isolated HOME): `Not logged in · Please run /login`.
- No provider API key is set in the environment (OPENAI_*/ANTHROPIC_*/GEMINI_*/
  XAI_*/QWEN_*/DASHSCOPE_*/MOONSHOT_*/DEEPSEEK_*/OPENROUTER_*/AZURE_* all
  absent), and `~/.codex` / `~/.claude` on this box are empty directories.

The four state fixtures the TOML branch demands (question / working / idle /
stress) each require a real agent turn against a live provider. Without any
credential that is impossible, so this task delivers its scope note and stops —
exactly the branch criterion 1 sanctions and T-0017 took for codex/gemini.

LIVE FACTS RECORDED ANYWAY (so the next attempt starts warm)
-----------------------------------------------------------
From the real binaries, isolated HOME, this box (transcript.txt):

- **codex 0.154.0** — resume is `codex resume [SESSION_ID] [PROMPT]`, with
  `--last` to continue the most recent session; session ids are UUIDs or
  session names. Config lives at `~/.codex/config.toml` (`-c key=value`
  overrides, `--profile` layers `$CODEX_HOME/<name>.config.toml`). Hooks are
  per-event with per-hook `trusted_hash` (see below); `--dangerously-bypass-
  hook-trust` exists for automation. `codex doctor` is a working auth/config
  health probe (succeeded at reporting the missing credential).
- **Claude Code 2.1.270** — resume is `--resume <session-id>` (continue that
  session), `--continue` (most recent), `--fork-session` (new id from a
  resumed one). Config is `~/.claude/settings.json`; hooks are configured
  there. `claude -p/--print` is the non-interactive mode.

CRITERION 4 — hooks.state.*.trusted_hash is NOT syncable (recorded, confirmed)
------------------------------------------------------------------------------
Confirmed in the real `config.toml` Orca writes for its codex runtime home
(`~/.config/orca/codex-runtime-home/home/config.toml`): each enabled hook is
recorded as

    [hooks.state."<path>/hooks.json:<event>:0:0"]
    enabled = true
    trusted_hash = "sha256:<digest>"

and `.orca-hook-trust-provenance.json` mirrors it. The `trusted_hash` digests
**the local hooks.json file** (it is a hash of a machine-local file, keyed by
an absolute path in the table name). Syncing `config.toml` or any file carrying
these entries would re-prompt hook trust on every machine — the hash would not
match that machine's file. So `hooks.state.*.trusted_hash` must never be
proposed for sync in `docs/harness-centralization.md`, and the sync engine's
config-class rules (T-0083/T-0086) should treat codex config as machine-local
until a sync-shaped subset (model/provider lists only) is defined.

WHAT WOULD UNBLOCK THIS TASK
----------------------------
Any one of: `codex login` / `claude /login` on this box, or an
OPENAI_API_KEY / ANTHROPIC_API_KEY in the environment. The moment one exists,
the same scratch install runs, and the four fixtures can be recorded against
it (the resume argv above is already documented). The follow-up is filed as
T-0089 with this note as its input.