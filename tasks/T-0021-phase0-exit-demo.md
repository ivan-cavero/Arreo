---
id: T-0021
title: Phase-0 exit demo — the automated proof pack
phase: 0
priority: 1
status: todo
depends_on: [T-0008, T-0009]
scope:
  - xtask/src/demo/**
  - .loop/**
---

## Goal

`cargo xtask demo phase0`: runs the whole Phase 0 exit demonstration end to end and emits
`.loop/PHASE-DONE.md` — the evidence pack the loop sentinel `LOOP COMPLETE` requires.

## Acceptance criteria

- [ ] One command proves all Phase 0 exit criteria (ROADMAP §6): 10 agents, live states,
      < 100 MB RSS, TUI-less CLI, on Linux; plus links to the Windows/macOS CI runs.
- [ ] Output: human-readable summary + machine-readable JSON evidence (budgets vs actuals).
- [ ] CI job runs it nightly and on `phase-done` tag; failure blocks the phase exit.

## Verification

```console
cargo xtask demo
```
