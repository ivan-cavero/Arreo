---
id: T-0093
title: Notification rules — one config, every transition, no noise
phase: 4
priority: 2
status: proposed
depends_on: [T-0004, T-0033]
scope:
  - crates/arreo-core/src/notify/**
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-server/tests/notify.rs
  - docs/notifications.md
  - .loop/evidence/T-0093/**
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice api
---

## Goal

The state engine (T-0004) already knows when an agent becomes blocked, done or idle, and the
audit log (T-0033) records it. What does not exist is the operator's *policy*: which
transitions are worth being told about, on which machine, and how often. ROADMAP §6 Phase 4:
"notification rules engine".

One sentence: one config file decides which agent transitions notify, with the noise control
that makes such a file usable (per-pane rules, quiet hours, a coalescing window, and a
once-per-episode rule).

## Acceptance criteria

- [ ] `arreo_core::notify` owns the rule and is **pure**: `(transition, rule) -> decision`
      (notify / suppress with a reason). No I/O in the decision, so every boundary is testable
      without a daemon and there is exactly one place the policy lives.
- [ ] `[notify]` in the daemon config: rules match on transition kind, pane id/glob and machine;
      `quiet_hours` (a local-time window that suppresses but **counts** what it suppressed);
      `coalesce_secs` (one notification per pane per window, the newest reason winning);
      `once_per_episode` (a blocked→working→blocked cycle notifies twice, a flap within one
      episode notifies once).
- [ ] Delivery is a channel with one implementation and an honest failure: the local channel
      writes an audit row (`notify.sent` / `notify.suppressed` with the reason). A future push
      channel is a new implementation of the same trait, not a branch in the engine.
- [ ] Suppression is **never silent**: every suppressed notification has a counted, queryable
      row (`arreo notify --why <pane>` reports the last decision and the rule that made it).
      "Why did I not get told?" is the question this feature creates, so it is answerable.
- [ ] Boundaries asserted, not sampled: exactly-at-quiet-hours-start/end, the coalesce window's
      edge, a machine restart mid-episode (the episode state is durable, not in memory).
- [ ] Slice: a real daemon with a scripted pane drives a blocked transition, and the api slice
      asserts the audit rows for notified, coalesced and suppressed paths.

## Notes

- The transition source is the existing engine; this task adds no detection.
- Time is injected the way T-0055 injected the relay's clock (`ARREO_CLOCK_OFFSET_MS`-style),
  so quiet-hours tests do not sleep and cannot flake.
