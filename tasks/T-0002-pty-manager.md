---
id: T-0002
title: PTY manager core — spawn, read, write, resize per pane with a ring buffer
phase: 0
priority: 1
status: done
depends_on: [T-0001]
scope:
  - crates/arreo-core/src/pty/**
  - crates/arreo-core/tests/pty/**
---

## Goal

Own real PTYs on all three OSes via `portable-pty`. One task per PTY (tokio), bounded
channels, no unbounded queues.

## Acceptance criteria

- [ ] Spawn a shell (or `/bin/echo` in tests) in a PTY, read output, write input, kill.
- [ ] Resize (cols/rows) propagates to the child.
- [ ] Output lands in a bounded hot ring buffer (512 lines) with an API to drain and to
      page older lines; memory per pane ≤ 3 MB asserted by a test.
- [ ] Child exit is observed (exit code available); zombie-free.
- [ ] Works on Linux; ConPTY path exercised by T-0007 (this task writes it portable-first).

## Verification

```console
cargo test -p arreo-core pty
cargo xtask e2e --slice pty    # stub until T-0008 wires the battery
```
