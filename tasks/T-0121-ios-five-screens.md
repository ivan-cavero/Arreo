---
id: T-0121
title: The five screens — iOS
phase: 3
priority: 3
status: needs-human
depends_on: [T-0119, T-0114, T-0115, T-0116, T-0117, T-0118]
scope:
  - apps/ios/**
  - .loop/evidence/T-0121/**
verify:
  - (xcodebuild + a simulator run on a macOS runner)
---

## Goal

Phase 3's UI work, iOS side: pairing QR, overview, agent detail, quick answers, themes — the
five screens, over the shared core. ROADMAP §3.5 is explicit that these are *UI-only* work:
"any protocol/crypto change lands once, in Rust".

## Acceptance criteria

- [ ] All five screens exist and each one's data comes from the FFI surface (T-0114 metrics,
      T-0115 acts, T-0116 themes, T-0117 pushes, T-0118 the QR payload) — **no screen reaches
      around the boundary**, and none re-implements a rule the core owns (a state's word, a
      refusal's sentence, a colour's fallback).
- [ ] Quick answers work end to end: a push arrives, the notification is answered from the
      screen, and the agent's state leaves `Question` — the T-0094 property, on a phone.
- [ ] Themes render from the server's document (T-0116), not from a bundled asset, and the depth
      fallback is visible on a 16-colour rendering.
- [ ] Screenshots of every screen and every claimed state under `.loop/evidence/T-0121/`, per
      PROMPT §6's rule that a mobile claim is proven in the simulator.
- [ ] Offline behaviour: the app opened with no network shows what it last knew and says so,
      rather than an empty list that reads as "no agents".

## Notes

- The Rust half of every screen's data is a separate task and lands first; if a screen needs a
  read the boundary does not have, that is a finding for a new task, not a reason to link the
  core directly.
