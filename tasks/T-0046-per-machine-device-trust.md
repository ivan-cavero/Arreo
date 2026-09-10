---
id: T-0046
title: Per-machine device trust — a grant on A is not a grant on B
phase: 2
priority: 2
status: proposed
depends_on: [T-0018, T-0043, T-0044]
scope:
  - crates/arreo-core/src/mesh/trust.rs
  - crates/arreo-core/src/mesh/mod.rs
  - crates/arreo-server/src/mesh/trust.rs
  - crates/arreo-server/tests/trust.rs
  - crates/arreo-cli/src/machines.rs
  - specs/adr/0010-per-machine-device-trust.md
  - .loop/evidence/T-0046/**
---

## Goal

Encode §3.7's trust model and §4's authorization row: each machine independently decides which devices
may touch its agents, roles are evaluated on the machine that owns the agents, and the relay stays
metadata-only. "A phone paired to the VPS is not automatically paired to the Pi" must be a test, not a
hope — and the fix for a refusal must be one obvious command.

## Acceptance criteria

- [ ] Record shape: per-machine grants keyed `(machine_id, device_id)` carrying `role`
      (`viewer`|`operator`), `granted_at`, `granted_by`, `revoked_at`; device identity comes from
      `arreo_core::identity::{DeviceId, DeviceCert, DeviceStore}` (consumed, never redefined). A test
      proves there is no account-wide trust row: a grant made on A must not authorize B, and querying
      B's grant set after A's grant returns exactly zero rows.
- [ ] Default grant: completing pairing with a machine grants that device `operator` on that machine
      only, recorded by the machine (not the peer, not the relay); the joining side cannot name a role
      in the join request — privilege escalation at join time is refused.
- [ ] Extending trust is explicit and target-side: `arreo machines trust <device> [--machine <name>]
      [--role viewer|operator]`, executed on or for the target machine, requires an existing owner
      grant on that machine, prints the device fingerprint being granted, and needs `--yes` or an
      interactive confirmation; an untrusted device cannot grant anything (exit 5), and neither A nor
      the relay can grant on B's behalf.
- [ ] Enforcement is per verb on the owning machine: `read`/`metrics` require `viewer`;
      `send`/`spawn`/attach-control require `operator`; a `viewer` device is refused `spawn`/`send`
      with exit 5 while `read`/`metrics` succeed — a role × verb matrix test asserts each cell,
      evaluated on B for a session targeting B.
- [ ] Revocation is local, immediate and complete: `arreo devices revoke <name> --machine <name>` on B
      drops only B's grant (the same device keeps working against A), takes effect on the next
      connection, and tears down that device's live B sessions within ≤ 5 s; a relay-only revoke must
      neither grant nor deny (a test performs one and asserts access is unchanged).
- [ ] Refusal path is actionable: every denial names the machine, the missing role and the exact granting
      command; `.loop/evidence/T-0046/` shows deny → grant → attach succeeding in both directions.
- [ ] Auditability: every grant, revoke and refusal appends an audit row (device, machine, action,
      timestamp) through T-0018's audit log; the rows are exportable and a test asserts none is
      silently missing.
- [ ] `specs/adr/0010-per-machine-device-trust.md` records the decision and the rejected alternatives:
      an account-wide trust list at the relay, A acting as trust broker, and auto-extend on first
      cross-machine attach (the convenience that would silently void the model).

## Notes

Two homes on purpose: `arreo-core/src/mesh/trust.rs` holds the record types plus pure role × verb
evaluation (unit-testable without a daemon), and `arreo-server/src/mesh/trust.rs` holds the
machine-local store and the enforcement call sites. Identity stays owned by `arreo-core/src/identity/**`
and server-side pinning/issuance by `arreo-server/src/devices.rs` (soft dependency on the Phase 2
pairing/transport task, separate file; this task reads that store and adds its own table, joining the
audit log through the device foreign key). Rejected: a single trust list owned by the account or relay
(one compromised relay or one mistaken click would unlock every machine), copying a "global role" per
machine (drift), and trust-by-name (names are renameable directory metadata).
Honest gaps: the pairing owner is the only `admin` in v1 with no separate admin UI, and team roles plus
device-policy inheritance are out of scope (§4 ships viewer/operator first).

## Verification

```console
cargo test -p arreo-server --test trust
cargo test --workspace
```
