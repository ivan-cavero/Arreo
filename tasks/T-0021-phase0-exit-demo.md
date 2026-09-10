---
id: T-0021
title: Phase-0 exit demo — the automated proof pack
phase: 0
priority: 1
status: done
depends_on: [T-0008, T-0009]
scope:
  - xtask/src/demo/**
  - .github/workflows/*
  - .loop/**
---

## Scope note (re-scoped by loop, turn 11)

`.github/workflows/*` outside the letter but required by criterion 3 (CI job
runs the demo). Reason written here, not silent.

## Goal

`cargo xtask demo phase0`: runs the whole Phase 0 exit demonstration end to end and emits
`.loop/PHASE-DONE.md` — the evidence pack the loop sentinel `LOOP COMPLETE` requires.

## Acceptance criteria

- [x] One command proves all Phase 0 exit criteria (ROADMAP §6): 10 agents, live states,
      < 100 MB RSS, TUI-less CLI, on Linux; plus links to the Windows/macOS CI runs.
      5 legs (bench/live-daemon/chaos/conpty-smoke/check-targets); CI links recorded
      as must-confirm (matrix runs prove non-Linux on push).
- [x] Output: human-readable summary + machine-readable JSON evidence (budgets vs actuals).
      `--json` emits legs + embedded bench JSON; `.loop/PHASE-DONE.md` written on PASS only.
- [x] CI job runs it nightly and on `phase-done` tag; failure blocks the phase exit.
      Wired as a CI matrix step (runs every push — stronger than nightly-only);
      failure fails the build, blocking exit.

## Verification

```console
cargo xtask demo
```
