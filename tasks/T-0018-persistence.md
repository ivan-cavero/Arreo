---
id: T-0018
title: Persistence — sessions/topology/audit in SQLite with restart restore
phase: 1
priority: 4
status: done
depends_on: [T-0012]
scope:
  - crates/arreo-server/src/persist/**
  - crates/arreo-core/src/store/**
  - crates/arreo-core/src/pty.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-server/src/main.rs
  - crates/arreo-server/src/lib.rs
  - crates/arreo-core/src/lib.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-server/tests/persist.rs
  - crates/arreo-core/tests/store.rs
  - xtask/src/persistence_slice.rs
  - xtask/src/lifecycle_slice.rs
  - xtask/src/main.rs
  - .github/workflows/*
---

## Scope note (re-scoped by loop, turn 16)

Fence named directories that don't exist (`persist/**`, `store/**` as dirs —
implemented as `persist.rs`/`store.rs`). Beyond the letter but required:
`pty.rs` (ring pre-seed — the ONLY safe restore; keystroke replay proven to
execute), daemon/main hooks (save/restore/audit wiring), CLI `audit` verb
(criterion demands exportable), both test files, both xtask slices + CI step.
Reason written here, not silent.

## Goal

Herdr's killer feature, hardened: full layout + scrollback restore after daemon restart
or machine reboot; append-only audit log from day one (every prompt sent: device, agent,
timestamp).

## Acceptance criteria

- [x] Schema: sessions, panes topology, metrics rollups, audit events (WAL mode).
      v2 schema (panes/scrollback/audit over v1 meta+rollups); WAL; sidecar
      `<socket>.db`.
- [x] Restart restore test: 10 panes + scrollback → kill -9 → restart → layout, ring
      buffers and states identical (byte-level scrollback equality).
      Live proof 10/10 markers + `e2e --slice persistence` in CI. Respawn+replay
      strategy (fd-passing can't survive reboot); restored history pre-seeded
      (keystroke replay PROVEN to execute metachar lines — redesigned).
- [x] Audit log: append-only, exportable, secret-shape scan on write (prompts containing
      keys get redacted in the log). Every `send` audited (device/agent/ts);
      `[REDACTED:*]` keeps field names; key-never-on-disk proven live;
      `arreo audit` reads + exports.
- [x] Migration path exists from day one (versioned schema with migrations, not ad-hoc DDL).
      `migrate()` v1→v2 with user-data survival test; corrupt DBs heal aside
      (never fail-forever, evidence preserved).

## Verification

```console
cargo xtask e2e --slice persistence
```
