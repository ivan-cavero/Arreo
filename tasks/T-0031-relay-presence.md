---
id: T-0031
title: Relay presence — online/offline/last-seen from one stated staleness rule
phase: 2
priority: 3
status: done
depends_on: [T-0029]
scope:
  - crates/arreo-relay/src/presence.rs
  - crates/arreo-relay/src/store.rs
  - crates/arreo-relay/tests/presence.rs
  - crates/arreo-relay/src/router.rs
  - .loop/evidence/T-0031/**
---

## Goal

ROADMAP §3.7 + §3.14: the relay knows which of the account's machines and devices are up and
shows "last seen X" instead of a scary error when one has been off for two weeks. Presence is
metadata only (names + online/offline + last-seen) and there is exactly one staleness rule in
the system — this module owns it and the directory consumes it.

## Acceptance criteria

- [x] One rule, in code and as the only place it lives: heartbeat every **30 s**; `online`
      while `last_seen` is ≤ **90 s** old (3 missed beats), `offline` beyond that, `stale` when
      `last_seen` > **30 days** (the tombstone signal the directory consumes).
      `presence::Presence::of(last_seen, now)` is the exported API — no consumer re-derives a
      window.
- [x] Boundaries are pinned with an injected clock (tests never sleep): 0 s / 89 s / 90 s / 91 s
      and 29 d / 30 d / 30 d + 1 s each assert the exact variant, plus a clock that steps
      backwards (a future `last_seen` clamps to `online`, never panics or reports a negative age).
- [x] Lifecycle truth: connect writes `online`; heartbeats refresh `last_seen` every 30 s
      (jittered, reconnect-safe); disconnect writes `last_seen` immediately, so `online` cannot
      outlive the socket by more than the 90 s window; a relay `kill -9` + restart recomputes
      presence from storage — nothing reads online until it reconnects (no phantom state).
- [x] The 15-day case reads honestly — `offline (last seen 15 d ago)`, never an error — and that
      formatting is what `arreo machines` (T-0044) and remote attach (T-0032) render, asserted
      for 0 s / hour / day / month shapes.
- [x] Read path is indexed and cheap: listing 10,000 devices is one query (`EXPLAIN QUERY PLAN`
      shows the index, no table scan) completing in < 50 ms on the dev box, asserted not assumed.
- [x] Presence holds metadata only: the row is `(device_id, account_id, presence, last_seen)`, a
      schema test fails if payload/keys/agent-state columns appear, and heartbeats write no audit
      row (audit is for actions, T-0033).
- [x] Evidence under `.loop/evidence/T-0031/`: the boundary table, restart transcript, 10k-device
      timing and the 15-day rendering.

## Landing notes (2026-09-11)

The thresholds were already the product contract (`arreo_core::mesh::directory`, T-0043: 90 s
window, 30-day stale) — this task did not re-derive them, it gave them a lifecycle. What is new:
`crates/arreo-relay/src/presence.rs` owns the relay-side rule (`presence_at`, the heartbeat
cadence, the `DevicePresence` row and its rendering), the router writes `last_seen_ms` on connect,
on every envelope, and on disconnect, and `Router::presence` applies the one rule at read time.

Three things the work taught, all in the code or the tests rather than smoothed over:

- **A heartbeat needs no new wire kind.** Every envelope refreshes `last_seen_ms`, and a session
  that only ever receives is kept alive by a zero-length frame to self that the relay records and
  consumes. A dedicated kind would be a second path for the same fact — and a second path is how a
  product starts lying about who is alive.
- **A restart test must compare against the relay's clock, not the test's.** The first draft of
  the lifecycle test restarted with the clock moved and compared stored rows against the test
  process's wall clock — asserting a window it did not exercise. The `now` is wall-plus-offset,
  stated in the test. Same class as T-0055's lesson, one layer up.
- **`now_ms()` caches its offset, so a test cannot reuse it across a seam change.**
  `OnceLock` means the first read in a process wins; the lifecycle test sets the env var *after*
  earlier reads and would have compared against 0. The test computes wall-plus-offset directly
  instead — and `clock_offset_ms` is now public so both sides can read the same variable.

## Notes

- Crates: `arreo-relay` only; its tables register through the single `store.rs` connection and
  its ordered migrations (no second connection — the boundary T-0043 also relies on).
- Why relay-observed presence instead of peer probing: the relay already sees connect and
  disconnect (§3.4, "presence: names + online/offline only"), so a heartbeat is one indexed write
  per device per 30 s, where a probe from a NAT'd phone cannot be answered at all. Rejected:
  socket liveness alone (a half-open QUIC connection after a phone sleeps would lie for hours);
  a 10 s window (3× the writes for no product value); per-consumer staleness logic (drift between
  the directory, `arreo machines` and the phone is how a product starts lying).
- 30 d `stale` deliberately matches §3.14's 30-day inbox default so one number explains both the
  retention window and the "this machine is gone" signal.
- Honest gaps: presence says nothing about *agent* state — a machine that is online with a
  crashed daemon still reads online until its socket drops; NAT rebinding can delay a disconnect
  observation by up to the 90 s window, which is why the window is stated instead of guessed.

## Verification

```console
cargo test -p arreo-relay --test presence
cargo xtask e2e --slice relay
```
