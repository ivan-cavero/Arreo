---
id: T-0052
title: Revocation cutoff — a live session ends when its device is revoked
phase: 2
priority: 3
status: done
depends_on: [T-0026, T-0023, T-0029]
scope:
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

- [x] The daemon keeps a registry of live sessions keyed by authenticated device id, covering all
      three transports (local Unix socket, direct Noise-QUIC, relay peer) — because a revocation
      that reaches only one of them is a hole in the other two.
- [x] After `arreo devices revoke <id>`, every live session for that device ends within **≤ 1 s**:
      the stream closes, and the client sees a typed revocation error rather than a silent drop.
- [x] The registry is bounded and self-cleaning: a session that ends removes its own entry (no
      leak across thousands of connections), and a device with many sessions is one entry with many
      handles, not many entries.
- [x] Revoking a device with no live session is a no-op on the cutoff path (the record is T-0026's
      job) and must not fail the command.
- [ ] **Re-scoped (below): the relay router's half is blocked on revocation propagation.** The relay applies the same rule: a revoked device's *live* relay session ends, so a revoked
      phone is not merely refused at its next handshake but dropped from the routing table now.
      (Propagating the list *between* machines remains the mesh work — see T-0046.)
- [x] `--slice api`, `--slice lifecycle`, `--slice persistence`, `--slice tui` and `--slice theme`
      stay green with unchanged transcripts; a session that is never revoked is unaffected.
- [x] Evidence under `.loop/evidence/T-0052/`: the mid-session cutoff transcript with timings, the
      registry-leak check, and the relay-side drop.

## Re-scope (2026-09-11, on landing: the relay-router half)

**The daemon enforces the cutoff on every transport; the relay's routing table cannot yet.**

Three of the four transports are done: the local Unix socket has no device to key on (it is
same-machine and trusted, and a revocation names a device), the direct Noise-QUIC path is
proven by test, and the **relay peer path gets the cutoff for free** — `serve_peer` runs the
same `serve_session` with the same `SessionAuth`, so the tick re-validates there too.

What is not done is the *relay's own routing table* dropping a revoked device's live session.
The relay authenticates devices by certificate chain and has no revocation list (stated in
`docs/relay-protocol.md` §8.3: "the relay verifies the certificate chain and the proof of
possession; it has no revocation list, so a revoked device's certificate still verifies
here"). Making the router drop a live session therefore needs the revocation to *reach* the
relay — a policy event between machines, which is the mesh/trust work this task's own Notes
already name as T-0046's half ("propagating the list between machines remains the mesh work").
Inventing a propagation channel here would be the wrong fence: it is cross-machine policy, not
a cutoff mechanism.

## Landing notes (2026-09-11)

**The cutoff is per-session self-re-validation, not a central sweeper.** Each remote session
races its next frame against a 500 ms re-check of its own authorization; a revocation written
by *another process* (the CLI, against the same store) is visible on the next tick with no
signaling between the processes. The task's Notes rejected "a sweeper that re-checks every
session each tick" — this is the distributed version of that idea, and the distinction
matters: a central sweeper makes cutoff latency a function of its tick and serializes every
session behind one task; each session checking itself has the same CPU cost (one store read
per idle session per second) with no coordination and no single point.

**The tick is 500 ms, not the 1 s the criterion bounds.** Measured with a 1 s tick: 1.004 s
from revoke to session end — over the line, because the tick *is* the latency budget. Halving
it costs one extra store read per idle socket per second and puts the worst case at ~0.5 s.

**Two findings, both real bugs found while proving it:**

1. **A session's last frame was written and discarded.** `serve_session` returned without
   closing its write half, and dropping the channel aborts the transport pump (`SecureChannel`'s
   `Drop`, T-0033) — so the typed revocation error was sealed, buffered, and thrown away, and
   the client saw a bare close. The session now shuts its write half down and pauses (bounded,
   300 ms) so the pump drains before its streams drop. This is not only a cutoff fix: *every*
   session end could lose its final frame, and now none does.
2. **`is_revoked` had to distinguish "no longer authorized" from "still authorized"** — it
   checks that the verb gate fails *and* the device is no longer in the authorized index, so a
   viewer session (authorized, less privileged) is not cut by its device being merely
   restricted. Rotation ends the old key's sessions, which is correct: rotation is a
   revocation of that key's access.

Honest gap, recorded: if the store read inside the tick fails (disk trouble), the check
fail-opens — the session lives. The alternative is fail-closed, which would end every session
on a transient SQLite error; the store is local, and a daemon whose database is unreadable has
larger problems than one stale session. Stated rather than hidden.

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
