---
id: T-0006
title: Metrics sampler — per-process RAM/CPU with history rollups
phase: 0
priority: 3
status: todo
depends_on: [T-0002]
scope:
  - crates/arreo-core/src/metrics/**
  - crates/arreo-core/tests/metrics/**
---

## Goal

The resource-truth primitive: sample RAM/CPU of each pane's process tree every 1 s
(exponential backoff when idle), 10 s rollups to SQLite (schema lives here).

## Acceptance criteria

- [ ] Linux: `/proc`-based sampling (RSS, CPU%, pids of process tree); cgroup v2
      `memory.current` used when available.
- [ ] Sampler is cross-platform-shaped: platform module per OS, trait in core (Windows/macOS
      implementations land later; Linux one is real here).
- [ ] Rollups persisted to SQLite (WAL); retention pruning function exists.
- [ ] Overhead test: sampler itself ≤ 1% CPU at 30 panes.
- [ ] `arreo metrics <pane>` (CLI) prints a live table from the daemon.

## Verification

```console
cargo test -p arreo-core metrics
cargo xtask bench --probe metrics
```
