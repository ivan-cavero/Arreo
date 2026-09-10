---
id: T-0041
title: cgroup pressure history + graded alerts that always precede a kill
phase: 2
priority: 3
status: proposed
depends_on: [T-0019, T-0040]
scope:
  - crates/arreo-core/src/enforce/**
  - crates/arreo-core/src/metrics/**
  - crates/arreo-core/src/store.rs
  - crates/arreo-core/tests/enforce_alerts.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-cli/src/main.rs
  - xtask/src/enforcement_slice.rs
  - .loop/evidence/T-0041/**
---

## Goal

T-0019 can throttle, notify on breach and kill. This task makes the story honest *before* the
kill: graded thresholds (80% warn, 95% critical, 100% breach) with cgroup pressure history,
so a 4 GB agent produces "you are about to lose this agent" rather than a silent
disappearance (§3.1 enforcement, P3 resource truth, §3.9's "you get told" seed).

## Acceptance criteria

- [ ] cgroup pressure series: `memory.current`, `memory.max`, `pids.current` and
      `memory.events` (`max`, `oom_kill`) of `arreo-<id>-<pid>` are sampled at the T-0040
      cadence into the same store, so one graph shows the ceiling, the total (children
      included) and OOM events next to the process-tree RSS line.
- [ ] Graded alerts with hysteresis: `warn` at 80% and `critical` at 95% of `memory.max`
      fire on the first tick that crosses each level; a level re-arms only after the reading
      falls below `warn − 10%`. One alert row per crossing, ordered per pane, no storm.
- [ ] Every alert is an audit row (`level`, pane, current, limit, top consumer pid) **and** a
      state event visible in TUI/CLI. With no client attached the alert is buffered and
      delivered on attach — dropping it is the failure mode this criterion forbids.
- [ ] Kill ordering invariant: when `kill_on_breach` is set, the episode's `critical` alert
      row always precedes the kill row in the audit log (same tick or earlier), and at most
      one kill occurs per breach episode even if the group stays over the limit.
- [ ] Latency: an alert reaches an attached client ≤ 2 s after the sampling tick that crossed
      the threshold (1 s cadence + 1 s delivery budget), asserted with timestamps in the slice.
- [ ] The T-0019 "agent eats 4 GB" scenario extended: on a delegated cgroup v2 box the 512 MB
      hog yields warn → critical → (kill iff configured), each with its row and its visible
      event, while the harness stays inside its own budget. Without delegation the slice stays
      loud about the skip (T-0019's rule) and the ordering assertions still run on the emit
      path, never silently pass.
- [ ] An attention listing (`arreo agents --attention` or the existing equivalent) surfaces
      alerting panes ahead of merely-working ones, so a script can page on it.

## Notes

- Alerts are scoped to cgroup pressure, not to a notification-rules engine: per-agent rules,
  digests, push routing and quick actions are §3.9/Phase 4. What this task guarantees is that
  the event exists, is ordered and is never lost — later surfaces deliver it.
- Ordering is enforced where rows are written (one transaction per tick: alerts, then any
  kill), because a log that must be *interpreted* to prove ordering is not a proof.
- Rejected: alerting from RSS alone. RSS of the pane's process tree misses grandchildren that
  moved trees and misses `oom_kill` entirely; the cgroup counters are the kernel's own answer.
- Honest gap: macOS stays advisory (rlimit + monitor + kill, per T-0019's matrix) — the
  thresholds work off the monitor path there, and the ordering claim is Linux-only until a
  Darwin delegate exists. Windows Job Objects get the same treatment when its paths land.
- Extends T-0019's `enforcement` slice in place (no new slice); evidence in
  `.loop/evidence/T-0041/`.

## Verification

```console
cargo xtask e2e --slice enforcement
cargo test -p arreo-core --test enforce_alerts
cargo xtask bench
```
