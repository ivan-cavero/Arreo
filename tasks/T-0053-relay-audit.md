---
id: T-0053
title: Relay audit log — what passed through the relay, metadata only
phase: 2
priority: 3
status: done
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

- [x] `relay_audit` through the single relay `store.rs` (the one connection and migration owner,
      schema v5), append-only via `RelayStore::record` — there is deliberately no update or delete —
      ordered by `(ts_ms, rowid)` so a backwards clock never reorders history. Asserted by a test that
      pins two rows to one millisecond and reads them back in write order.
- [x] Actions with outcomes, a closed vocabulary in `arreo_relay::audit::actions`:
      `session.connect` (device, account, peer, proto version), `session.disconnect` (detail `clean`
      or `error`), `relay.refuse` (unknown account, bad certificate, bad proof, **and the handshake
      budget**), `inbox.drop`, `inbox.expire`, `audit.prune` — each an `ok | refused | expired` from
      the machine log's own `AuditOutcome`. Refusals are written at all three sites and each carries
      the verifier's own reason, so "bad certificate" and "bad proof of possession" are distinguishable.
- [x] **Metadata only, asserted by scan**: the schema test pins the column set and fails if any
      column so much as *suggests* content, a secret or a payload; and a test that runs a real
      exchange through the real binary then scans every file in the relay's state directory — the
      database included — for the marker the payload carried. `detail` is capped at 240 characters,
      so a pathological identifier cannot become a place content accumulates.
- [x] Peer addresses truncate at write (**IPv4 /24, IPv6 /48**) via
      `arreo_core::store::truncate_peer` — the machine log's own function, so the two redact
      identically — and stay truncated in exports, because the row never held a full address. The
      type enforces it: `RelayAuditEvent::peer` takes a `SocketAddr`, and only the store turns it into
      text.
- [x] `arreo-relay audit export [--format jsonl|json] [--since MS] [--until MS] [--action NAME]
      [--out PATH|-]`, taking bare Unix milliseconds exactly as the machine's verb does and filtering
      through the machine's own `AuditQuery` type. The bytes come from one shared renderer
      (`arreo_core::store::render_export`, which T-0053 extracted from `audit_export`), so "identical
      filters, identical bytes" is a property of the code: a test asserts the relay's export equals
      the renderer's output for each format, and pins the key order and the empty-window shapes
      (JSONL: zero bytes; JSON: `[]`).
- [x] Retention: nothing prunes automatically; `arreo-relay audit prune --before MS` is offline and
      explicit, and records **itself** as an `audit.prune` row carrying the count it removed. The
      hourly sweep warns past 100 MB of text (`AUDIT_WARN_BYTES`), naming the prune command. The size
      is measured, not inferred from a row count.
- [x] `docs/audit.md` gained §11 (the relay's trail): schema, actions, redaction, the read path,
      retention, and an explicit "what this is not" list. §8 now points at it instead of calling the
      relay's log a future task, and the document's title/schema versions were stale (v5/v6 for a
      v7 build) and are corrected.
- [x] Evidence under `.loop/evidence/T-0053/`: the twelve-test transcript (a real session's connect
      and disconnect read back through the operator's own `audit export`, the two refusal cases, the
      metadata-only scan, and the store-level properties) plus a sample of the operator's path —
      an empty export, a prune, and the self-recorded row.

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

## Outcome

Done. `RelayStore::record` is the one writer (append-only by construction), the writers are the
router's connect/disconnect/refusals and the mailbox's own eviction/expiry, and the read side is
`arreo-relay audit export|prune` over the same renderer and filter vocabulary as the machine's log.

Three things the work turned up, all fixed in-scope:

- **A `u64` epoch cast to `i64` wraps negative.** A `--since` of `u64::MAX` (what a caller reaches for
  to mean "no lower bound") matched *every* row; a `--before` of the same deleted *none*. Both are the
  opposite of the request. The query now answers the impossible window directly instead of casting
  into it.
- **`note_expiry` deadlocked** by re-locking the store mutex while the caller still held it. Merely
  slow would have been tolerable; a hung `enqueue` is not. The lock is released before the trail is
  written, in both call sites.
- **A live QUIC session's disconnect is noticed at ~15 s**, not immediately — the connection idles
  out. The test waits past that and says why; the timing is a fact about the transport, not a promise
  anyone made.

Also corrected on the way: `accept_connection` now returns a typed `Accepted` (an open connection, or
the address of a peer over its budget), because the rate-limit refusal is an audit row and the
address was previously only in a log line.
