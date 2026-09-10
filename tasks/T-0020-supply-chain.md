---
id: T-0020
title: Supply-chain hygiene — cargo vet/audit, release profile, cargo-dist skeleton
phase: 1
priority: 5
status: todo
depends_on: [T-0001]
scope:
  - deny.toml
  - cargo-vet.toml
  - .github/workflows/*
  - .cargo/**
---

## Goal

The security posture starts at build time, not at launch: reproducible-ish, audited,
signed-from-the-start releases.

## Acceptance criteria

- [ ] `cargo vet` + `cargo audit` in CI (zero unresolved findings = merge gate).
- [ ] Release profile: `panic=abort`, `strip`, `opt-level` tuned; binary size tracked in
      the bench harness (daemon ≤ 20 MB, assert in budget).
- [ ] cargo-dist pipeline skeleton produces signed binaries for the 3 OSes + install
      scripts (PowerShell + sh) wired to `arreo.dev` placeholders.
- [ ] `cargo deny` bans: no new unmaintained crates; dependency philosophy documented in
      CONTRIBUTING referenced from the check.

## Verification

```console
cargo vet check && cargo deny check
cargo xtask package --dry-run
```
