---
id: T-0033
title: Machine audit log — who connected, what they did, exportable and redacted
phase: 2
priority: 3
status: proposed
depends_on: [T-0018, T-0026, T-0029]
scope:
  # Split on starting the work (see "Re-scope" below): the relay's own audit
  # table is T-0053, because it is a different writer, a different database and a
  # different lifetime.
  - crates/arreo-core/src/store.rs
  - crates/arreo-server/src/audit.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-server/src/transport.rs
  - crates/arreo-server/src/relay_client.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-core/tests/audit.rs
  - docs/audit.md
  - .loop/evidence/T-0033/**
---

## Goal

ROADMAP §4 ("SQLite append-only log: every prompt sent, device, timestamp, agent touched.
`arreo audit` + export"): T-0018 shipped the *prompt* log; Phase 2 adds the connection and action
trail across machine and relay, redacted by rule and exportable.

## Acceptance criteria

- [ ] The machine `audit` table gains `action`, `outcome`, `peer`, `detail` in the next free
      ordered migration (T-0026 took v4; this never renumbers), append-only via API. (The relay's
      own `relay_audit` table is **T-0053** — see the re-scope below.)
- [ ] Actions with outcomes: `session.connect` (device, cert fingerprint, peer, proto version),
      `session.disconnect`, `attach`, `send`, `spawn`, `split`, `device.revoke` (T-0026),
      `relay.refuse` (T-0029), `inbox.drop`/`inbox.expire` (T-0030 counts) — each `ok | refused |
      expired`, so a review reads intent and result.
- [ ] Scripted-session proof: connect → attach → send → revoke → reconnect-refused yields exactly
      that `(action, outcome, device, pane)` sequence on the owning machine (store read-back,
      `arreo audit --json` in the slice), ordered by `(ts_ms, rowid)` so a backwards clock never
      reorders history. The relay's `session.*`/`relay.refuse` rows are T-0053's half of the same
      sequence.
- [ ] Redaction stated and enforced: prompts pass the existing secret scan (`[REDACTED:<label>]`);
      pairing codes, key material, Noise secrets and payload bytes are never written; peer addresses
      truncate at write (**IPv4 /24, IPv6 /48**) and stay truncated in exports. A scan of both DBs
      and every export finds no live secret and no full address.
- [ ] Export: the store's `audit_export` (unreachable today — nothing calls it) is wired to
      `arreo audit export --format jsonl|json --since <ts> --until <ts> --out <file|->`; the same
      filters over one store give byte-identical output, and redaction happens before the row is
      written, so no flag can un-redact. (`arreo-relay audit export` is T-0053's.)
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

## Re-scope (2026-09-11, on starting the work)

**Split, with T-0053 holding the relay half.**

The task bundled two audit logs that share a *shape* and almost nothing else: the machine's `audit`
table (written by the daemon, in the daemon's SQLite, on the machine that owns the panes) and the
relay's `relay_audit` (written by the relay process, in the relay's own database, on a box that
outlives every machine going offline). They have different writers, different schemas, different
lifetimes and different test harnesses — the relay's needs the real relay binary, the machine's the
real daemon — and the task's own notes say as much ("the relay audit is its own table").

The line is drawn at the process boundary: this task owns everything a *machine* records about what
reached it, and T-0053 owns everything the *relay* records about what passed through it. T-0032's
criterion ("a remote `send` lands as an audit row on the machine that owns the pane") is satisfied
by this half, which is why the split does not block the remote-attach work.

**Fence gained `crates/arreo-server/src/transport.rs` and `relay_client.rs`:** a `session.connect`
row has to be written where a session is accepted, and those are the two files that accept one
(direct transport and relay peer). Writing the schema without the writers would be the
"written but not rendered" defect again, one layer down.
