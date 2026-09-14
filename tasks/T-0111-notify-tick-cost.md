---
id: T-0111
title: The notify tick's cost — an unindexed audit scan per transition, and a pump that races itself
phase: 4
priority: 3
status: proposed
depends_on: [T-0093]
scope:
  - crates/arreo-core/src/store.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-core/src/pty.rs
  - xtask/src/api_slice.rs
  - .loop/evidence/T-0111/**
verify:
  - cargo test -p arreo-core --test store
  - cargo test -p arreo-server --test notify
---

## Why this exists

Two findings from the `reviewer` pass over T-0093 (`agent://NotifyReview`, findings 5 and 6).
Neither is a wrong answer today; both are costs that grow with the fleet, on a tick that runs
once a second for the life of the daemon.

**The scan.** `notify_history` runs
`SELECT … FROM audit WHERE action = 'notify.sent' ORDER BY ts_ms DESC, rowid DESC LIMIT 200`
once **per transition** — and there is no index on `audit.action` (the store indexes
`devices.public_key`, `metrics_series`, `machine_trust` and `sync_revisions`, not this). The
reviewer ran `EXPLAIN QUERY PLAN` against a real daemon store: `SCAN audit` +
`USE TEMP B-TREE FOR ORDER BY`. The log is append-only and pruned only by hand (T-0033's rule),
so the work grows with the whole trail for ever. On a 30-pane machine every pane that prints
anything also produces a `working` transition, so that is ≥ 30 unindexed scans per second, each
materialising up to 200 `StoredAudit` rows.

**The pump's concurrency.** `PaneEntry::pump` snapshots the journal *before* taking the `fed`
lock and writes the snapshot's length back unconditionally:

```rust
let (raw, _) = self.pane.raw_snapshot();
…
*fed = raw.len();
```

If pump A snapshots first and pump B (with a longer journal) takes `fed` first, A then writes the
smaller length: `fed` regresses and the next pump re-feeds bytes the engine already consumed —
duplicating visible text, re-arming `error_armed`, and potentially emitting a spurious `working`
transition that the tick would turn into a decision and a row. The reviewer labelled this a
hypothesis (the window needs a specific deschedule) and so does this file; the *shape* is plain
in the source. It predates T-0093 — two client sessions could always race a pane — but until now
an unattached pane was never pumped at all, and the tick now pumps every pane once a second.

## Acceptance criteria

- [ ] **The scan is bounded by something other than the log's length.** Either an index that
      serves the query (`CREATE INDEX … ON audit(action, ts_ms)` — a schema version bump and a
      migration, so the N−1 store rule applies) or a read that does not scan: e.g. keeping the
      last-notified fact where it is already held, or narrowing to the pane with an indexed
      predicate. State the chosen shape and why in the task's evidence; measure it —
      `EXPLAIN QUERY PLAN` before and after, and a timing on a store seeded with ≥ 100k audit
      rows (a throwaway script under `target/test-scratch/`, not a committed test).
- [ ] `notify_history` is called **once per pane per tick**, not once per transition, or its cost
      is otherwise bounded: a pane whose output produces three transitions in one second must not
      pay three identical scans. (The history cannot change between two transitions in the same
      batch — nothing is written in between — so a per-batch read is both cheaper and equally
      correct. Say so where the code does it.)
- [ ] The tick **measures its own work**, the way T-0040's writer does (its sleep starts after the
      sweep, so a slow sweep delays the next tick rather than stacking ticks) — or a comment says
      why not. A 1 s tick whose body grows with the fleet must not silently become a 3 s tick.
- [ ] **`pump` cannot regress `fed`**: the journal read and the `fed` update are one critical
      section (or the snapshot's length is only ever moved forward), so a re-feed of already-read
      bytes is impossible. A test that fails on today's code if one can be written honestly; if
      it cannot (the window is a deschedule), say so and pin the invariant by construction
      instead — `fed` never decreasing, asserted or argued in the code.
- [ ] The engine feed happens under the same lock discipline as the `fed` update, so two pumps
      cannot feed their slices out of journal order.

## Notes

- Do not weaken `notify_history`'s correctness to make it cheap: the newest-row semantics it has
  now (T-0093's F1 fix) are what the coalescing window and the episode rule depend on.
- The metrics writer's tick in the same function is the model for "measure your own work"; reuse
  its shape rather than inventing a second one.
