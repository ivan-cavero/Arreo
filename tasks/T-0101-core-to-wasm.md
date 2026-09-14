---
id: T-0101
title: The shared core compiles to WASM — the browser's third UI needs the same logic
phase: 4
priority: 3
status: proposed
depends_on: [T-0023, T-0025]
scope:
  - crates/arreo-core/Cargo.toml
  - crates/arreo-core/src/**
  - crates/arreo-core-wasm/**
  - xtask/src/check_targets.rs
  - docs/web.md
  - .loop/evidence/T-0101/**
verify:
  - cargo check --target wasm32-wasip2 -p arreo-core-wasm
  - cargo test --workspace
---

## Goal

ROADMAP §3.10: "Same Rust core, compiled to WASM for protocol/crypto/grid logic — the browser
client is a third thin UI over the exact same core as mobile (third UI, one logic)."

One sentence: the parts of the core a browser needs (protocol codec, device identity, pairing,
the theme engine) build for `wasm32` behind a thin façade crate, with the non-portable parts
(PTY, filesystem, `/proc`) absent by construction rather than stubbed.

## Acceptance criteria

- [ ] `crates/arreo-core-wasm` is a façade that re-exports the browser-relevant subset: the
      msgpack codec, `Message`/`AgentState`, the identity/pairing types, the theme engine, and
      the state engine's pure parts.
- [ ] It builds for `wasm32-wasip2` with **no `cfg` stubs inside `arreo-core`**: the split is by
      feature and by module, so the browser gets the same code the daemon runs, not a copy.
      `cargo check --target wasm32-wasip2 -p arreo-core-wasm` is the gate.
- [ ] The codec and theme engine pass the **same tests** on both targets (the test module is
      shared), and `check-targets` gains the wasm row with an honest PASS/SKIP.
- [ ] Randomness and time are injected on wasm (WebCrypto/getrandom's wasm backend and an
      injected clock), never read from a syscall that does not exist there.
- [ ] `docs/web.md` records what the browser can and cannot do from this subset, and what the
      next task (T-0102) has to add — the honest boundary, not a promise.

## Notes

- Prerequisite for T-0102 (the dashboard), which needs this before it can render anything.
- The `transport` feature (quinn/rustls) is **not** part of the wasm build: the browser speaks
  WebTransport through the platform, so the noise/QUIC stack stays out of the wasm binary.
