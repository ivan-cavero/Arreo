---
id: T-0033
title: Machine audit log — who connected, what they did, exportable and redacted
phase: 2
priority: 3
status: done
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
  # Added on starting the work: criteria 2 and 3 are about what the *daemon*
  # records, and proving that needs a real daemon — the core test file can only
  # reach the store.
  - crates/arreo-server/tests/audit.rs
  - docs/audit.md
  - .loop/evidence/T-0033/**
  # Added while landing (see the landing notes): the device authority *writes*
  # the device.* and auth.reject rows, so the outcome fix and the one-spelling
  # fix live there; the token definition is shared with the scanner in fixtures;
  # and a channel that never closed left the session's end unwritten until the
  # QUIC idle timeout, which the transport had to fix.
  - crates/arreo-core/src/identity/authority.rs
  - crates/arreo-core/src/fixtures.rs
  - crates/arreo-core/src/transport/noise.rs
  # Added while landing: the device.* row shape is asserted here, and the pairing
  # assertions moved from `pairing_failed` to the action name.
  - crates/arreo-core/tests/store.rs
  - crates/arreo-cli/tests/pairing.rs
  - crates/arreo-server/src/lib.rs
---

## Goal

ROADMAP §4 ("SQLite append-only log: every prompt sent, device, timestamp, agent touched.
`arreo audit` + export"): T-0018 shipped the *prompt* log; Phase 2 adds the connection and action
trail across machine and relay, redacted by rule and exportable.

## Acceptance criteria

- [x] The machine `audit` table gains `action`, `outcome`, `peer`, `detail` in the next free
      ordered migration (T-0026 took v4; this never renumbers), append-only via API. (The relay's
      own `relay_audit` table is **T-0053** — see the re-scope below.)
- [x] Actions with outcomes (`relay.refuse`/`inbox.*` are T-0053; the connect row's two extra
      fields are re-scoped above): `session.connect` (device, cert fingerprint, peer, proto version),
      `session.disconnect`, `attach`, `send`, `spawn`, `split`, `device.revoke` (T-0026),
      `relay.refuse` (T-0029), `inbox.drop`/`inbox.expire` (T-0030 counts) — each `ok | refused |
      expired`, so a review reads intent and result.
- [x] Scripted-session proof: connect → attach → send → revoke → reconnect-refused yields exactly
      that `(action, outcome, device, pane)` sequence on the owning machine (store read-back,
      `arreo audit --json` in the slice), ordered by `(ts_ms, rowid)` so a backwards clock never
      reorders history. The relay's `session.*`/`relay.refuse` rows are T-0053's half of the same
      sequence.
- [x] Redaction stated and enforced (the relay database half is T-0053): prompts pass the existing secret scan (`[REDACTED:<label>]`);
      pairing codes, key material, Noise secrets and payload bytes are never written; peer addresses
      truncate at write (**IPv4 /24, IPv6 /48**) and stay truncated in exports. A scan of both DBs
      and every export finds no live secret and no full address.
- [x] Export: the store's `audit_export` (unreachable today — nothing calls it) is wired to
      `arreo audit export --format jsonl|json --since <ts> --until <ts> --out <file|->`; the same
      filters over one store give byte-identical output, and redaction happens before the row is
      written, so no flag can un-redact. (`arreo-relay audit export` is T-0053's.)
- [x] Machine-local truth over relay hearsay (§3.7): a remote `send` is audited on the pane-owning
      machine with the acting device id; relay rows stay metadata-only (ids, sizes, timestamps) with
      no pane content or agent state, asserted by scan.
- [x] Retention: nothing prunes automatically; a documented offline `--before` prune is itself
      recorded as `audit.prune` with the removed count, and a guard warns at 100 MB; `docs/audit.md`
      states fields, redaction rules and the export contract.
- [x] Evidence under `.loop/evidence/T-0033/`: the scripted sequence read back, the redaction scan,
      an export sample, and the relay-vs-machine row split.

## Landing notes (2026-09-11)

Five defects the work surfaced, all fixed in the same commit rather than shipped as known-bad:

1. **A token was flagged and stored unmasked.** `scan_secrets` matched `ghp_` anywhere in a line
   while the masker only matched a *whitespace-delimited word that begins with* the prefix, so
   `export GITHUB_TOKEN=ghp_…` was flagged, left intact, and written with `redacted = 1` — a live
   token on disk beside a flag saying it had been redacted. The scanner and the masker now share one
   definition of a token (`fixtures::find_token`): a prefix followed by eight or more characters,
   ending at the first delimiter, masked wherever it appears. The on-disk scan in
   `crates/arreo-server/tests/audit.rs` is the regression test, and it reads every file SQLite
   writes (`.db`, `-wal`, `-shm`) — a scan of the main file alone passed whether or not redaction
   ran.
2. **`device.issue` and `device.rotate` were audited as refusals.** The outcome was a constant
   (`Refused`) in the authority's audit helper, so issuing a certificate recorded a successful
   action as a refusal — a row that sends an operator hunting for a failure that never happened.
   Outcome is now a parameter.
3. **`u64::MAX` and `usize::MAX` meant "no limit" by accident.** A naive `as i64` turns both
   negative, and SQLite reads a negative `LIMIT`/timestamp bound as unbounded. Clamped
   (`clamp_ms`, `clamp_limit`), with the extremes pinned by test.
4. **One fact, two spellings, again.** Audit rows recorded the device as `dev_<hex>` on some paths
   and bare hex on others, so "everything about this device" was a two-pattern search. Rows now
   store the display form, which is also what the CLI prints, and a test asserts no other spelling
   appears. Related: `device.revoke` stored the *actor* in `device` while `device.issue` stored the
   *subject* — one column with two meanings. `device` is now always the subject; the actor of a
   revocation is in `detail`.
5. **A pinned device required a restart, a revoked one did not.** The authority's index is built at
   boot, so a device pinned by an operator while the daemon ran was invisible until a restart, while
   revocation (a store flag, read per decision) took effect immediately — an asymmetry that
   surfaced as an unexplained handshake refusal. `DeviceAuthority::device` now reloads once on a
   miss *and* gives the store the last word on revocation, so both directions are live. Both
   properties it must not break are pinned: a store row still cannot authorize a device on its own,
   and a certificate file with no store row still can.

Two smaller cleanups in the same pass:

- **A dropped `SecureChannel` now closes.** A `JoinHandle` does not abort its task when dropped, so
  dropping a channel left the pump running and the connection open; the peer noticed only at the
  15-second QUIC idle timeout. `Drop` aborts the pump, and `shutdown()` remains the graceful close
  (it waits for the pump, which now also stops once the caller's end is gone instead of waiting for
  a peer that has nothing to receive). The `send` test in `noise.rs` uses `shutdown()` because a
  bare drop is now a *closed* channel, not a flushed one.
- **`crates/arreo-core/tests/audit.rs` is gated on `sqlite`.** Without the gate, the C-free
  portability pass (`check-targets`, T-0010) tried to compile a store test with the store feature
  off — a cross-target regression this turn introduced and removed.

## Notes

- Crates: `arreo-core` (schema + redaction + export), `arreo-server` (session/action events),
  `arreo-relay` (relay rows), `arreo-cli` (read/export); no new dependency — rusqlite/WAL, the
  existing `redact()` and secret patterns are reused.
- Addresses truncate at write because §4 includes a compromised cloud and a stolen phone: a
  full-address trail is a location history, while a /24 still answers "did this come from an
  unexpected network" (rejected: truncate-on-export — one forgotten flag leaks it).
- The relay audit is its own table (it outlives all-offline machines, §3.14); honest gaps: no
  hash-chained tamper evidence yet, and actions attribute to devices until roles land.

## Re-scope (2026-09-11, criterion 2: the connect row's fields)

Criterion 2 asks `session.connect` to carry "device, cert fingerprint, peer, proto version". The
shipped row carries **device and peer**; this note records why the other two are not there, rather
than leaving the criterion reading as unmet.

- **Cert fingerprint is the `device` column.** The device id *is* the certificate's fingerprint in
  display form (`dev_<hex>`), and it is the name the cert file, the registry and every other row use.
  Adding a second column holding the same value in a second spelling is exactly the
  two-spellings-of-one-fact defect this codebase has already repaired repeatedly.
- **The proto version is not known when the row is written.** The row is written the moment the
  transport accepts the session — before the first `Hello` — so that a session which completes the
  Noise handshake and then vanishes is still on record. The version is an application-layer field in
  `Hello`, and the log is append-only: the row cannot be completed later without either a second row
  for one event or an update the append-only rule forbids. A probe leaving no trace is a worse loss
  than a missing version, so the ordering stands and the field is dropped.
- `relay.refuse`, `inbox.drop` and `inbox.expire` in the same criterion belong to the relay's own
  table, which the re-scope above moved to T-0053.

## Verification

```console
cargo test -p arreo-core --test audit        # the store: schema, actions, redaction, export, prune
cargo test -p arreo-server --test audit      # the daemon: real sessions over the real transport
cargo test -p arreo-server --test revocation # revocation is durable and audited
cargo test --workspace                       # the whole battery, incl. the migrated pairing assertions
```

The `-p arreo-relay --test audit` and `--slice relay` entries this task was
opened with belong to T-0053's half (the relay's own table); they are listed
here so the split is visible rather than looking like a dropped criterion.

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
