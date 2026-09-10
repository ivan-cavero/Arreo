---
id: T-0010
title: Cross-OS portability gates — cargo-xwin + osxcross compile checks per commit
phase: 0
priority: 2
status: done
depends_on: [T-0001]
scope:
  - xtask/src/**
  - xtask/Cargo.toml
  - .github/workflows/*
  - docker/**
  - docs/cross-os.md
  - crates/arreo-core/Cargo.toml
  - crates/arreo-core/src/metrics/**
  - crates/arreo-core/tests/metrics.rs
---

## Scope note (re-scoped by loop, turn 7)

Full xwin/osxcross SDKs deferred with written reason (GBs of downloads +
Apple-SDK sourcing for one signal CI runners give free — see ledger + 
`docs/cross-os.md`). The `sqlite` feature gate on arreo-core (+ `cfg` on the
store test) is load-bearing for the lite pass and owned here. `docker/**`
untouched (no container needed — rustup std suffices for layer 1).

## Goal

Portability errors surface in minutes on the Linux dev box, not when CI (or worse, a user)
finds them. This task is the concrete mechanism behind ROADMAP §3.11's "three-OS humility".

## Acceptance criteria

- [x] `cargo xtask check-targets`: layered gate — full workspace check per
      target; C-build-script-only failures fall back to the C-free lite pass
      (`arreo-core --no-default-features`); rustc errors FAIL, SDK absence
      SKIPs with reason, `--enforce` turns SKIP into failure. Default targets:
      `x86_64-unknown-linux-gnu` (PASS) + `x86_64-pc-windows-msvc` (SKIP until
      SDK; pure-Rust PASS). Extra targets via `--targets` (darwin needs an
      osxcross SDK → SKIP with reason). Wired as a CI job (non-enforcing;
      matrix remains authority).
- [x] `docs/cross-os.md`: the strategy (layers + what each proves; Wine =
      quick checks only; macOS-on-non-Apple = EULA violation, don't; why no
      local full SDKs; feature-unification gotcha).
- [x] A deliberate `#[cfg(windows)]`-style portability mistake is caught by the gate
      (planted `std::os::unix` without cfg → FAIL `error[E0433]` exit 1;
      evidence in `.loop/evidence/T-0010/gate-negative.txt`; reverted clean).

## Verification

```console
cargo xtask check-targets
```
