---
id: T-0117
title: Push payload and offline-queued delivery
phase: 3
priority: 2
status: done
depends_on: [T-0030, T-0093, T-0104]
scope:
  - crates/arreo-core/src/notify/**
  - crates/arreo-core/src/proto/message.rs
  - crates/arreo-core-ffi/src/codec.rs
  - crates/arreo-relay/src/**
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-server/src/relay_client.rs
  - crates/arreo-server/src/main.rs
  - docs/notifications.md
  - .loop/evidence/T-0117/**
evidence:
  - .loop/evidence/T-0117/push-payload.txt
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice relay
  - cargo xtask e2e --slice api
---

## Goal

ROADMAP §3.5: "push notifications (blocked/done, **including offline-queued delivery**)". T-0093
decides *what* notifies and records every decision; T-0030 built the relay's durable per-device
inbox with bounded retention and counted drops. What does not exist is the payload a push
carries and the rule that makes it survive a phone being offline.

## Acceptance criteria

- [x] A notification that is **delivered** (not suppressed) is also enqueued for every paired
      device that should receive it — the T-0093 policy decides the audience, so the rules engine
      stays the one place "who is told" lives.
- [x] The payload is bounded and self-sufficient: pane id, machine, state, the same sentence
      T-0093's row carries, the action list (T-0094), and a timestamp. A push that needs a second
      round trip to be renderable is not a push.
- [x] **Offline is a delay, not a loss**: a device that reconnects drains what it missed, in
      order, and T-0030's dedupe `(device, seq)` makes the redelivery safe. Asserted with a
      device that is absent across several transitions and then attaches.
- [x] **Retention's drops are counted and visible** (T-0030's rule, T-0055's proof shape): a
      device absent past the window is told how many it missed rather than silently seeing a gap.
- [x] Suppression stays quiet: a notification the policy withheld (quiet hours, coalesced,
      same-episode) is **not** pushed — T-0093's decision is the gate, and a push that ignored it
      would make the whole rules engine decorative.
- [x] `docs/notifications.md` gains the push section: the payload, the audience rule, the
      offline behaviour and the retention interaction.

## Notes

- APNs/FCM are the *transport*, and they are a different machine's problem (a store account, a
  key). This task is the payload and the queue semantics, which are provable here against the
  real relay.

## Scope note (re-scoped by the planner before dispatch, with the reason)

The fence above originally stopped at `arreo-relay`. Probing the code showed that cannot work,
so the fence was widened **before** any code was written rather than discovered mid-task:

- The audience rule is the daemon's: "every paired device that should receive it" is decided by
  the T-0093 policy, which lives in the daemon's notify tick (`daemon.rs`), and the paired-device
  list is the daemon's device authority. The relay has neither.
- The *queueing* is already the relay's and needs no new rule: `Inbox::enqueue` (T-0030) is what
  the relay's router calls when a device is offline, so a push that is **sent** to an absent
  device is queued by the code that already exists. The task is therefore "send the payload to
  the right devices", not "add a queue".
- The send needs a relay-session handle the notify tick does not have today: the tick holds
  `registry`, `db` and the policy; the session lives in `relay_client.rs`'s task, reached from
  `main.rs`'s composition root. Wiring that (a sender the tick can publish to) is the bulk of the
  real work, and it is `arreo-server` work.
- `crates/arreo-core-ffi/src/codec.rs` is in the fence because the FFI's `WireMessage` mirrors
  `Message` **variant for variant with an exhaustive match** (T-0116's lesson: adding a core
  variant is a compile error there, deliberately). If this task adds a message, it adds the mirror
  arm in the same commit — but it must **not** touch `crates/arreo-core-ffi/tests/**`, which
  T-0125 owns concurrently.

Nothing else changed: the goal, the criteria and the notes stand as written.

## Outcome

Done. A notification the T-0093 policy delivers is pushed to every paired device that should
receive it, sealed per device with one-way Noise (so an *absent* device can be pushed to), queued
by the relay's existing `Inbox` while the phone is away, and drained in order on reconnect with
T-0030's `(device, seq)` dedupe making redelivery safe. A suppressed notification is **not**
pushed, and that guarantee is a type rather than a check: `push_payload` answers `None` for
`Decision::Suppressed`, so the only thing a caller can do with a withheld decision is not push it.

Five live-process tests, and two mutations verified by the integrator: a suppressed notification
being pushed reddens both the unit test and the e2e one; an audience that excludes the absent
device reddens the offline test with `left: 0, right: 3`.

**The worker flagged one of its own tests as passing vacuously pre-fix** (with no push leg,
nothing was pushed, so a "suppressed is not pushed" test passed for the wrong reason). That was
honest and correct, and the resolution is that the test is live now: the mutation that pushes a
withheld decision fails it.

**A real defect in T-0030 found and fixed in scope**: `sweep_locked` attributes each device's
expiries to that device, but `enqueue` and `drain` also added the whole sweep's total to the
caller's device, so a device was told it had lost messages it never had (`dropped 4` for 2). Both
double-counts removed, with a test that reddens (`left: 4, right: 1`) when the drain-side one is
restored.

**One gap recorded, not fixed**: a daemon that takes over a live handoff has no relay session of
its own yet, so it cannot push; it now logs that instead of being silently row-only. Wiring a
relay session into the handoff path is outside this fence and is in the ledger's known-gaps line.

See `.loop/evidence/T-0117/push-payload.txt` for the wiring notes (why the drain lives in
`serve_loop`'s select, why the send uses `send_to_peer` and never `stream_to`), the three
mutations including the two bad ones the integrator had to correct, and the three recorded bends.
