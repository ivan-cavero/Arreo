---
id: T-0098
title: Plugin host v0 — a WASM component, a capability manifest, and a gate
phase: 4
priority: 3
status: proposed
depends_on: [T-0001]
scope:
  - crates/arreo-plugin-api/**
  - crates/arreo-core/src/plugin/**
  - crates/arreo-cli/src/main.rs
  - docs/plugins.md
  - .loop/evidence/T-0098/**
verify:
  - cargo test --workspace
  - cargo clippy --workspace --all-targets -- -D warnings
---

## Goal

ROADMAP §3.15 and §6 Phase 4: the plugin system is the code-shaped extension point, and
`crates/arreo-plugin-api` is an empty shell ("Empty shell per T-0001"). The Phase 4 exit
criterion names it directly: "a third-party 'hello widget' plugin hot-loads without touching
the core".

One sentence: a `.wasm` component with a declared capability manifest loads into the host, and
anything outside its declared capabilities **fails at the gate** rather than at 2 a.m. in a
render loop.

## Acceptance criteria

- [ ] `arreo-plugin-api` gains the host-facing types: the capability vocabulary from §3.15
      (`read-agent-state`, `add-widget`, `add-command`, `add-keybinding`, `notify`,
      `modify-theme-tokens`) and the component's exported interface, versioned.
- [ ] A manifest is **data and is parsed strictly**: unknown capability = refusal naming it
      (never ignored), missing manifest = refusal, version outside the host's window = refusal
      naming both. A capability a plugin did not declare is refused when it calls it, with the
      capability named in the error.
- [ ] `wasmtime` + the component model is the runtime; the dependency is new, so the ledger gets
      a note with the rationale and `cargo vet`/`cargo deny` exemptions are regenerated. Check
      the size probe (T-0020's budget) and say what the host costs — a plugin runtime that
      doubles the binary is a decision, not an accident.
- [ ] A **test plugin** lives in the repo (a ~30-line Rust component compiled in CI, checked in
      as bytes with its source) and the test battery loads it, calls it, and refuses it under a
      manifest that omits a capability it uses.
- [ ] Determinism and the tick budget are structural, not promised: a plugin call is bounded
      (fuel/epoch), a plugin that exceeds it is trapped and flagged, and the host never panics
      on a hostile module. Tests include a plugin that loops for ever and one that returns
      malformed data.
- [ ] `arreo plugins list` / `arreo plugins doctor` report what is loaded, its declared
      capabilities and its last trap — the operator's view of the gate.

## Notes

- Scope fence: this is the **host and the gate**. Sidebar widgets (T-0100), hot-reload and
  `arreo plugin new` (T-0099) are their own tasks, because each has its own failure mode.
- No plugin may reach a PTY, a file path or the network in v0; the capability list above is
  closed, and adding to it is a later, deliberate act.
