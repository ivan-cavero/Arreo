---
id: T-0089
title: Adapter suite v2 — record Codex, [CC] and the long tail once a credential exists
phase: 4
priority: 4
status: proposed
depends_on: [T-0081, T-0082]
scope:
  - adapters/**
  - fixtures/**
  - .loop/evidence/T-0089/**
verify:
  - cargo xtask adapters --check
---

## GATE — read this first, it costs one command

**This task is gated on a credential existing on this box, not on code.** T-0081 and T-0082
proved the blocker is credentials, not availability: Codex, [CC], Copilot, Qwen Code, Kimi
Code and Kilo all install from npm and run here, and every one refuses a turn for want of a
login or an API key.

So the first step is a **re-probe, not a rediscovery**:

```console
# any of these means the gate is open
codex login status 2>&1 || true
printenv | grep -E '^(OPENAI|ANTHROPIC|GITHUB_TOKEN|GH_TOKEN|QWEN_|MOONSHOT_|KILO_)' || true
ls ~/.codex/auth.json ~/.claude/.credentials.json 2>/dev/null
```

If none of them is present: **close this task again** with a fresh one-command transcript under
`.loop/evidence/T-0089/` and stop. Do not re-run the discovery — the versions, the resume
argv and the exact refusal strings are already recorded (see Inputs). If one is present, work
the criteria below.

## Goal

T-0081 (Codex + [CC]) and T-0082 (the extension-family long tail) both closed with a scope
note and **N = 0** recorded adapters, because no credential exists here to drive a live turn.
Both remain real work: Codex and [CC] account for most of the fleet's real usage and still run
on `default.toml`'s universal shapes.

This is the retry, as one task rather than two, because the blocker is one thing.

## Inputs (already recorded — do not re-derive)

- `.loop/evidence/T-0081/scope-note.md` + `transcript.txt`: codex 0.154.0 (`codex resume
  [SESSION_ID] [PROMPT]`, `--last`, config `~/.codex/config.toml`, per-hook
  `hooks.state.*.trusted_hash`), [CC] 2.1.270 (`--resume <id>` / `--continue` /
  `--fork-session`, config `~/.claude/settings.json`), and the scratch install recipe.
- `.loop/evidence/T-0082/scope-note.md` + `transcript.txt`: Copilot 1.0.83, Qwen Code 0.23.3,
  Kimi Code 0.42.0, Kilo 7.6.2 — installable, runnable, unauthenticated; plus which npm names
  are *not* the product (`grok-cli`, `cursor-agent`).
- `specs/harness-matrix.md` (T-0075) rows, and `.loop/evidence/T-0075/herdr-integrations.txt`.

## Acceptance criteria

- [ ] Each harness recorded gets its adapter TOML (harness id, program match, native-tier event
      map, resume strategy, patterns) plus **≥4 live-recorded fixtures** (question / working /
      idle / stress), with the CLI version it was recorded from stated in the TOML header. No
      TOML without a recording; no fixture synthesized from documentation.
- [ ] Codex: the resume strategy is `resume <session-id>` (or `--last`), and
      `hooks.state.*.trusted_hash` is **never** proposed for sync — the digest is of a
      machine-local file, so syncing it would re-prompt hook trust on every machine.
- [ ] The mechanism-merge question T-0082 deferred is answered from recordings: if Qwen's
      `-session`-suffixed hook and the Kilo/Hermes non-shell plugin shapes turn out to share
      one "harness reports its own session identity" mechanism, that is recorded and (if it
      holds) proposed as one adapter shape rather than three.
- [ ] `cargo xtask adapters --check` green, with the fixture count stated in the evidence.

## Verification

```console
cargo xtask adapters --check
```
