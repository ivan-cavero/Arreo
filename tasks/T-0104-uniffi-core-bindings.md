---
id: T-0104
title: UniFFI bindings — one core, three UIs, and the mobile half starts here
phase: 3
priority: 2
status: proposed
depends_on: [T-0025, T-0029, T-0044]
scope:
  - crates/arreo-core-ffi/**
  - docs/mobile.md
  - xtask/src/main.rs
  - .loop/evidence/T-0104/**
verify:
  - cargo test --workspace
  - cargo xtask ffi --check
---

## Goal

Phase 3 (ROADMAP §3.5) has **no tasks at all** — 0 of 60 filed — and its first bullet is "Rust
core via UniFFI; iOS and Android built in parallel — shared core means duplicated UI only". The
mobile UIs need Xcode and the Android SDK, neither of which exists on this box; the *bindings*
do not.

One sentence: the client-relevant core (pairing, device identity, the relay session, the
directory, the protocol codec, the theme engine) gets a typed UniFFI surface, so the two mobile
UIs are written against a generated API rather than against a Rust ABI.

## Acceptance criteria

- [ ] `crates/arreo-core-ffi` exposes the client surface with UniFFI: pairing (both sides), the
      device identity and its fingerprint, relay session connect/drain/ack, `machines` list, the
      message codec, and the theme engine's tokens. No PTY, no filesystem, no daemon — a phone is
      a client.
- [ ] Errors cross the boundary as **typed** errors with the same sentences the CLI prints (an
      `Error` enum, not a stringly-typed panic) — the "no silent divergences" rule T-0074
      applied to the TUI, applied here.
- [ ] `cargo xtask ffi --check` generates bindings for both target languages and **compiles what
      it generated** where a toolchain exists, reporting an honest SKIP where it does not (the
      `check-targets` pattern). Kotlin bindings can be compiled with a JDK if one is installed —
      probe and report; Swift needs macOS and is SKIPped with the reason.
- [ ] The generated surface has a contract test: a Rust-side smoke test drives pairing + a
      session through the FFI layer itself (not through the Rust API underneath it), so a
      boundary that cannot actually be crossed is caught here rather than in an iOS build.
- [ ] `docs/mobile.md` records the API surface, the two SKIP reasons, and what a mobile UI still
      has to bring (store, push, QR camera) — the honest split between this box and the
      toolchains it does not have.

## Notes

- This is deliberately the *only* Phase 3 task that can be finished here, and it is the one
  everything else in Phase 3 depends on. The iOS/Android UI tasks are drafted when this lands,
  and their proof is a store build — a different machine, the same class of gate as T-0090.
- UniFFI is a new dependency: ledger note with the rationale, and the alternatives considered
  (a hand-written C ABI, `cbindgen`, `flutter_rust_bridge`) recorded with why UniFFI wins for
  Swift + Kotlin from one definition.
