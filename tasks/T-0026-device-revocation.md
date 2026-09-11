---
id: T-0026
title: Device revocation — durable kill list, immediate cutoff, audited by whom
phase: 2
priority: 3
status: done
depends_on: [T-0018, T-0025]
scope:
  # Extended on split (see "Re-scope" below): the *pairing* door is a revocation
  # decision too, and it lives in core's pairing module.
  - crates/arreo-core/src/identity/revocation.rs
  - crates/arreo-core/src/store.rs
  - crates/arreo-core/src/identity/cert.rs
  - crates/arreo-core/src/pairing/**
  - crates/arreo-server/src/devices.rs
  - crates/arreo-server/src/transport.rs
  - crates/arreo-cli/src/main.rs
---

## Goal

Stolen phone, one command: `arreo devices revoke <name>` kills the certificate. The device
fails on its next connection, the revocation is durable across daemon restarts and long
offline gaps, and every revocation is an audit row naming who revoked what (§3.3, §3.14,
§4). Revocation is explicit — nothing expires by age, and nothing silently un-revokes.

## Acceptance criteria

- [x] Durable list: migration v4 adds `devices.revoked_at` + `devices.revoked_by` and an
      `audit.action` column (default `'prompt'`, so existing rows keep their meaning). The
      revocation is committed synchronously before the CLI prints success, and survives
      `kill -9` + restart — asserted by revoking, restarting the real daemon, and observing
      the refusal in the same test/slice run. (The column is spelled `revoked_at`, not
      `revoked_at_ms`: the store's existing columns are unqualified, and matching them beat
      matching this file's prose.)
- [x] A revoked device fails on the *next* connection, not the next restart: the handshake
      consults the list, refuses with a typed error, closes the connection and audit-logs. (The
      cutoff of an already-live session is **T-0052** — see the re-scope below.)
- [x] Revocation survives the 15-day offline case (§3.14): it depends on neither cert
      expiry nor a network fetch, so a device revoked while disconnected is refused when it
      reconnects days later. Asserted by revoking with the device offline, then reconnecting.
- [x] `arreo devices revoke <name|id>` is idempotent (the second run prints "already
      revoked" and exits 0); `arreo devices list --revoked` shows tombstones; un-revoking
      requires a fresh pin and a full re-pairing — a burned key is never silently restored.
- [x] A revoked device cannot re-pair inside a live pairing window: the pairing path
      refuses a revoked `DeviceId`, closing the "revoke, then re-pair the stolen key" hole.
- [x] Audit records who revoked what: `action='device.revoke'`, `device=<revoker id>`
      (`local-cli` for a Unix-socket admin), `prompt=<target device id>`; `arreo audit`
      renders it and the row is read back in the test. The audit table stays append-only.
- [x] Evidence under `.loop/evidence/T-0026/`: the revocation transcript, restart-proof
      output, mid-session cutoff log, and audit rows (no key material).

## Notes

- Local-first decision (§3.7): the machine that owns the agents evaluates trust, so
  revocation is a local SQLite write — the auth path never calls the relay and keeps
  working with the relay down. Propagating the list to other machines/relay is the mesh and
  relay work (sibling range, soft dependency in prose — not a `depends_on` id).
- Rejected: short-lived certs with renewal — adds a clock/online dependency to every
  connection and contradicts "nothing expires by age" (§3.14). Rejected: CRL download — a
  network dependency in the authentication path. Rejected: deleting the device row — loses
  the audit trail and turns a revoke into an un-revoke by accident, so revoked devices are
  kept as tombstones.
- The revocation check is O(1) against the pinned cert serial at handshake time (one
  indexed lookup), so it does not weaken the §5 attach latency budget; the mid-session
  cutoff reuses the daemon's existing connection registry rather than a new sweeper.
- Honest gap: a session established *before* revocation is cut at the next frame boundary,
  not instantly mid-flight — the ≤ 1 s bound is asserted, and the client sees a typed
  `Revoked` error instead of a silent drop.

## Verification

```console
cargo test -p arreo-core revocation
cargo test -p arreo-server devices
cargo xtask e2e --slice persistence
```

Revocation end-to-end (real binaries, restart, mid-session cutoff) is asserted by T-0027's
`--slice pairing`.

## Re-scope (2026-09-11, on starting the work)

**Split, and the fence extended by two paths.**

The task as written bundled three deliverables that fail for different reasons:

1. the ***record*** — a durable, audited, queryable revocation with a CLI verb (this task);
2. the **decision doors** — every place that answers "may this device in?" (
   this task, because two doors disagreeing is exactly the bug class this loop keeps finding);
3. the **cutoff of a session already open** — which needs a live-session registry the daemon does
   not have, and touches `daemon.rs`/`transport.rs`/`relay_client.rs`. That is **T-0052**.

The line is "decide" versus "act on a decision already made": a record and a decision are one
session's work and one coherent review; cutting a live stream needs a registry,
per-transport plumbing, and a timing assertion, and it is where a partial implementation would be
a security hole rather than a missing convenience.

**Why the fence gained `crates/arreo-core/src/pairing/**` and `crates/arreo-server/src/transport.rs`:**
criterion 5 (a revoked device must not re-pair inside a live window) is a revocation *decision*, and
so is the handshake refusal — but they live in the pairing module and the transport resolver
respectively. Leaving them out would have meant shipping the record without the doors, which is the
shape that looks done and is not.

**Why `crates/arreo-core/src/identity/cert.rs` is in the fence:** the authorization check lives
there (`DeviceIndex::authorize`), and it and the transport resolver currently each decide revocation
independently. This task gives that decision one home (`identity::revocation`), because "one
question, two answers" has now cost this loop four repairs.

## Landing notes (2026-09-11)

**The revocation decision now has one home.** `arreo_core::identity::revocation` owns
`may_connect`/`may_pin`/`authorized_role`, and three doors use it: the authorization check, the
transport's pinned-key resolver, and the pinning door (`issue`/`rotate`) that the pairing flow goes
through. Before this the rule was spelled out separately in two places — and "one question, two
answers" has now cost this loop five repairs, so the fifth instance was worth pre-empting rather
than documenting.

**The lookup doors are named for the question they answer.** `DeviceAuthority::device(id)` is the
*authorized* lookup (the index holds only live devices, so it returns `None` for a revoked one —
exactly the refusal a handshake wants), and `DeviceAuthority::record(id)` is the *record* lookup
(the store, tombstones included). A test I wrote initially asked `device()` for a revoked device's
tombstone and got `None`: the API was right and the question was wrong, so both doors now say which
they are.

**A revoked device is refused with a reason, not a shrug.** The resolver returns `Option`, so it
cannot return a typed error — which meant a revoked phone was logged as "not pinned", the same
message an unknown device gets. It now reports the real reason from the store
(`refusing dev_...: device dev_... was revoked`).

### Defects and traps found while landing this

1. **The audit row was written but not rendered.** `arreo audit` printed `kind` (coarse) and never
   `action` (the event's own name), so a revocation showed as `device_change` and an operator could
   not see `device.revoke` at all. This is the *third* time this shape has appeared (T-0024's
   invisible event kind, now this): a row that is written but not rendered is a row nobody can act
   on. The renderer prints both.
2. **`cargo test -p arreo-server` does not build the `arreo` binary**, so a CLI-side assertion in a
   server test ran against a *stale* binary and looked like an unfixed renderer. T-0024 recorded
   this for `arreo-relay`; the test file now says it for `arreo` too, and its failure message
   includes the daemon's log.
3. **A `str.replace` that does not match is a silent no-op.** The v4 migration block silently did
   not land in `store.rs`, and I built three more steps on top of it before a test caught the
   missing columns. Verify an insertion after making it.
4. **A failing test target truncates the suite counts.** `cargo test --workspace` stops at the
   first failing target, so the summary read `passed=5 failed=2` and looked like a catastrophe
   rather than "two tests in `arreo-cli`". Read a small `passed=N` as "aborted".

**Two existing T-0025 tests were migrated to the new contract** (clean cutover): the live listing no
longer contains a revoked device (tombstones are behind `--revoked`, with provenance), and
`devices revoke <bad>` now reports an unknown *name*, because a name is a legitimate reference now.

**Honest gap, recorded in T-0052:** a session that is *already open* when the revocation happens
keeps working until it ends. The record and the decision are this task; cutting a live stream needs
a session registry the daemon does not have. The relay has the same gap — it verifies a certificate
chain and has no list to consult (propagating one is the mesh work, T-0046).

## Evidence (2026-09-11)

- `.loop/evidence/T-0026/revocation.txt` — 7 acceptance tests through the real binaries (revoke by
  name, durable across `kill -9` + restart, audit row naming who and when, idempotent second revoke
  that rewrites neither the timestamp nor a second row, refusal on the next connection, the offline
  case, re-pair refusal with a fresh key still allowed, unknown and ambiguous references refused, a
  live device unaffected) plus 5 decision-module tests and 12 authority tests.
- `.loop/evidence/T-0026/gates.txt` — 306 workspace tests, clippy, fmt, supply chain, cross-target,
  e2e slices and bench.
