---
id: T-0094
title: Quick actions — a notification you can answer where you read it
phase: 4
priority: 2
status: proposed
depends_on: [T-0093, T-0061]
scope:
  - crates/arreo-core/src/notify/**
  - crates/arreo-core/src/proto/message.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-tui/src/**
  - xtask/src/api_slice.rs
  - docs/notifications.md
  - .loop/evidence/T-0094/**
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice api
---

## Goal

A blocked agent is a question, and today answering it means: read the notification, find the
pane, open it, type the answer. ROADMAP §6 Phase 4: "quick-action notifications". The product's
flagship signal (`Question`, T-0061) deserves to be actionable at the point it is read.

One sentence: a notification for a blocked pane carries the actions that answer it, and acting
on one sends exactly the bytes the operator chose — audited as the device, never as a keystroke
replay.

## Acceptance criteria

- [ ] A question notification carries a bounded action list: `reply <text>`, `skip`, `kill`.
      `reply` sends the text plus newline through the **existing** send path (T-0014), so it is
      audited, gated by the same per-verb trust rule, and subject to the same secret scan.
- [ ] The TUI answers in place: the notification panel's key for each action, and the pane's
      state visibly leaves `Question` after a reply. Keyboard-complete.
- [ ] The action is *not* a stored keystroke: the reply text is constructed as a message, never
      replayed into the pane's input buffer from a recording (T-0018's proven hazard).
- [ ] Refusals are the pane's, not the notifier's: an action on a pane that has since exited
      reports that, and a viewer-role device gets the trust refusal naming the role — the same
      sentence the CLI prints for a direct send.
- [ ] `arreo notify act <pane> <action> [--text ...]` is the CLI door onto the same path, so a
      script or a future phone uses one implementation.
- [ ] Slice: a real daemon, a real blocked pane, an action taken from the CLI and (in the tui
      slice) from the panel — with the pane's transcript showing the answer once.

## Notes

- Wire change: the action list rides the existing notification payload with serde defaults, so
  the N−1 window (T-0028) still holds — an older client renders the notification and ignores
  the actions, which the compat slice must assert.
