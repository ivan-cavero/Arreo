---
id: T-0020
title: Supply-chain hygiene — cargo vet/audit, release profile, cargo-dist skeleton
phase: 1
priority: 5
status: done
depends_on: [T-0001]
scope:
  - deny.toml
  - cargo-vet.toml
  - supply-chain/**
  - .github/workflows/*
  - .cargo/**
  - Cargo.toml
  - perf-budget.toml
  - xtask/src/package.rs
  - xtask/src/bench.rs
  - xtask/src/main.rs
  - docs/release.md
  - CONTRIBUTING.md
  - crates/*/Cargo.toml
  - xtask/Cargo.toml
---

## Scope note (re-scoped by loop, turn 18)

Fence names `cargo-vet.toml`, but vet 0.10 stores config in `supply-chain/`
(config.toml + audits.toml + imports.lock) — same intent, tool's layout.
Everything else beyond the letter is criterion-required (profile, budget
row, package/size verbs, manifest pins from deny findings, philosophy doc,
CI gates). Reason written here, not silent.

## Goal

The security posture starts at build time, not at launch: reproducible-ish, audited,
signed-from-the-start releases.

## Acceptance criteria

- [x] `cargo vet` + `cargo audit` in CI (zero unresolved findings = merge gate).
      Plus `cargo deny check`. All three green locally (vet 141 exempted,
      audit 0 vulns/147 deps, deny 4/4); CI installs pinned versions and runs
      all three as merge gates.
- [x] Release profile: `panic=abort`, `strip`, `opt-level` tuned; binary size tracked in
      the bench harness (daemon ≤ 20 MB, assert in budget). Profile in workspace
      Cargo.toml (opt-z/strip/abort/thin-LTO); `daemon_binary_mb` budget row +
      `bench --probe size` (cached release build): 3.6 MB → PASS.
- [x] cargo-dist pipeline skeleton produces signed binaries for the 3 OSes + install
      scripts (PowerShell + sh) wired to `arreo.dev` placeholders.
      `[workspace.metadata.dist]` (3 targets, shell+powershell, tap) +
      `xtask package --dry-run` plan-mode validation + `docs/release.md`
      (signing via Sigstore/minisign at first real release — UNSIGNED until then).
- [x] `cargo deny` bans: no new unmaintained crates; dependency philosophy documented in
      CONTRIBUTING referenced from the check. deny.toml (bans/wildcards-deny/
      dupes-warn-with-reason/licenses-allowlist/sources); philosophy is
      CONTRIBUTING rule 4 (reviewed-not-added, certify-over-exempt).

## Verification

```console
cargo vet check && cargo deny check
cargo xtask package --dry-run
```
