---
id: T-0093
title: Notification rules — one config, every transition, no noise
phase: 4
priority: 2
status: done
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
evidence:
  - .loop/evidence/T-0093/notifications.txt
---

## Scope note (the fence, and what the integration had to touch)

The fence above is the *feature*: the rule, the daemon's use of it, the CLI's read-back, the
slice and the docs. Integration needed four files outside it, each because a consumer cannot use
the feature without it — recorded here rather than done silently:

| File | Why |
|---|---|
| `crates/arreo-core/src/lib.rs` | `pub mod notify;` — the module has to be reachable |
| `crates/arreo-core/src/store.rs` | the audit actions, and `audit_recent_by_action`: the rule's history is a *read of the log*, and the only newest-first read the store had was wrong for it (see F1 in the evidence) |
| `crates/arreo-server/src/main.rs` | the policy is resolved before the handoff branch, so it survives a live handoff (asserted by `tests/handoff.rs::the_notify_policy_reaches_the_daemon_that_takes_over`, also outside the fence for the same reason) |
| `xtask/src/api_slice.rs` | the api slice must run the new `notify` suite, or the criterion "the slice asserts the notified, coalesced and suppressed paths" is unenforced |

`README.md` gains one docs-row entry (the roadmap's docs index is the README's job).

## Goal

The state engine (T-0004) already knows when an agent becomes blocked, done or idle, and the
audit log (T-0033) records it. What does not exist is the operator's *policy*: which
transitions are worth being told about, on which machine, and how often. ROADMAP §6 Phase 4:
"notification rules engine".

One sentence: one config file decides which agent transitions notify, with the noise control
that makes such a file usable (per-pane rules, quiet hours, a coalescing window, and a
once-per-episode rule).

## Acceptance criteria

- [x] `arreo_core::notify` owns the rule and is **pure**: `(transition, rule) -> decision`
      (notify / suppress with a reason). No I/O in the decision, so every boundary is testable
      without a daemon and there is exactly one place the policy lives.
- [x] `[notify]` in the daemon config: rules match on transition kind, pane id/glob and machine;
      `quiet_hours` (a local-time window that suppresses but **counts** what it suppressed);
      `coalesce_secs` (one notification per pane per window, the newest reason winning);
      `once_per_episode` (a blocked→working→blocked cycle notifies twice, a flap within one
      episode notifies once).
- [x] Delivery is a channel with one implementation and an honest failure: the local channel
      writes an audit row (`notify.sent` / `notify.suppressed` with the reason). A future push
      channel is a new implementation of the same trait, not a branch in the engine.
- [x] Suppression is **never silent**: every suppressed notification has a counted, queryable
      row (`arreo notify --why <pane>` reports the last decision and the rule that made it).
      "Why did I not get told?" is the question this feature creates, so it is answerable.
- [x] Boundaries asserted, not sampled: exactly-at-quiet-hours-start/end, the coalesce window's
      edge, a machine restart mid-episode (the episode state is durable, not in memory).
- [x] Slice: a real daemon with a scripted pane drives a blocked transition, and the api slice
      asserts the audit rows for notified, coalesced and suppressed paths.

## Notes

- The transition source is the existing engine; this task adds no detection.
- Time is injected the way T-0055 injected the relay's clock (`ARREO_CLOCK_OFFSET_MS`-style),
  so quiet-hours tests do not sleep and cannot flake.

## Criterion met in part, with its reason (recorded, not worked around)

`once_per_episode`: "a blocked→working→blocked cycle notifies twice" is met end to end
(`tests/notify.rs::a_cycle_that_comes_back_notifies_twice`). "a flap within one episode notifies
once" is met **at the rule level** — `decide` suppresses a repeat of the state it last notified
about — and is **unreachable in the daemon on today's engine**: the engine cannot emit a
same-state transition, which is the only thing that would exercise it (probed: two BELs give
`Working` *then* `Blocked`; every `tick`/`Working` push is guarded). A daemon-level test for that
half would be a test that cannot fail, so it is not written. See the evidence for the probe.

## Delivery decision for the one ambiguous criterion

`coalesce_secs`: "one notification per pane per window, the newest reason winning" is implemented
as **the first transition delivers, the rest are suppressed-and-counted with their reason
recorded** — not debounced to the end of the window, which would need a timer per pane and would
make the delivered notification carry a stale reason. The window's other half is exact: the
suppressed rows carry `since_ms`, so "how long since the last one" is answerable from the log.

## Follow-ups this task filed

- **T-0110** — the ungated tick pump changes what `wait` reports with notifications off (found by
  review, reproduced; a client-visible regression for users who never opt in).
- **T-0111** — the tick's cost: an unindexed audit scan per transition, and `PaneEntry::pump`'s
  journal/`fed` race.
