---
id: T-0054
title: Relay peer-disconnect signalling — a device learns its peer went away
phase: 2
priority: 3
status: proposed
depends_on: [T-0029, T-0050, T-0030]
scope:
  - crates/arreo-core/src/relay/mod.rs
  - crates/arreo-core/src/relay/client.rs
  - crates/arreo-core/src/relay/session.rs
  - crates/arreo-relay/src/router.rs
  - crates/arreo-relay/src/store.rs
  - crates/arreo-server/src/relay_client.rs
  - crates/arreo-tui/tests/remote.rs
  - specs/adr/**
  - .loop/evidence/T-0054/**
---

## Goal

A reconnect must be accepted, not swallowed. Today the relay routes by device id and keeps one
stream per peer, but it never tells a device that its peer *disconnected* — so after an abrupt drop
the far end still holds the dead stream, and the next handshake from that peer is delivered into it
and lost. The consequence is that a client which vanishes (a killed process, a lost network, a
sleeping laptop) cannot reconnect to the same peer until the far end happens to notice, which today
can take tens of seconds or not happen at all.

Found while building T-0032 (remote TUI attach): the remote path works, and a *reconnect* to the
same peer does not. The evidence is in `.loop/evidence/T-0034/` (the T-0032 turn) — a client whose
first session succeeded fails every reconnect attempt for over a minute while the relay reports
`deliver` for each one.

## Acceptance criteria

- [ ] The relay tells a device when a peer it has a live stream with goes offline: on `deregister`,
      every session holding a stream to that device is notified (the notification names the peer, so
      a device multiplexing several peers knows which one went away).
- [ ] The vocabulary is versioned in `arreo-core::relay` (`v1` extended, never renumbered): a new
      `RelayKind` or a new `Outcome` variant, decided in the ADR with the reason — one shape, not
      two ways to say the same thing.
- [ ] `RelaySession`'s reader turns the notice into the affected peer's stream ending with a typed
      reason (`the peer disconnected`), so the layer above sees a clean end-of-stream rather than a
      decryption failure or a silent stall.
- [ ] A daemon holding a stream to a peer that disconnected releases it, so the peer's next
      connection is accepted by a *fresh* `serve_peer` rather than delivered into a dead stream.
      Asserted: after the notice, `stream_to` returns a stream whose handshake succeeds.
- [ ] T-0032's drop criterion passes on top of this: killing the relay connection at three points
      (during snapshot, mid-delta, idle) and reattaching with `Resume{last_line}` yields a transcript
      byte-identical to a control run with no drop — no duplicated line, no gap. The test that
      proves it is `crates/arreo-tui/tests/remote.rs`, extended rather than replaced.
- [ ] The notice is metadata only (§3.7/§3.14): a device id, no pane content and no agent state,
      asserted by scanning the relay's state and log the way T-0032's parity test does.
- [ ] The inbox path is unaffected: a peer that is offline when a message is sent still queues
      (T-0030), and the sender still sees `queued`; only *live* streams end.
- [ ] Evidence `.loop/evidence/T-0054/`: the reconnect transcript before and after (the before is
      the failure this task fixes), the relay's notice in its log, and the byte-identical comparison.

## Notes

- Why it is its own task and not a line in T-0032: it is a wire-protocol addition plus a relay-side
  broadcast, with its own failure modes (a notice for a peer that already reconnected, a notice
  racing the reconnect itself, a device with no stream for that peer). T-0032 is a client; this is
  the carrier's contract, and the client-side half (retry with backoff) already ships there.
- The client-side mitigation is real but bounded and is *not* a substitute: retrying with the
  session's backoff (T-0032) recovers once the far end gives up the dead stream, and the far end
  gives up only when its own read or write fails. A protocol notice removes the wait; it cannot be
  removed by retrying harder.
- Honest gap after this lands: a notice is best-effort like every other relay message. A device that
  was itself offline during the disconnect learns nothing from it — it learns from the relay's
  refusal/queue outcome when it next writes, which is the existing path.
