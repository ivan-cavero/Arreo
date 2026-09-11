---
id: T-0052
title: Revocation cutoff — a live session ends when its device is revoked
phase: 2
priority: 3
status: proposed
depends_on: [T-0026, T-0023, T-0029]
scope:
  - crates/arreo-server/src/sessions.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-server/src/transport.rs
  - crates/arreo-server/src/relay_client.rs
  - crates/arreo-relay/src/router.rs
  - crates/arreo-server/tests/revocation_cutoff.rs
  - .loop/evidence/T-0052/**
---

## Goal

T-0026 makes revocation a durable decision and refuses the *next* connection. This closes the window
between: an attacker who already holds a session is cut off within a second of the revocation,
rather than continuing until they happen to reconnect. "Stolen phone, one command" is only true if
the command reaches the sessions the phone already has.

## Acceptance criteria

- [ ] The daemon keeps a registry of live sessions keyed by authenticated device id, covering all
      three transports (local Unix socket, direct Noise-QUIC, relay peer) — because a revocation
      that reaches only one of them is a hole in the other two.
- [ ] After `arreo devices revoke <id>`, every live session for that device ends within **≤ 1 s**:
      the stream closes, and the client sees a typed revocation error rather than a silent drop.
- [ ] The registry is bounded and self-cleaning: a session that ends removes its own entry (no
      leak across thousands of connections), and a device with many sessions is one entry with many
      handles, not many entries.
- [ ] Revoking a device with no live session is a no-op on the cutoff path (the record is T-0026's
      job) and must not fail the command.
- [ ] The relay applies the same rule: a revoked device's *live* relay session ends, so a revoked
      phone is not merely refused at its next handshake but dropped from the routing table now.
      (Propagating the list *between* machines remains the mesh work — see T-0046.)
- [ ] `--slice api`, `--slice lifecycle`, `--slice persistence`, `--slice tui` and `--slice theme`
      stay green with unchanged transcripts; a session that is never revoked is unaffected.
- [ ] Evidence under `.loop/evidence/T-0052/`: the mid-session cutoff transcript with timings, the
      registry-leak check, and the relay-side drop.

## Notes

- Depends on T-0026 (the record and the decision) and on T-0023/T-0029 (the transports whose
  sessions must be reachable).
- Why a registry instead of a sweeper that re-checks every session each tick: a retry loop makes the
  cutoff latency a function of the tick interval, and it burns CPU on every idle connection forever
  to answer a question that changes only when someone revokes. The revocation is an *event*; the
  registry turns it into one.
- Rejected: closing the daemon's listener (drops every session, not one); killing the device's
  panes (revocation is about *access*, not about destroying someone's work — and the machine owner
  may want the agent to keep running).
- Honest gap: the cutoff is at the next frame boundary rather than mid-frame, so the client may see
  one more delta before it is dropped; the ≤ 1 s bound covers it, and the client is told why.

## Verification

```console
cargo test -p arreo-server --test revocation_cutoff
cargo test -p arreo-relay --test router
cargo xtask e2e --slice api
```
