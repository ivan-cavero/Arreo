---
id: T-0060
title: A CLI relay session displaces the daemon's — one live session per device is one too few
phase: 2
priority: 2
status: proposed
depends_on: [T-0029, T-0031]
scope:
  - crates/arreo-relay/src/router.rs
  - crates/arreo-relay/tests/router.rs
  - crates/arreo-core/src/relay/session.rs
  - crates/arreo-server/src/relay_client.rs
  - .loop/evidence/T-0060/**
---

## Goal

Found while building T-0045, and it is a real operator footgun rather than a test artifact:

> **Any CLI command that talks to the relay uses the machine's own device key, and the relay
> keeps one live session per device — so running it on a machine whose daemon is connected
> displaces that daemon from the routing table.** The daemon's session stays open, it never
> learns, and the machine stops being reachable by name until its own reconnect schedule
> (up to the backoff ceiling) brings it back.

The repro that found it: a test polled `arreo machines list` as machine A while A's daemon was
connected, then attached to A by name — the attach timed out, A's daemon logged nothing, and
the directory still listed A as `online`. Replace the poll with a log observation and the
attach works immediately. In the field the same sequence is `arreo machines list` (or
`arreo attach`, or `arreo machines trust`) run on the box that hosts the daemon — an ordinary
thing to do while diagnosing, and it takes the machine off the relay.

## Acceptance criteria

- [ ] A test reproduces it against a real relay: daemon A connected, a second session for A's
      device id opens and closes, and A is still reachable by name afterwards (today it is
      not, and that test is the definition of fixed).
- [ ] The relay's per-device routing survives a transient second session: after the second
      session ends, the *previous* live session is the route again, not nothing. Whatever
      mechanism (a stack per device, a re-registration on the daemon's next envelope, or
      refusing the second session while one is live and live) is chosen by the criteria below.
- [ ] The choice is recorded in the ADR for relay routing (0013) as a follow-up note, with the
      alternative rejected and why. The two live designs are: (a) a device's second session
      takes over and the *first* is told it was displaced (so the daemon reconnects promptly,
      which is the T-0029 rule already), or (b) sessions are reference-counted per device and
      the route is the newest *open* one — (a) is simpler and turns a silent steal into a
      visible event, but it must actually tell the displaced daemon, or the machine stays dark
      until its timer fires.
- [ ] A displaced session is not silent: it either receives a typed end (so the daemon's
      reconnect is immediate rather than waiting for the backoff, which today can be a
      minute) or the daemon re-asserts on the cadence it already has (T-0056's
      `assert_machine`, 30 s) and the re-assertion re-registers it. Pick one and say why in
      the ADR note; either way the recovery is bounded and tested.
- [ ] The CLI does not make it worse: a relay-touching verb run on a machine with a connected
      daemon either reuses the daemon's session (if it can) or says so. `arreo machines list`
      is the common case and must not be able to take a machine off the relay.
- [ ] Evidence `.loop/evidence/T-0060/`: the repro transcript (before), the fixed behaviour
      (after), and the recovery bound measured rather than asserted.

## Notes

- Why this is its own task: it is relay session bookkeeping, and T-0045 is a client-side verb.
  Bolting a relay fix onto it would also make the cross-machine attach that *did* land
  unverifiable in the same commit.
- Why it matters beyond the CLI: the same displacement happens for any second process on a
  machine that authenticates with its device identity — a second daemon instance, a script
  using `arreo` for `metrics`, or a future web client running on the machine. The rule the
  relay states ("a reconnecting device keeps its newer session", T-0029) is right for a
  *reconnect*; it is wrong for a *concurrent* session that is about to disappear.
- The daemon already has the ingredients to recover quickly: it holds `closed_handle()` for
  its session and re-asserts its row every 30 s (T-0056).

## Verification

```console
cargo test -p arreo-relay --test router
cargo test -p arreo-cli --test remote_machine
```
