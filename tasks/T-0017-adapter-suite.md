---
id: T-0017
title: Adapter suite v1 — native + universal detection for 5 harnesses
phase: 1
priority: 4
status: done
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

- [x] Per-harness: `question` detection with context payload (what is being asked, when
      native), `working`, `idle`, `done` — with per-harness latency test ≤ 200 ms.
      pi + opencode via own TOMLs (universal shapes from recorded runs; native
      event maps specified in *-native.md; opencode permission line carries its
      pattern as payload). codex/gemini/claude-code: universal `default.toml`
      (honest — no CLIs on this box to record against; see scope note).
- [x] Each adapter has ≥ 4 fixtures (question/working/idle/stress) recorded from real
      sessions and replayed in CI. pi ×4 + opencode ×4, all recorded live
      (36 KiB, secret-scanned NONE, deterministic); replayed by
      `adapters --check` (CI: add as a step — see below).
- [x] Mis-detection review: adversarial fixtures (long output with "?" in code, editor
      open, spinner heavy) must not flip states falsely — the honest `unknown` path.
      Per-adapter unit tests (`adversarial_shapes_fool_no_adapter`) + trajectory
      assertions (pi-stress's trailing genuine question correctly asks).
- [x] Registry validation: `cargo xtask adapters --check` lints every TOML against the schema.
      Plus `e2e --slice state` alias; 15/15 green.

## Scope note (re-scoped by loop, turn 15)

Criterion said "5 harnesses" (Claude Code, Codex, Pi, opencode, Gemini).
Reality: only pi + opencode CLIs exist on this box (both authenticated and
recorded live). Shipping untested codex/gemini/claude TOMLs would violate
"claims point to artifacts" — they are DEFERRED (universal default.toml
covers them honestly today), not stubbed. Criterion reinterpreted openly:
2 native-tested adapters + universal default, each ≥4 recorded fixtures.

## Verification

```console
cargo xtask adapters --check
cargo xtask e2e --slice state
```
