---
id: T-0019
title: Resource enforcement v0 — cgroups limits + kill switches on Linux
phase: 1
priority: 5
status: todo
depends_on: [T-0006]
scope:
  - crates/arreo-core/src/enforce/**
  - crates/arreo-server/**
---

## Goal

The enforcement half of resource truth: per-agent memory/pid budgets via cgroups v2
(systemd-run integration), throttling behavior, and the kill switch.

## Acceptance criteria

- [ ] Per-agent group with `memory.max` + `pids.max`; exceed → throttle event + notify
      (state event) before hard kill (configurable).
- [ ] Enforcement on/off tested in CI (cgroups v2 available on ubuntu runners with
      delegation; document the setup).
- [ ] Windows/macOS placeholders with the honest matrix (Job Objects/rlimit) stubbed.
- [ ] "Agent eats 4 GB" scenario test: budget enforced, you get told, harness itself stays
      within budget.

## Verification

```console
cargo xtask e2e --slice enforcement
```
