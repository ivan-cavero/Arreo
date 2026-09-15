---
id: T-0119
title: iOS app shell (SwiftUI)
phase: 3
priority: 2
status: needs-human
depends_on: [T-0104]
scope:
  - apps/ios/**
  - docs/mobile.md
  - .loop/evidence/T-0119/**
verify:
  - cargo xtask ffi --check
  - (xcodebuild — on a macOS runner, not this box)
---

## Goal

Phase 3's iOS half: the app shell the five screens live in — a SwiftUI target that links the
generated `ArreoCore` bindings and the staticlib, holds the device seed in the Keychain, and
keeps a session to the paired machine.

**This cannot be built or run on the dev box, and the task says so up front.** Xcode and the
iOS SDK exist only on macOS; ROADMAP §8's mitigation is the GitHub macOS runner, the same class
of gate as T-0090's Windows proof. Per `AGENTS.md`: never fake a macOS result. So this task is
`needs-human` in the sense that it needs a *different machine* and a human to wire it.

## Acceptance criteria

- [ ] A SwiftUI target that links the generated bindings and the `staticlib` and calls at least
      one exported function at launch (the version, or the built-in theme), proven by a build
      log from a macOS runner.
- [ ] **The seed lives in the Keychain, never in a file this repo chooses** — the boundary was
      designed for that (the seed comes *in*: `device_key_from_seed`), and the shell is where
      that promise is kept or broken. A build that writes the seed to `UserDefaults` is a
      finding, not a shortcut.
- [ ] The app runs on a simulator and reaches a real machine over the relay, with a screenshot
      of the first frame under `.loop/evidence/T-0119/`.
- [ ] Size discipline measured, not asserted: the app's binary and the linked Rust library are
      weighed against ROADMAP §3.5's "~2–6 MB", and the numbers are recorded (this is what
      T-0113's symbol count is for).

## Notes

- Blocked on a human decision as much as a machine: an Apple Developer account and a bundle id
  are the human's, and the store track (T-0123) needs them.
- The screens themselves are T-0121.
