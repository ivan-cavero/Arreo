---
id: T-0117
title: Push payload and offline-queued delivery
phase: 3
priority: 2
status: proposed
depends_on: [T-0030, T-0093, T-0104]
scope:
  - crates/arreo-core/src/notify/**
  - crates/arreo-core/src/proto/message.rs
  - crates/arreo-relay/src/**
  - docs/notifications.md
  - .loop/evidence/T-0117/**
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

- [ ] A notification that is **delivered** (not suppressed) is also enqueued for every paired
      device that should receive it — the T-0093 policy decides the audience, so the rules engine
      stays the one place "who is told" lives.
- [ ] The payload is bounded and self-sufficient: pane id, machine, state, the same sentence
      T-0093's row carries, the action list (T-0094), and a timestamp. A push that needs a second
      round trip to be renderable is not a push.
- [ ] **Offline is a delay, not a loss**: a device that reconnects drains what it missed, in
      order, and T-0030's dedupe `(device, seq)` makes the redelivery safe. Asserted with a
      device that is absent across several transitions and then attaches.
- [ ] **Retention's drops are counted and visible** (T-0030's rule, T-0055's proof shape): a
      device absent past the window is told how many it missed rather than silently seeing a gap.
- [ ] Suppression stays quiet: a notification the policy withheld (quiet hours, coalesced,
      same-episode) is **not** pushed — T-0093's decision is the gate, and a push that ignored it
      would make the whole rules engine decorative.
- [ ] `docs/notifications.md` gains the push section: the payload, the audience rule, the
      offline behaviour and the retention interaction.

## Notes

- APNs/FCM are the *transport*, and they are a different machine's problem (a store account, a
  key). This task is the payload and the queue semantics, which are provable here against the
  real relay.
