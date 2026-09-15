---
id: T-0115
title: FFI: the act door for quick answers
phase: 3
priority: 2
status: done
depends_on: [T-0104, T-0094]
scope:
  - crates/arreo-core-ffi/**
  - docs/mobile.md
  - .loop/evidence/T-0115/**
evidence:
  - .loop/evidence/T-0115/act-door.txt
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

- [x] The session handle gains the act call: `(pane, action, text?)` → the outcome, using
      `NotifyAction` and the 4096-byte bound **from `arreo_core::notify`**, never a second copy
      of either.
- [x] The refusal sentences cross unchanged: "the pane has exited", "cannot reply: the pane is
      not asking (state=…)", "reply text is too long: N bytes, the bound is 4096" — the same
      strings the CLI prints, so a phone and a terminal cannot disagree about why an answer was
      refused. This is the typed-error rule applied to a *product* outcome: the act reply is a
      `Result` whose error is those sentences.
- [x] A `skip` and a `kill` cross too — all three actions, since the daemon's gate treats them
      differently (skip writes no pane bytes, kill goes through the pane-kill path).
- [x] The contract test drives all three actions plus one refusal through the boundary.
- [x] `docs/mobile.md` gains the door and the action list.

## Notes

- No new wire message: T-0094's `NotifyAct`/`NotifyActReply` are what the boundary calls.

## Outcome

Done. `RelaySessionHandle::notify_act(peer, server_key, pane, action, text)` answers a
notification over T-0094's existing `Message::NotifyAct` — no new wire message — riding
T-0114's cached per-peer conversation. All three actions cross (`reply` through the same
audited send path, `skip` writing no pane bytes, `kill` through the pane-kill path), and
**every refusal is the machine's own sentence byte for byte**: "the pane has exited",
"cannot reply: the pane is not asking (state=…)", "reply text is too long: N bytes, the
bound is 4096", plus the trust gate's.

Two things it deliberately does **not** do, both so the bound and the rule have one
definition: no client-side reply-bound check (the daemon refuses it *and* writes the audit
row; a courtesy copy would duplicate the sentence and hide the refusal from the log), and
no client-side capability check (a viewer's act is sent, and the daemon refuses it, so the
phone shows a sentence the machine actually said).

**The worker crashed mid-flight (exit 1) after landing the code, the two tests and the
golden symbol but before running the tests, the mutations or the docs.** The tree compiled
and its suite was green, so the integrator finished the unit rather than re-dispatching:
four mutations (all RED — including a per-call handshake that reddens in 10.32 s, T-0114's
p1 signature, proving the conversation reuse is load-bearing for this verb too), the
`docs/mobile.md` section, and the battery.

T-0114's lesson was applied and stated in the brief: the fixture models a real daemon (one
conversation, N verbs), and the act test asserts **seven verbs on one handshake** — a
metrics read, two taken actions, three refusals and a `kill` — with the refusal sentences
built by the daemon's own rules (`notify::state_word`, `MAX_REPLY_BYTES`, `PANE_EXITED`)
rather than typed as literals.

The full report and the mutation list: `.loop/evidence/T-0115/act-door.txt`.
