---
id: T-0072
title: Harness-aware session resume — reattach resumes pi/opencode sessions, not just scrollback
phase: 4
priority: 2
status: proposed
depends_on: [T-0017, T-0018]
scope:
  - adapters/**
  - crates/arreo-core/src/state/**
  - crates/arreo-core/src/store.rs
  - crates/arreo-server/src/persist.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-server/tests/persist.rs
  - xtask/src/persistence_slice.rs
  - xtask/src/main.rs
  - .loop/evidence/T-0072/**
verify:
  - cargo test --workspace
  - cargo clippy --workspace --all-targets -- -D warnings
  - cargo fmt --all -- --check
  - cargo xtask e2e --slice persistence
---

## Goal

Today's restore (T-0018, `persist.rs`) respawns the recorded command and pre-seeds
scrollback as visual history — the child is fresh, the harness session is not resumed.
Closing and reopening must resume the harness session where the harness supports it
(`opencode --session <uuid>`, pi resume), falling back to today's respawn+history
where it does not.

## Acceptance criteria

- [ ] Per-pane record carries `harness + session-id` alongside program/args (schema
      migration, old rows restore as today — no orphan DBs).
- [ ] Each adapter declares its resume strategy in data (`adapters/*.toml` + native
      maps): resume argv builder for pi + opencode, explicit `none` for harnesses
      without resume. No per-harness code branches outside the registry.
- [ ] Boot restore uses the resume argv when present; resume failure falls back loudly
      to respawn+history (one bad record never blocks the rest, same contract as T-0018).
- [ ] Proven live against real pi + opencode CLIs: spawn → produce session → kill -9
      daemon → restart → pane continues the harness session (session continuity asserted
      via the harness, not just byte-equal scrollback). Unknown-harness pane proves the
      fallback path unchanged.
- [ ] Safety: resume argv is constructed, never replayed keystrokes (T-0018's proven
      hazard stays fixed); session ids are not secret-scanned away and never logged
      in plaintext beyond what the harness itself prints.

## Verification

```console
cargo test --workspace
cargo xtask e2e --slice persistence
```
