---
id: T-0095
title: Approval gates — a configured verb waits for a human
phase: 4
priority: 3
status: proposed
depends_on: [T-0059, T-0061, T-0093]
scope:
  - crates/arreo-core/src/approval.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-server/src/audit.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-server/tests/approval.rs
  - docs/approvals.md
  - .loop/evidence/T-0095/**
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice api
---

## Goal

ROADMAP §6 Phase 4 lists "approval gates". The dangerous verbs already exist (`kill`,
`machines remove`, `devices revoke`, `spawn` with a budget), and a remote device with the
operator role can run them. An operator who wants a second pair of eyes on those has nothing
today.

One sentence: a config decides which verbs require an approval, the request becomes a question
the operator answers, and the answer is recorded — with the *denied* case as loud as the
granted one.

## Acceptance criteria

- [ ] `[approvals]` names verbs (and optional scope: machine, pane pattern). A gated verb does
      not execute; it becomes a pending request with an id, an expiry, and the exact arguments
      it would run — shown to the approver, because approving a description rather than the
      arguments is how approvals get bypassed.
- [ ] One decision path: `arreo approve <id>` / `arreo deny <id> --reason`, and the TUI panel's
      keys. The decision is audited (`approval.requested` / `approval.granted` /
      `approval.denied`) with the device that decided, and the requesting device is told which
      of the three happened.
- [ ] Expiry is enforced, not advisory: a request older than its window is `expired` and the
      verb never runs — a request that outlives its window is a decision nobody made.
- [ ] The gate cannot be bypassed by re-issuing the verb with a different shape: matching is on
      the resolved verb and its arguments, and a gated verb that arrives by any other route
      (relay session, TUI, CLI) hits the same gate. Tested per route.
- [ ] A machine with no `[approvals]` behaves exactly as today: no requests, no latency, no new
      rows. The default must not make every verb a question.
- [ ] Slice: a real daemon with a gate on `kill` — request, deny (pane still alive), request,
      approve (pane dead), and the expired case with an injected clock.

## Notes

- The trust gate (T-0046) answers *may this device do it*; this answers *should this happen
  now*. They are different questions and both must pass — the code says so where it composes
  them.
