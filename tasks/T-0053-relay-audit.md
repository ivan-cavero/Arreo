---
id: T-0053
title: Relay audit log — what passed through the relay, metadata only
phase: 2
priority: 3
status: proposed
depends_on: [T-0029, T-0030, T-0033]
scope:
  - crates/arreo-relay/src/audit.rs
  - crates/arreo-relay/src/store.rs
  - crates/arreo-relay/src/main.rs
  - crates/arreo-relay/src/router.rs
  - crates/arreo-relay/tests/audit.rs
  - docs/audit.md
  - .loop/evidence/T-0053/**
---

## Goal

The relay's half of the §4 audit trail: who connected to it, what it refused, and what it had to drop
— as metadata only, in its own database, outliving every machine that goes offline (§3.14). T-0033
lands the machine half; this is the other writer, split out because it is a different process, a
different schema and a different lifetime.

## Acceptance criteria

- [ ] `relay_audit` through the single relay `store.rs` (the one connection and migration owner),
      append-only via API, ordered by `(ts_ms, rowid)` so a backwards clock never reorders history.
- [ ] Actions with outcomes: `session.connect` (device, account, peer, proto version),
      `session.disconnect`, `relay.refuse` (the reason: unknown account, bad certificate, bad proof,
      rate limit), `inbox.drop`, `inbox.expire` — each `ok | refused | expired`.
- [ ] **Metadata only, asserted by scan**: no pane content, no agent state, no payload bytes, no key
      material. The schema test fails if a column could hold any of them, and a scan of the database
      after a real exchange finds no marker string from the traffic (the same check T-0029 makes for
      its live path).
- [ ] Peer addresses truncate at write (**IPv4 /24, IPv6 /48**) and stay truncated in exports — the
      relay is the box most likely to be someone else's, and a full-address trail there is a location
      history of everyone who used it.
- [ ] `arreo-relay audit export --format jsonl|json --since <ts> --until <ts> --out <file|->`, with
      the same filter semantics and byte-identical output for identical filters as T-0033's machine
      export; redaction happens before the row is written, so no flag can un-redact.
- [ ] Retention: nothing prunes automatically; a documented offline `--before` prune is itself
      recorded as `audit.prune` with the removed count, and a guard warns at 100 MB.
- [ ] `docs/audit.md` (T-0033's file) documents the relay's fields and the machine-vs-relay split.
- [ ] Evidence under `.loop/evidence/T-0053/`: the relay's rows for a real exchange read back, the
      metadata-only scan, and an export sample.

## Notes

- Split out of T-0033, which owns the machine's `audit` table, its actions and its export. Both
  halves share `docs/audit.md` and the redaction rules; T-0033 lands the rules, this applies them.
- The relay's rows are deliberately **not** a mirror of the machine's: the machine knows which pane
  was touched and by which device, the relay knows only that bytes moved between two ids. Keeping
  them separate is what stops the relay's database from becoming a place where content could
  accumulate (§4, §3.7 "machine-local truth over relay hearsay").
- Rejected: one shared audit table in one database (the relay would need the machine's database, or
  the machine the relay's — neither is possible, and the lifetimes differ).
- Honest gaps: no hash-chained tamper evidence yet; relay actions attribute to device ids, not to
  roles (the relay has no role model).

## Verification

```console
cargo test -p arreo-relay --test audit
cargo xtask e2e --slice relay
```
