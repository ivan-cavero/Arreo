---
id: T-0004
title: State engine v0 — universal tier detection with deterministic fixtures
phase: 0
priority: 2
status: done
depends_on: [T-0003]
scope:
  - crates/arreo-core/src/state/**
  - fixtures/**
  - adapters/**
---

## Goal

`working / blocked / question / idle / done / unknown` from PTY output alone: output
silence + cursor parked on a prompt-shaped line + bell. Detection latency ≤ 200 ms.

## Acceptance criteria

- [ ] State transitions emit an event stream per pane (with timestamps + confidence).
- [ ] Fixture replays: record a real Claude Code session (question case: permission prompt;
      working case: tool output streaming; idle case) → `.pty` fixture → deterministic
      replay asserts the exact state timeline.
- [ ] `question (inferred)` honestly labeled as inferred, with the matched pattern.
- [ ] Adapter format (TOML) drafted: per-harness prompt regexes, silence thresholds.
- [ ] Latency budget test: ≤ 200 ms from last output byte to state event.

## Verification

```console
cargo test -p arreo-core state
cargo xtask e2e --slice state     # replays fixtures
```
