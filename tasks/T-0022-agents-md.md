---
id: T-0022
title: Write AGENTS.md — agent-loop operational reference for the repo
phase: 0
priority: 1
status: done
depends_on: [T-0001]
scope:
  - AGENTS.md
---

## Goal

ROADMAP §10.1 names `AGENTS.md` as a day-one agent-loop foundation; no task owned it.
Write the operational reference so any harness (or loop worker) can build, test, and
measure Arreo without asking a human.

## Acceptance criteria

- [ ] `AGENTS.md` documents: workspace layout + dependency direction, build/test/clippy/
      fmt commands, `cargo xtask` verbs, perf-budget gate, evidence convention
      (`.loop/evidence/<id>/`), cross-OS strategy summary, dependency philosophy.
- [ ] Every command in the file was executed verbatim during verification (no stale docs).
- [ ] A cold reader (different model, zero context) can go from clone to green `cargo test`
      following only this file.

## Verification

```console
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

## Notes

- Gardened by the loop (turn 1): ROADMAP assumed this file; the queue had no owner.
  Small doc unit — executor-hat, no delegation.
