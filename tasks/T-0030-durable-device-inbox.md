---
id: T-0030
title: Durable per-device inbox — offline is normal, nothing queued is lost silently
phase: 2
priority: 2
status: proposed
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

- [ ] Durability: an envelope addressed to an offline device is committed before the sender is
      told `queued` (one transaction, WAL); `kill -9` on the real relay binary plus a restart
      still delivers it on the next drain.
- [ ] Retention is explicit: TTL default **30 days** (§3.14/§9, `--inbox-ttl-days`, 1..=365),
      swept hourly plus lazily on drain (no per-message timer); expiry is a counted, reported
      drop.
- [ ] Bounds with numbers, enforced before the write: **10,000 messages or 64 MiB per device**,
      whichever comes first (`--inbox-max-messages`, `--inbox-max-mb`); a breach evicts
      oldest-first, increments a durable `dropped_total` and logs the count — a full inbox never
      exceeds the bound and never fails the sender with an unbounded error.
- [ ] Exactly-once at the consumer: drain is cursor-ordered by monotonic `seq`; unacked rows are
      redelivered after a disconnect and the receiver's `(device, seq)` dedupe yields one
      delivery per message — the wire is at-least-once, ack + cursor advance in one transaction
      is what makes the consumer see each message once.
- [ ] Device disconnects → 3 messages published while it is offline → reconnect drains them in
      `seq` order exactly once (an injected duplicate frame produces no second delivery); repeat
      with a relay restart in between.
- [ ] Drops are never silent: the next drain reports `dropped: N`, stats expose `queued`,
      `dropped_total`, `expired_total` and `bytes` per device, and §5's `queued_loss = 0` row
      holds within the retention window in the slice.
- [ ] Rows are opaque: `(device, seq, bytes, received_at, expires_at)` plus the payload blob; a
      schema test fails if any column could hold keys, pairing codes or agent state, and the
      relay's logs name counts and ids only.
- [ ] Evidence under `.loop/evidence/T-0030/`: the offline→online transcript, eviction and
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
