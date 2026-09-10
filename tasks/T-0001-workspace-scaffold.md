---
id: T-0001
title: Scaffold the Cargo workspace with crates, xtask skeleton and CI stub
phase: 0
priority: 1
status: in-progress
depends_on: []
scope:
  - Cargo.toml
  - rust-toolchain.toml
  - crates/*
  - xtask/*
  - .github/workflows/*
  - .gitignore
---

## Goal

A compiling Cargo workspace that every later task builds on: strict dependency direction so
an agent touching `arreo-core` cannot break the TUI.

## Acceptance criteria

- [ ] Workspace crates exist and compile: `arreo-core`, `arreo-server`, `arreo-cli`,
      `arreo-relay` (empty shell), `arreo-plugin-api` (empty), `xtask`.
- [ ] Dependency direction enforced: server/cli depend on core; nothing depends on server.
      Enforced with a cargo-deny/cargo-workspace check or a clippy lint (document which).
- [ ] `rust-toolchain.toml` pins the current stable toolchain.
- [ ] `cargo xtask e2e`, `cargo xtask bench`, `cargo xtask conpty-smoke` exist as runnable
      stubs that print "not implemented" and exit non-zero for unimplemented phases.
- [ ] CI workflow runs fmt + clippy (zero warnings) + test on ubuntu/macos/windows runners.

## Verification

```console
cargo build --workspace
cargo clippy --workspace --all-targets   # zero warnings
cargo xtask e2e                          # stub runs, exits 0
```

## Notes

- e2e/bench stubs must *fail* (non-zero) when invoked with `--enforce` once budgets exist;
  keep the distinction between stub and gate visible from day one.
- Evidence: build + CI run links to `.loop/evidence/T-0001/`.
