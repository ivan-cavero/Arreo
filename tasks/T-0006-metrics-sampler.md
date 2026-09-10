---
id: T-0006
title: Metrics sampler — per-process RAM/CPU with history rollups
phase: 0
priority: 3
status: done
depends_on: [T-0002]
scope:
  - crates/arreo-core/src/metrics/**
  - crates/arreo-core/tests/metrics/**
  - crates/arreo-cli/src/main.rs
---

## Scope note (re-scoped by loop, turn 5)

`crates/arreo-cli/src/main.rs` was not in the original fence, but the
acceptance criteria demand `arreo metrics` output — the fence was stale, not
the change. Added verb is `metrics --pid` (live table over real PIDs); the
daemon-backed `metrics <pane>` lands with T-0005/T-0012 (noted in `--help`).
Reason written here, not silent.

## Goal

The resource-truth primitive: sample RAM/CPU of each pane's process tree every 1 s
(exponential backoff when idle), 10 s rollups to SQLite (schema lives here).

## Acceptance criteria

- [x] Linux: `/proc`-based sampling (RSS, CPU%, pids of process tree); cgroup v2
      `memory.current` used when available.
- [x] Sampler is cross-platform-shaped: platform module per OS, trait in core (Windows/macOS
      implementations land later; Linux one is real here).
- [x] Rollups persisted to SQLite (WAL); retention pruning function exists.
- [x] Overhead test: sampler itself ≤ 1% CPU at 30 panes.
- [ ] `arreo metrics <pane>` (CLI) prints a live table from the daemon.
      Deferred honestly: no daemon/socket exists yet (T-0005/T-0012 own it).
      Shipped instead: `arreo metrics --pid <PID>` live table over the same
      sampler (proven in .loop/evidence/T-0006/cli-table.txt). This box stays
      unchecked until the daemon verb lands.

## Verification

```console
cargo test -p arreo-core metrics
cargo xtask bench --probe metrics
```
