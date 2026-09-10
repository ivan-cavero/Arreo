---
id: T-0009
title: Red-team pass on the spike — break the daemon on purpose
phase: 0
priority: 5
status: todo
depends_on: [T-0005]
scope:
  - crates/arreo-core/**
  - crates/arreo-server/**
  - xtask/src/chaos/**
---

## Goal

The adversarial pass the whole methodology demands (PROMPT.md §6): try to break what the
spike built — every finding becomes a task with repro steps, or gets fixed in-scope if small.

## Acceptance criteria

- [ ] Chaos suite (`xtask e2e --slice chaos`): kill the daemon mid-write (scrollback never
      lost), spam 10k rapid resizes, OOM-shaped giant output line, binary garbage into the
      VT parser, two writers racing on `pane send`, socket disconnect during attach.
- [ ] Every failure found: fixed in-scope or logged as a Phase 1 task with exact repro
      steps. No silent findings.
- [ ] Fuzz the VT parser + state engine inputs (cargo-fuzz or a deterministic corpus loop).

## Verification

```console
cargo xtask e2e --slice chaos
```

## Notes

The phase is not "prove the daemon" until somebody tried hard to make it fall over.
