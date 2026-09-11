---
id: T-0030
title: Durable per-device inbox — offline is normal, nothing queued is lost silently
phase: 2
priority: 2
status: done
depends_on: [T-0029]
scope:
  - crates/arreo-relay/src/inbox.rs
  - crates/arreo-relay/src/store.rs
  - crates/arreo-relay/tests/inbox.rs
  - crates/arreo-relay/src/main.rs
  - perf-budget.toml
  - .loop/evidence/T-0030/**
---

## Goal

ROADMAP §3.14, the requirement behind "power the server off for two weeks and everything
resumes": a per-device encrypted inbox at the relay with bounded size and age, so commands and
approvals sent to an offline machine are queued, delivered exactly once on reconnect, and —
when a bound forces a drop — counted instead of vanishing. The relay stores ciphertext it
cannot read (§4, P2).

## Acceptance criteria

- [x] Durability: an envelope addressed to an offline device is committed before the sender is
      told `queued` (one transaction, WAL); `kill -9` on the real relay binary plus a restart
      still delivers it on the next drain.
- [x] Retention is explicit: TTL default **30 days** (§3.14/§9, `--inbox-ttl-days`, 1..=365),
      swept hourly plus lazily on drain (no per-message timer); expiry is a counted, reported
      drop.
- [x] Bounds with numbers, enforced before the write: **10,000 messages or 64 MiB per device**,
      whichever comes first (`--inbox-max-messages`, `--inbox-max-mb`); a breach evicts
      oldest-first, increments a durable `dropped_total` and logs the count — a full inbox never
      exceeds the bound and never fails the sender with an unbounded error.
- [x] Exactly-once at the consumer: drain is cursor-ordered by monotonic `seq`; unacked rows are
      redelivered after a disconnect and the receiver's `(device, seq)` dedupe yields one
      delivery per message — the wire is at-least-once, ack + cursor advance in one transaction
      is what makes the consumer see each message once.
- [x] Device disconnects → 3 messages published while it is offline → reconnect drains them in
      `seq` order exactly once (an injected duplicate frame produces no second delivery); repeat
      with a relay restart in between.
- [x] Drops are never silent: the next drain reports `dropped: N`, stats expose `queued`,
      `dropped_total`, `expired_total` and `bytes` per device, and §5's `queued_loss = 0` row
      holds within the retention window in the slice.
- [x] Rows are opaque: `(device, seq, bytes, received_at, expires_at)` plus the payload blob; a
      schema test fails if any column could hold keys, pairing codes or agent state, and the
      relay's logs name counts and ids only.
- [x] Evidence under `.loop/evidence/T-0030/`: the offline→online transcript, eviction and
      expiry counters, the duplicate-delivery check, and the restart transcript.

## Notes

- Crates: `arreo-relay` only (AGPL) — rusqlite/WAL through the existing `store.rs` connection
  and its ordered migrations (T-0043 adds directory tables in a later migration; never a second
  connection). No new dependency: SQLite plus the T-0029 envelope type.
- Why a relay-side inbox instead of sender retry: the sender may be the phone that is about to
  sleep (§3.14) and mobile OSes kill background sockets, so the queue must outlive both
  endpoints. Rejected: unbounded retention (a disk-fill vector on a self-hosted VPS), in-memory
  queueing (fails the 15-day case by definition), machine-side queueing (the machine is off).
- TTL is wall-clock from `received_at`; §7's managed free tier (7 days) and Cloud (30 days) are
  pricing configuration of this same knob, not a fork — §3.14 keeps self-hosted at parity.
- Ack semantics mirror the daemon's `Read{from_line}` cursor habit (ADR 0007): the client owns
  the cursor, the relay owns the bytes until the ack. Honest gaps: no per-producer fairness (a
  noisy device fills only its own inbox), ordering is per-destination `seq` only, and push-wakeup
  for a sleeping phone is a later phase — this task guarantees the queue is there when it wakes.

## Verification

```console
cargo test -p arreo-relay --test inbox
cargo xtask e2e --slice relay
```

## Landing notes (2026-09-11)

**The stored blob is the whole framed envelope, not just the payload.** That is what keeps the
relay unable to read even the header of what it queues: on drain the bytes are replayed
(`Outbound::Raw`) rather than rebuilt, so no relay-side type ever decodes a queued envelope. It also
means the row needs no sender column — which is what keeps the schema at
`(device, seq, received_at, expires_at, bytes)` and the opacity test meaningful.

**Exactly-once is stated as at-least-once plus the consumer's half.** Rows are deleted only by
`ack`; an unacked drain redelivers. The tests assert both halves — that the relay *does* redeliver,
and that a `(device, seq)` dedupe collapses it to one delivery — rather than claiming end-to-end
exactly-once the relay cannot provide.

**Two defects found by the tests, both real:**

1. `stats()` read the cached `queued`/`bytes` columns from `inbox_stats`, but a sweep deletes rows
   without touching those columns — so the reported queue depth went stale the moment anything
   expired (the test caught it as "queued = 2 after everything expired"). `queued` and `bytes` are
   now counted from the rows, and only the lifetime drop counters come from the table. A stat that
   reports a depth the queue does not have is worse than no stat.
2. The test harness's stderr reader returned as soon as it found the bound address, which dropped
   the pipe and killed the relay on its next log line (EPIPE) — presenting as an authentication
   failure. The reader now keeps draining. Worth remembering for any future test that spawns the
   relay.

**`perf-budget.toml`:** the existing `queued_loss = 0` row is annotated with the mechanism that now
makes it observable (`dropped_total`/`expired_total` plus the per-drain report) rather than adding a
duplicate row. It stays `phase0 = false` on purpose: `cargo xtask bench` measures load and does not
check that row, so marking it `phase0 = true` would be a false claim about what runs today.

**Honest gaps, unchanged from the task's own notes:** ordering is per-destination `seq` only; no
per-producer fairness; no push-wakeup for a sleeping phone (the guarantee is that the queue is there
when it wakes); delivery to a *live* session is still reported on enqueue to that session's queue,
not on the peer reading it.

## Evidence (2026-09-11)

- `.loop/evidence/T-0030/inbox.txt` — 11 inbox acceptance tests (defaults and flag validation,
  schema opacity, oldest-first eviction by count and by bytes, oversized-message refusal, lazy and
  swept expiry with counted drops, unacked redelivery plus cursor dedupe, the CLI refusing
  impossible bounds, the migration leaving the directory intact, and the process-level
  `kill -9` → restart → drain case) and 14 router tests.
- `.loop/evidence/T-0030/gates.txt` — workspace suite, clippy, fmt, supply chain, cross-target,
  e2e slices and bench.
