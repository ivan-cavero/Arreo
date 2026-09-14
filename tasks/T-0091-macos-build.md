---
id: T-0091
title: macOS builds again — gate the Linux-only nix/rustix APIs in pty/adopt
phase: 2
priority: 1
status: proposed
depends_on: [T-0038]
scope:
  - crates/arreo-core/src/pty.rs
  - crates/arreo-core/src/pty/**
  - xtask/src/check_targets.rs
  - xtask/src/main.rs
  - .loop/evidence/T-0091/**
verify:
  - cargo test --workspace
  - cargo clippy --workspace --all-targets -- -D warnings
  - cargo fmt --all -- --check
---

## Goal

`arreo-core` does not compile on macOS (`macos-latest`, 4 errors): `SendFlags::NOSIGNAL`
(Linux-only in `nix`), `ExitProbe::Pidfd` (variant cfg-gated, use-site at `adopt.rs:753`
not), `SocketFlags::NONBLOCK | CLOEXEC` (no such flags in `rustix::net` on this target,
`pty.rs:509`), plus an `unused_mut` warning (`adopt.rs:203`, fatal under `-D warnings`).

## Acceptance criteria

- [ ] `cargo build --workspace --all-targets` green on the `macos-latest` runner.
- [ ] Each of the four errors fixed at the API level (cfg-gated equivalent or portable
      call with a comment naming the platform difference) — no `cfg` that silently
      disables behavior without a test proving the macOS path does the same job.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean on macOS, including
      the `unused_mut`.
- [ ] Local cross-check recorded: `osxcross` macOS build attempted, pass or precisely
      documented miss (so the next macOS-only breakage is caught before the runner).
- [ ] No Linux behavior change: full workspace battery + handoff + compat slices green.

## Verification

```console
# On the runner: macos-latest build + clippy legs green.
cargo test --workspace   # local Linux half
```
