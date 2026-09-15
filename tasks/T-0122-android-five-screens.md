---
id: T-0122
title: The five screens — Android
phase: 3
priority: 3
status: needs-human
depends_on: [T-0120, T-0114, T-0115, T-0116, T-0117, T-0118]
scope:
  - apps/android/**
  - .loop/evidence/T-0122/**
verify:
  - (gradle assembleDebug + an emulator run on a runner with the SDK)
---

## Goal

The Android mirror of T-0121, and the reason ROADMAP §3.5 budgets "+4 weeks versus a
single-platform start": the same five screens, a different toolkit, one shared core.

## Acceptance criteria

- [ ] All five screens, each reading through the FFI surface, with the same rule as T-0121: no
      screen reaches around the boundary and none re-implements a core rule.
- [ ] Quick answers work end to end from a push to a state change, on the emulator.
- [ ] Themes render from the server's document with the depth fallback visible.
- [ ] Screenshots of every screen and claimed state under `.loop/evidence/T-0122/`.
- [ ] **The two UIs agree**: the same pane, the same state word, the same refusal sentence, the
      same colours, side by side in one evidence file. That comparison is the whole point of a
      shared core, and it is the only check that catches a divergence introduced by a UI.

## Notes

- That last criterion is the phase's real integration test: duplicated UI is the accepted cost,
  and identical *behaviour* is the thing being bought.
