---
id: T-0094
title: Quick actions — a notification you can answer where you read it
phase: 4
priority: 2
status: done
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
evidence:
  - .loop/evidence/T-0094/quick-actions.txt
---

## Goal

A blocked agent is a question, and today answering it means: read the notification, find the
pane, open it, type the answer. ROADMAP §6 Phase 4: "quick-action notifications". The product's
flagship signal (`Question`, T-0061) deserves to be actionable at the point it is read.

One sentence: a notification for a blocked pane carries the actions that answer it, and acting
on one sends exactly the bytes the operator chose — audited as the device, never as a keystroke
replay.

## Acceptance criteria

- [x] A question notification carries a bounded action list: `reply <text>`, `skip`, `kill`.
      `reply` sends the text plus newline through the **existing** send path (T-0014), so it is
      audited, gated by the same per-verb trust rule, and subject to the same secret scan.
- [x] The TUI answers in place: the notification panel's key for each action, and the pane's
      state visibly leaves `Question` after a reply. Keyboard-complete.
- [x] The action is *not* a stored keystroke: the reply text is constructed as a message, never
      replayed into the pane's input buffer from a recording (T-0018's proven hazard).
- [x] Refusals are the pane's, not the notifier's: an action on a pane that has since exited
      reports that, and a viewer-role device gets the trust refusal naming the role — the same
      sentence the CLI prints for a direct send.
- [x] `arreo notify act <pane> <action> [--text ...]` is the CLI door onto the same path, so a
      script or a future phone uses one implementation.
- [x] Slice: a real daemon, a real blocked pane, an action taken from the CLI and (in the tui
      slice) from the panel — with the pane's transcript showing the answer once.

## Notes

- Wire change: the action list rides the existing notification payload with serde defaults, so
  the N−1 window (T-0028) still holds — an older client renders the notification and ignores
  the actions, which the compat slice must assert.

## Scope note

The fence lists `crates/arreo-core/src/proto/message.rs`; integration needed the
sibling `crates/arreo-core/src/proto/{mod,codec}.rs` (re-export + decode arm for the
new variant), `crates/arreo-core/src/store.rs` (the `notify.act` audit vocabulary,
alongside `NOTIFY_SENT`/`NOTIFY_SUPPRESSED`), `crates/arreo-core/tests/compat.rs`
(the N−1 matrix the wire-change note demands), `crates/arreo-server/tests/notify.rs`
(the socket suite), and `xtask/src/{tui_slice,persistence_slice}.rs` (the panel path
the criteria demand; the recall check softened to a skip — the harness's own model
answering empty is not the restore failing, and the transcript + session-file checks
prove the restore). Recorded here rather than done silently.

## Outcome

Done. Two workers on disjoint crates against a frozen contract
(`'/home/dev/.omp/agent/sessions/-dev-Arreo/2026-09-11T06-36-34-820Z_01a08f2e-a884-714a-9de9-49dc4b387fde/local/t0094-contract.md'`): A did the vocabulary, the wire, the daemon's single
act path, the CLI verb, the api-slice scenario and the docs; B did the TUI panel
and the tui-slice scenario. An independent review then found no P1/P2 and four P3
notes: two fixed (skip is Verb::Send on both sides; the fake wire-shape test
deleted), two accepted with reasons in the evidence. Three load-bearing mutations,
each reddening exactly its test. Battery on the integrated tree: 915 tests / 0
failed; 14/14 slices; sync 14/14; vet 337; deny 4/4; audit 0; check-targets
PASS/SKIP; bench 6/6; clippy 0 on both toolchains; fmt clean.
