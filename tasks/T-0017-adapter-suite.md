---
id: T-0017
title: Adapter suite v1 — native + universal detection for 5 harnesses
phase: 1
priority: 4
status: todo
depends_on: [T-0004, T-0011]
scope:
  - adapters/**
  - crates/arreo-core/src/state/**
  - fixtures/**
---

## Goal

Adapters for Claude Code, Codex, Pi, opencode, Gemini CLI — native tier where the harness
has hooks/events, universal tier otherwise. Coverage tests in CI replay recorded fixtures
for every adapter.

## Acceptance criteria

- [ ] Per-harness: `question` detection with context payload (what is being asked, when
      native), `working`, `idle`, `done` — with per-harness latency test ≤ 200 ms.
- [ ] Each adapter has ≥ 4 fixtures (question/working/idle/stress) recorded from real
      sessions and replayed in CI.
- [ ] Mis-detection review: adversarial fixtures (long output with "?" in code, editor
      open, spinner heavy) must not flip states falsely — the honest `unknown` path.
- [ ] Registry validation: `cargo xtask adapters --check` lints every TOML against the schema.

## Verification

```console
cargo xtask adapters --check
cargo xtask e2e --slice state
```
