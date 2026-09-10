---
id: T-0010
title: Cross-OS portability gates — cargo-xwin + osxcross compile checks per commit
phase: 0
priority: 2
status: todo
depends_on: [T-0001]
scope:
  - xtask/src/**
  - .github/workflows/*
  - docker/**
  - docs/cross-os.md
---

## Goal

Portability errors surface in minutes on the Linux dev box, not when CI (or worse, a user)
finds them. This task is the concrete mechanism behind ROADMAP §3.11's "three-OS humility".

## Acceptance criteria

- [ ] `cargo xtask check-targets`: builds + clippy for `x86_64-unknown-linux-gnu`,
      `x86_64-pc-windows-msvc` (via cargo-xwin/xwin), `x86_64-apple-darwin` and
      `aarch64-apple-darwin` (via osxcross) — runs in < 5 min, wired as a pre-merge CI job.
- [ ] `docs/cross-os.md`: the strategy (cross-compile = builds; CI runners = behavior;
      Wine = quick checks only, never a claim; macOS-on-non-Apple = EULA violation, don't).
- [ ] A deliberate `#[cfg(windows)]`-style portability mistake is caught by the gate
      (test the test — evidence in `.loop/evidence/T-0010/`).

## Verification

```console
cargo xtask check-targets
```
