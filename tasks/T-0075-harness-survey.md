---
id: T-0075
title: Harness survey — states, resume, custom models/providers for ~20 CLIs + centralization design
phase: 4
priority: 2
status: proposed
depends_on: [T-0017]
scope:
  - specs/harness-matrix.md
  - docs/harness-centralization.md
  - .loop/evidence/T-0075/**
verify:
  - cargo xtask adapters --check
---

## Goal

We support 2 harnesses deeply (pi, opencode — T-0017) and the rest via an honest
universal fallback. Before adapter-suite v2 (more TOMLs), T-0072 (resume), and the
config-sync task (§3.8), survey ~20 harness CLIs and answer the same four questions
for each: **how to read its state, how to resume its session, how it takes custom
models/providers, and which of its config is safe to centralize.**

Seed list (from ROADMAP §§3.8–3.9 + T-0017; extend with Orca's CLI list as discovery
source, aiming ≥ 15 with the rest honestly marked untried): Claude Code, Codex, Pi,
opencode, Gemini CLI, Grok, OMP, plus whatever else is installed or documented on
this box. Every claim names the CLI version it was checked against.

## Acceptance criteria

- [ ] `specs/harness-matrix.md`: one row per harness with version checked —
      state signals (native hooks/events with exact payload, OSC marks, prompt shapes,
      bell/exit-code behavior, detection latency observed), resume mechanism (exact
      resume argv + session-file location, verified live or marked unverified),
      custom models/providers (config paths, fields, env/secret handling).
- [ ] `docs/harness-centralization.md`: the sync design note — per-harness which
      files/fields are safe to sync (e.g. opencode.jsonc provider/model lists incl.
      custom provider verbs) vs machine-local (keys, absolute paths); secret-shape
      rules so keys never sync; conflict/merge pointer to ROADMAP §3.8. The owner's
      case (one opencode.jsonc custom provider edited once → all PCs) must be worked
      as the example end to end on paper.
- [ ] Evidence: executed transcripts under `.loop/evidence/T-0075/` (commands run
      verbatim, outputs quoted) — a row without a transcript is marked `untried`,
      never filled from memory. No behavior-changing adapter TOML edits in this task
      (those are adapter-suite v2); notes only.
- [ ] Follow-ups filed: adapter-suite v2 (new TOMLs, one task per batch) + config-sync
      implementation task with the matrix row-refs as its input.

## Verification

```console
cargo xtask adapters --check
```
