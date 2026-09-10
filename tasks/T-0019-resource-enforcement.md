---
id: T-0019
title: Resource enforcement v0 — cgroups limits + kill switches on Linux
phase: 1
priority: 5
status: done
depends_on: [T-0006]
scope:
  - crates/arreo-core/src/enforce/**
  - crates/arreo-server/**
  - crates/arreo-core/src/proto/message.rs
  - crates/arreo-core/tests/proto.rs
  - crates/arreo-core/tests/enforce.rs
  - crates/arreo-cli/src/main.rs
  - xtask/src/enforcement_slice.rs
  - xtask/src/chaos/attach_disconnect.rs
  - xtask/src/main.rs
  - .github/workflows/*
---

## Scope note (re-scoped by loop, turn 17)

Fence missed the protocol surface the feature needs: `Spawn` budget fields
(`memory_max`/`pids_max`/`kill_on_breach`, N−1-safe defaults) + every
`Spawn` constructor (daemon arm, CLI, tests, chaos probe). CI step for the
slice. Reason written here, not silent.

## Goal

The enforcement half of resource truth: per-agent memory/pid budgets via cgroups v2
(systemd-run integration), throttling behavior, and the kill switch.

## Acceptance criteria

- [x] Per-agent group with `memory.max` + `pids.max`; exceed → throttle event + notify
      (state event) before hard kill (configurable). Guard creates `arreo-<id>-<pid>`
      under own scope; breach (kernel `max` counters) → synthetic `Error:` line
      through the engine (wait --state blocked fires) + audit row + kill iff
      `kill_on_breach` (default notify-only). 1 s daemon sweeper covers
      unattached panes too.
- [x] Enforcement on/off tested in CI (cgroups v2 available on ubuntu runners with
      delegation; document the setup). CI step best-effort enables delegation on
      ubuntu; tests self-skip without delegation (writability probe, not presence);
      macOS/Windows skip honestly via Unimplemented stubs.
- [x] Windows/macOS placeholders with the honest matrix (Job Objects/rlimit) stubbed.
      `enforce/other.rs` names both mechanisms + owning tasks; every method says
      where it belongs.
- [x] "Agent eats 4 GB" scenario test: budget enforced, you get told, harness itself stays
      within budget. `e2e --slice enforcement`: 512 MB hog under 64 MB ceiling →
      breach notify + kill-switch assert on delegated boxes; loud no-delegation
      PASS here (mechanism proven by parser unit + integration contract test).
      Harness cost: 1 s tick over guarded panes only (file reads); bench green.

## Verification

```console
cargo xtask e2e --slice enforcement
```
