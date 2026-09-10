---
id: T-0018
title: Persistence — sessions/topology/audit in SQLite with restart restore
phase: 1
priority: 4
status: todo
depends_on: [T-0012]
scope:
  - crates/arreo-server/src/persist/**
  - crates/arreo-core/src/store/**
---

## Goal

Herdr's killer feature, hardened: full layout + scrollback restore after daemon restart
or machine reboot; append-only audit log from day one (every prompt sent: device, agent,
timestamp).

## Acceptance criteria

- [ ] Schema: sessions, panes topology, metrics rollups, audit events (WAL mode).
- [ ] Restart restore test: 10 panes + scrollback → kill -9 → restart → layout, ring
      buffers and states identical (byte-level scrollback equality).
- [ ] Audit log: append-only, exportable, secret-shape scan on write (prompts containing
      keys get redacted in the log).
- [ ] Migration path exists from day one (versioned schema with migrations, not ad-hoc DDL).

## Verification

```console
cargo xtask e2e --slice persistence
```
