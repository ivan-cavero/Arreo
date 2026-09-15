---
id: T-0115
title: FFI: the act door for quick answers
phase: 3
priority: 2
status: proposed
depends_on: [T-0104, T-0094]
scope:
  - crates/arreo-core-ffi/**
  - docs/mobile.md
  - .loop/evidence/T-0115/**
verify:
  - cargo test -p arreo-core-ffi
  - cargo xtask ffi --check
---

## Goal

Phase 3's "quick answers": a phone answers a blocked agent from the notification. T-0094 built
the whole path — `Message::NotifyAct` with the bounded three actions, the daemon's single act
path, the pane's state as the authority on refusals, the audit row. T-0104's boundary has no
door onto it, so a phone can be told an agent is asking and cannot answer.

## Acceptance criteria

- [ ] The session handle gains the act call: `(pane, action, text?)` → the outcome, using
      `NotifyAction` and the 4096-byte bound **from `arreo_core::notify`**, never a second copy
      of either.
- [ ] The refusal sentences cross unchanged: "the pane has exited", "cannot reply: the pane is
      not asking (state=…)", "reply text is too long: N bytes, the bound is 4096" — the same
      strings the CLI prints, so a phone and a terminal cannot disagree about why an answer was
      refused. This is the typed-error rule applied to a *product* outcome: the act reply is a
      `Result` whose error is those sentences.
- [ ] A `skip` and a `kill` cross too — all three actions, since the daemon's gate treats them
      differently (skip writes no pane bytes, kill goes through the pane-kill path).
- [ ] The contract test drives all three actions plus one refusal through the boundary.
- [ ] `docs/mobile.md` gains the door and the action list.

## Notes

- No new wire message: T-0094's `NotifyAct`/`NotifyActReply` are what the boundary calls.
