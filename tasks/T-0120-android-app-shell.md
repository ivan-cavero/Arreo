---
id: T-0120
title: Android app shell (Compose)
phase: 3
priority: 2
status: needs-human
depends_on: [T-0104]
scope:
  - apps/android/**
  - docs/mobile.md
  - .loop/evidence/T-0120/**
verify:
  - cargo xtask ffi --check
  - (gradle assembleDebug — needs the Android SDK + NDK)
---

## Goal

Phase 3's Android half, and the mirror of T-0119: a Compose app that loads the `cdylib` through
JNA, keeps the device seed in the Android Keystore, and holds a session to the paired machine.

**Also not buildable here.** The Android SDK, NDK and a JVM toolchain for the generated Kotlin
are absent (the box has a JDK, and T-0104 established that a JDK alone does not even compile
Kotlin). The Android leg runs on a Linux runner with the SDK installed, which is the cheaper
half of this problem — no macOS needed.

## Acceptance criteria

- [ ] A Gradle project that packages the `cdylib` for the four ABIs (arm64-v8a, armeabi-v7a,
      x86_64, x86) and calls an exported function at launch, proven by an `assembleDebug` log
      from a runner with the SDK.
- [ ] **The seed lives in the Android Keystore** — the same rule as T-0119's Keychain, and the
      same finding if it lands in `SharedPreferences`.
- [ ] `cargo xtask ffi --check`'s Kotlin compile SKIP is **lifted** on that runner (install
      `kotlinc` + JNA + kotlinx-coroutines-core, the three the SKIP names), so the gate's one
      non-SKIP compile path actually executes somewhere. That is a CI job, and it is the cheapest
      real proof this phase can get.
- [ ] The app runs on an emulator and reaches a real machine over the relay, with a screenshot
      under `.loop/evidence/T-0120/`.

## Notes

- The Android leg is the *better* first target of the two: it needs no Apple hardware, and it
  exercises the Kotlin bindings the gate already generates.
