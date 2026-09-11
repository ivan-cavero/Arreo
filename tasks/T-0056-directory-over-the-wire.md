---
id: T-0056
title: Directory over the wire — a machine registers itself and lists the account
phase: 2
priority: 2
status: done
depends_on: [T-0029, T-0043, T-0050]
scope:
  - crates/arreo-core/src/relay/mod.rs
  - crates/arreo-core/src/relay/client.rs
  - crates/arreo-core/src/relay/session.rs
  - crates/arreo-relay/src/router.rs
  - crates/arreo-server/src/relay_client.rs
  - crates/arreo-server/tests/relay_daemon.rs
  - docs/relay-protocol.md
  - .loop/evidence/T-0056/**
---

## Goal

T-0043 built the machine directory and its rules; the relay owns the only copy, and **nothing can
reach it over the wire**. That is what blocks T-0044 (`arreo machines add` cannot complete a join,
`list` has nothing to read), and through it T-0045, T-0046 and T-0047. This task is the missing
RPC: a machine asserts its directory row, and an account device reads the directory.

## Acceptance criteria

- [x] Two request kinds and one reply kind in `arreo-core::relay`, versioned v1 (additive — the
      N−1 rules in ADR 0017 apply): `join` (device→relay) carrying `JoinRequest { v, name,
      proto_version, machine_key, signature }`; `machines` (device→relay) carrying
      `MachinesRequest { v, all }`; `directory` (relay→device) carrying
      `DirectoryReply { v, seq, granted, machines, refused }`. The reply is discriminated by its
      **kind**, not guessed from the payload's shape — the drain report's guess-by-shape is a wart
      this does not repeat.
- [x] A machine proves it holds the key it claims: `join` carries the machine's public key and a
      signature (by that key) over a payload bound to the **session nonce** the relay issued at
      handshake time, so a recorded join cannot be replayed onto another session. A signature by
      the wrong key, a replayed payload, or an unparseable key is refused with a typed reason and
      **no row is written**.
- [x] `join` is idempotent and doubles as presence: the first call claims a name through a live
      `JoinTicket` minted by the relay; a later call from the same `machine_id` refreshes
      `last_seen_ms` without a ticket and without rewriting the name. A machine that comes back
      after a restart therefore re-asserts its row rather than failing.
- [x] `machines` answers with the account's rows (presence computed by the relay's own rule —
      `Directory::list`, never a second staleness formula), `all` including tombstoned names. Rows
      are the same `arreo_core::mesh::MachineRow` the directory export uses: one shape, on the wire
      and off it.
- [x] `RelaySession` gains a request/response path for these kinds (the existing `drain`/`ack` are
      fire-and-forget): a pending-reply map keyed by the request's sequence number, a bound on the
      wait, and a typed error on timeout or refusal. A reply for an unknown sequence is logged and
      dropped, not mis-delivered.
- [x] The daemon asserts its row on every relay (re)connect and refreshes it on the presence
      cadence, logging the granted name once (including the deterministic suffix when the name
      collided). A relay that refuses (unregistered account, bad proof) leaves the daemon serving
      locally and says so loudly.
- [x] `docs/relay-protocol.md` documents the three kinds, the join proof payload and the refusal
      reasons; the §4.2 kind table and §8.4 implementer checklist grow accordingly.
- [x] Evidence `.loop/evidence/T-0056/`: a two-machine transcript (both register, both list and see
      each other), the replay refusal, the name-collision suffix, and a rejoin after restart.

## Notes

- Why the machine's own key: `MachineId::from_key` is already the directory's identity rule (T-0043),
  and a machine's root key is the one thing that is stable across device re-pairing. Claiming a row
  for a key you do not hold must be impossible, which is what the signature buys.
- Who may register: a session authenticated with an account certificate, which means a device the
  account's owner already paired. v1 has no separate machine-admin role (T-0046's honest gap says the
  same about grants), so this is the narrowest door available today, and the ADR-level reasoning is
  recorded here rather than implied.
- Rejected: the CLI talking to the relay directly (it would need its own relay session and identity
  plumbing, duplicating the daemon's); a `join` that takes a role (privilege at join time is T-0046's
  refusal); presence as a separate heartbeat kind (it would be the same request with less
  information — a `join` already asserts the row).
- Honest gap: the reply carries no pagination, so an account with tens of thousands of machines
  answers in one envelope (bounded by the 1 MiB cap, which fails loudly rather than truncating).

## Landing notes (2026-09-11)

**Two kinds carry a request, one carries both answers.** `join` is both "admit me" and "I am still
here" because the relay can tell the difference by key: a machine the account already has gets a
presence refresh (no ticket, and crucially *no re-application of the name rule* — a machine that
reconnects keeps the suffix it was granted rather than racing for the name again), and a machine the
account does not have claims a name through a ticket the relay mints. One kind means one proof to
verify and one row to keep coherent.

**The proof is bound to the session nonce.** The relay keeps the nonce it issued past the handshake
for exactly this: a recorded join replayed into another session fails, which a test asserts by
sending a *correct* proof on the wrong session and watching it be refused. Without the binding, any
device in the account could replay another machine's join from the wire.

**The reply's kind identifies it.** The drain report's guess-by-shape (`decode DrainReport, else
Outcome`) is the pattern this deliberately does not copy: `directory` decodes as one type, and a
`refused` field distinguishes "you may not" from "there are none".

**What an account device may do.** Any session authenticated with an account certificate may assert
a machine row and read the directory. That is the narrowest door v1 has — the certificate means the
account's owner paired the device — and it is recorded here rather than implied. A future
machine-admin role would narrow it; nothing needs to widen it.

**Found while proving it:** the daemon-log assertion in `a_daemon_registers_itself_in_the_account_directory`
was flaky (1 in ~6 runs) because the test read stderr once, immediately after observing the row over
the wire — the row is written by the relay, the log line by the daemon, and the thread collecting
that stderr is a step behind. Now polled with a deadline, like the row. A test that fails 1 in 6 is a
test that will fail in CI at the worst moment.

**Honest gap:** no pagination (an account with tens of thousands of machines answers in one envelope,
bounded by the 1 MiB cap, which fails loudly rather than truncating), and the `machines` read is not
scoped down to "machines that concern you" — directory names and presence are account-level metadata
by design (§3.7), so every member sees the whole account.

## Verification

```console
cargo test -p arreo-core --lib relay
cargo test -p arreo-relay --test router
cargo test -p arreo-server --test relay_daemon
```
