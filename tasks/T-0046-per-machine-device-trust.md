---
id: T-0046
title: Per-machine device trust — a grant on A is not a grant on B
phase: 2
priority: 2
status: in-progress
depends_on: [T-0018, T-0043, T-0044]
scope:
  - crates/arreo-core/src/mesh/trust.rs
  - crates/arreo-core/src/mesh/mod.rs
  - crates/arreo-server/src/mesh/trust.rs
  - crates/arreo-server/tests/trust.rs
  - crates/arreo-cli/src/machines.rs
  - specs/adr/0019-per-machine-device-trust.md
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
- [ ] `specs/adr/0019-per-machine-device-trust.md` records the decision and the rejected alternatives:
      an account-wide trust list at the relay, A acting as trust broker, and auto-extend on first
      cross-machine attach (the convenience that would silently void the model).
      **Fence corrected (2026-09-11): the number was 0010, which is
      `0010-pairing-spake2.md` — this ADR could never have written that file without clobbering a
      landed decision. Renumbered to 0019.**

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

## Progress (2026-09-11) — the model, landed and tested

Three layers are in, with 15 tests and no behaviour change yet (nothing is wired, so nothing can
regress while the rest is built):

- `arreo-core/src/mesh/trust.rs` — `TrustRecord` keyed `(machine_id, device_id)`, the pure
  role × verb rule (`evaluate`), and **one** denial builder that always names the machine, the role
  needed and the exact `arreo machines trust …` command. Enumerates every cell of the matrix, in both
  directions (a policy test that only asserts refusals passes with a function that refuses everything).
- `arreo_core::store` schema **v7** — the `machine_trust` table, with `trust_records` /
  `record_trust` / `revoke_trust` and a one-way `trust_initialized` marker. The machine is part of the
  primary key, so "a grant on A is not a grant on B" is a key constraint rather than a promise; the
  test asserts A's grant leaves B with **exactly zero rows**.
- `arreo-server/src/mesh/trust.rs` — the machine-local `TrustLedger`: its identity comes from the root
  key (the same `MachineId::from_key` the directory row uses, so there is one definition of "which
  machine is this"), plus `backfill_once`, which grants every already-pinned device the default role
  **once** — without it, an upgrade would lock out every device paired before this existed, and a
  marker that could re-run would silently heal a deliberate "revoke everything".

**Still to do in this task:** wire the ledger into `SessionAuth::check` (both gates: certificate, then
this machine's grant) and run the backfill at boot before any session is served; the socket verbs and
`arreo machines trust <device> [--machine] [--role] [--yes]`; `arreo devices revoke --machine`; the
`arreo-server/tests/trust.rs` acceptance tests; and the ADR.

**Decisions made while landing the model, for the ADR to record:** the grant reuses `Role` (accepting
the roadmap's `operator` as a spelling of `Owner`, one value, no translation table) rather than
inventing a trust-specific role enum; a *colliding* rename is refused while a *claim* gets the
suffix (T-0057's precedent — an explicit request means the name); a refusal is built in one place; and
an unreadable ledger is a *different* failure from a missing grant (the first is a store fault, the
second a decision).

## Verification

```console
cargo test -p arreo-server --test trust
cargo test --workspace
```
