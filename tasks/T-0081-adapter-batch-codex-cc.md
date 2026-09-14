---
id: T-0081
title: Adapter suite v2 batch B — Codex and [CC] native tier, gated on live recordings
phase: 4
priority: 3
status: done
depends_on: [T-0075, T-0080]
scope:
  - adapters/**
  - fixtures/**
  - .loop/evidence/T-0081/**
verify:
  - cargo xtask adapters --check
---

## Goal

Two harnesses account for most of the fleet's real usage and neither has an adapter: Codex and
[CC] run on `default.toml`'s universal shapes. T-0075's survey reached the **hook entry point**
for both from artifacts installed on this box (Orca ships Codex's `hooks.json` with eight
events — `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`,
`SubagentStart`, `SubagentStop`, `Stop` — and herdr's registry names [CC]'s hook directory), but
**no `codex` or `claude` binary exists on this machine**, so nothing about payloads, exit codes,
question shapes or resume argv is known.

That is the whole shape of this task: it starts by obtaining a live CLI and recording it. If
that is impossible, it delivers the scope note and stops — exactly as T-0017 did for
codex/gemini. Shipping an unrecorded TOML would break the rule this repository is built on
(every claim points at an artifact), and a guessed adapter is worse than the honest universal
fallback it would replace.

## Outcome — the scope-note branch of criterion 1

**The blocker is credentials, not the binary.** Both CLIs install from npm into scratch and
both run on this box (`codex-cli 0.154.0`, `[CC] 2.1.270`); neither can drive a turn:

- `codex doctor` (isolated HOME): `✗ auth  no Codex credentials were found`.
- `codex exec "say hi"`: `401 Unauthorized: Missing bearer or basic authentication`.
- `claude -p "say hi"`: `Not logged in · Please run /login`.
- No provider API key in the environment; `~/.codex` and `~/.claude` are empty here.

The four state fixtures the TOML branch requires (question/working/idle/stress) each need a
real agent turn, so no TOML was written and no fixture was synthesized. Live facts recorded
anyway, so the retry starts warm: codex resume is `codex resume [SESSION_ID] [PROMPT]`
(`--last`), config `~/.codex/config.toml`; [CC] resume is `--resume <id>` / `--continue` /
`--fork-session`, config `~/.claude/settings.json`. Full transcript and note:
`.loop/evidence/T-0081/`.

## Acceptance criteria

- [x] Scope note naming the exact blocker (no credentials) plus the transcript proving the
      attempt — `.loop/evidence/T-0081/scope-note.md` + `transcript.txt` (which CLIs exist,
      what npm offers, install, `--version`, `doctor`, a real turn attempt, credential
      stores, env). The TOML branch is unreachable: a live turn is required and none can run.
- [x] No TOML edited without a recording behind it, and no fixture synthesized from
      documentation — `adapters/**` and `fixtures/**` are untouched in this task.
- [x] `cargo xtask adapters --check` green: **24 passed, 0 failed** — the fixture count is
      unchanged because the branch that adds fixtures was not taken, and the count is stated
      in the evidence.
- [x] `hooks.state.*.trusted_hash` recorded as **never syncable**: the real
      `config.toml` Orca writes carries one `trusted_hash = "sha256:…"` per hook, keyed by an
      absolute path, and the digest is of the *local* `hooks.json` — syncing it would
      re-prompt hook trust on every machine. Note in `.loop/evidence/T-0081/scope-note.md`.

## Follow-up

T-0089 — the retry, gated on any credential appearing (or a `codex login` / `claude /login`
on this box). It carries this note's live facts as its input.

## Notes

- Inputs: `specs/harness-matrix.md` (T-0075) rows for Codex and [CC];
  `.loop/evidence/T-0075/herdr-integrations.txt` and `host-integration-artifacts.txt`.
- The survey deliberately marked these rows `untried`/host-artifact-only. They stay that way:
  this task's job was to obtain a recording, and the recording is impossible without a
  credential — a fact now proven rather than assumed.
