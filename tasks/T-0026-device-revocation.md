---
id: T-0026
title: Device revocation — durable kill list, immediate cutoff, audited by whom
phase: 2
priority: 3
status: proposed
depends_on: [T-0018, T-0025]
scope:
  - crates/arreo-core/src/identity/revocation.rs
  - crates/arreo-core/src/store.rs
  - crates/arreo-server/src/devices.rs
  - crates/arreo-cli/src/main.rs
---

## Goal

Stolen phone, one command: `arreo devices revoke <name>` kills the certificate. The device
fails on its next connection, the revocation is durable across daemon restarts and long
offline gaps, and every revocation is an audit row naming who revoked what (§3.3, §3.14,
§4). Revocation is explicit — nothing expires by age, and nothing silently un-revokes.

## Acceptance criteria

- [ ] Durable list: migration v4 adds `devices.revoked_at` + `devices.revoked_by` and an
      `audit.action` column (default `'prompt'`, so existing rows keep their meaning). The
      revocation is committed synchronously before the CLI prints success, and survives
      `kill -9` + restart — asserted by revoking, restarting the real daemon, and observing
      the refusal in the same test/slice run.
- [ ] A revoked device fails on the *next* connection, not the next restart: the handshake
      consults the list, refuses with a typed `Error{ revoked }`, closes the socket and
      audit-logs. An already-live session for that device is terminated by the daemon after
      revocation (stream ends within ≤ 1 s), asserted by revoking mid-session.
- [ ] Revocation survives the 15-day offline case (§3.14): it depends on neither cert
      expiry nor a network fetch, so a device revoked while disconnected is refused when it
      reconnects days later. Asserted by revoking with the device offline, then reconnecting.
- [ ] `arreo devices revoke <name|id>` is idempotent (the second run prints "already
      revoked" and exits 0); `arreo devices list --revoked` shows tombstones; un-revoking
      requires a fresh pin and a full re-pairing — a burned key is never silently restored.
- [ ] A revoked device cannot re-pair inside a live pairing window: the pairing path
      refuses a revoked `DeviceId`, closing the "revoke, then re-pair the stolen key" hole.
- [ ] Audit records who revoked what: `action='device.revoke'`, `device=<revoker id>`
      (`local-cli` for a Unix-socket admin), `prompt=<target device id>`; `arreo audit`
      renders it and the row is read back in the test. The audit table stays append-only.
- [ ] Evidence under `.loop/evidence/T-0026/`: the revocation transcript, restart-proof
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
