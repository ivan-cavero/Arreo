---
id: T-0033
title: Audit log — who connected, from where, what they did, what was revoked
phase: 2
priority: 3
status: proposed
depends_on: [T-0018, T-0026, T-0029]
scope:
  - crates/arreo-core/src/store.rs
  - crates/arreo-server/src/audit.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-relay/src/audit.rs
  - crates/arreo-relay/src/store.rs
  - crates/arreo-relay/src/main.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-core/tests/audit.rs
  - crates/arreo-relay/tests/audit.rs
  - docs/audit.md
  - .loop/evidence/T-0033/**
---

## Goal

ROADMAP §4 ("SQLite append-only log: every prompt sent, device, timestamp, agent touched.
`arreo audit` + export"): T-0018 shipped the *prompt* log; Phase 2 adds the connection and action
trail across machine and relay, redacted by rule and exportable.

## Acceptance criteria

- [ ] One schema, two writers: the machine `audit` table gains `action`, `outcome`, `peer`,
      `detail` in the next free ordered migration (T-0026 takes v4; this never renumbers), and the
      relay gets `relay_audit` through the single relay `store.rs`; append-only via API.
- [ ] Actions with outcomes: `session.connect` (device, cert fingerprint, peer, proto version),
      `session.disconnect`, `attach`, `send`, `spawn`, `split`, `device.revoke` (T-0026),
      `relay.refuse` (T-0029), `inbox.drop`/`inbox.expire` (T-0030 counts) — each `ok | refused |
      expired`, so a review reads intent and result.
- [ ] Scripted-session proof: connect → attach → send → revoke → reconnect-refused yields exactly
      that `(action, outcome, device, pane)` sequence on the owning machine plus the relay's
      `session.*`/`relay.refuse` rows (store read-back, `arreo audit --json` in the slice), ordered
      by `(ts_ms, rowid)` so a backwards clock never reorders history.
- [ ] Redaction stated and enforced: prompts pass the existing secret scan (`[REDACTED:<label>]`);
      pairing codes, key material, Noise secrets and payload bytes are never written; peer addresses
      truncate at write (**IPv4 /24, IPv6 /48**) and stay truncated in exports. A scan of both DBs
      and every export finds no live secret and no full address.
- [ ] Export: the store's `audit_export` (unreachable today — nothing calls it) is wired to
      `arreo audit export --format jsonl|json --since <ts> --until <ts> --out <file|->` and to
      `arreo-relay audit export`; same filters → byte-identical output, and redaction happens before
      the row is written, so no flag can un-redact.
- [ ] Machine-local truth over relay hearsay (§3.7): a remote `send` is audited on the pane-owning
      machine with the acting device id; relay rows stay metadata-only (ids, sizes, timestamps) with
      no pane content or agent state, asserted by scan.
- [ ] Retention: nothing prunes automatically; a documented offline `--before` prune is itself
      recorded as `audit.prune` with the removed count, and a guard warns at 100 MB; `docs/audit.md`
      states fields, redaction rules and the export contract.
- [ ] Evidence under `.loop/evidence/T-0033/`: the scripted sequence read back, the redaction scan,
      an export sample, and the relay-vs-machine row split.

## Notes

- Crates: `arreo-core` (schema + redaction + export), `arreo-server` (session/action events),
  `arreo-relay` (relay rows), `arreo-cli` (read/export); no new dependency — rusqlite/WAL, the
  existing `redact()` and secret patterns are reused.
- Addresses truncate at write because §4 includes a compromised cloud and a stolen phone: a
  full-address trail is a location history, while a /24 still answers "did this come from an
  unexpected network" (rejected: truncate-on-export — one forgotten flag leaks it).
- The relay audit is its own table (it outlives all-offline machines, §3.14); honest gaps: no
  hash-chained tamper evidence yet, and actions attribute to devices until roles land.

## Verification

```console
cargo test -p arreo-core --test audit
cargo test -p arreo-relay --test audit
cargo xtask e2e --slice relay
```
