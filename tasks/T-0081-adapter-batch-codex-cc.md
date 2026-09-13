---
id: T-0081
title: Adapter suite v2 batch B — Codex and [CC] native tier, gated on live recordings
phase: 4
priority: 3
status: proposed
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

## Acceptance criteria

- [ ] Either: `adapters/codex.toml` and `adapters/claude.toml` (harness id, program match,
      native-tier event map, resume strategy, patterns) each with **≥4 live-recorded fixtures**
      (question/working/idle/stress) and the CLI version they were recorded from stated in the
      TOML header — or a scope note naming the exact blocker (binary absent, no credentials, no
      license) plus the transcript proving the attempt.
- [ ] No third option: no TOML is edited without a recording behind it, and no fixture is
      synthesized from documentation.
- [ ] `cargo xtask adapters --check` green with the fixture count stated (the count is the
      evidence that the new fixtures actually run).
- [ ] If Codex's hooks land: `hooks.state.*.trusted_hash` is **never** proposed for sync in
      `docs/harness-centralization.md` (it hashes a local file, so syncing it would re-prompt
      hook trust on every machine) — note it in the task's evidence either way.

## Notes

- Inputs: `specs/harness-matrix.md` (T-0075) rows for Codex and [CC];
  `.loop/evidence/T-0075/herdr-integrations.txt` and `host-integration-artifacts.txt`.
- The survey deliberately marked these rows `untried`/host-artifact-only. Do not promote them
  to claims without a recording — that is this task's first job, not its formality.
