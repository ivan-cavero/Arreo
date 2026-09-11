---
id: T-0046
title: Per-machine device trust — a grant on A is not a grant on B
phase: 2
priority: 2
status: done
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

- [x] Record shape: per-machine grants keyed `(machine_id, device_id)` carrying `role`
      (`viewer`|`operator`), `granted_at`, `granted_by`, `revoked_at`; device identity comes from
      `arreo_core::identity::{DeviceId, DeviceCert, DeviceStore}` (consumed, never redefined). A test
      proves there is no account-wide trust row: a grant made on A must not authorize B, and querying
      B's grant set after A's grant returns exactly zero rows.
- [x] Default grant: completing pairing with a machine grants that device `operator` on that machine
      only, recorded by the machine (not the peer, not the relay); the joining side cannot name a role
      in the join request — privilege escalation at join time is refused.
- [x] **(landed in T-0059)** Extending trust is explicit and target-side: `arreo machines trust <device> [--machine <name>]
      [--role viewer|operator]`, executed on or for the target machine, requires an existing owner
      grant on that machine, prints the device fingerprint being granted, and needs `--yes` or an
      interactive confirmation; an untrusted device cannot grant anything (exit 5), and neither A nor
      the relay can grant on B's behalf.
- [x] Enforcement is per verb on the owning machine: `read`/`metrics` require `viewer`;
      `send`/`spawn`/attach-control require `operator`; a `viewer` device is refused `spawn`/`send`
      with exit 5 while `read`/`metrics` succeed — a role × verb matrix test asserts each cell,
      evaluated on B for a session targeting B.
- [x] **(landed in T-0059)** Revocation is local, immediate and complete.
      The criterion as written: `arreo devices revoke <name> --machine <name>` `arreo devices revoke <name> --machine <name>` on B
      drops only B's grant (the same device keeps working against A), takes effect on the next
      connection, and tears down that device's live B sessions within ≤ 5 s; a relay-only revoke must
      neither grant nor deny (a test performs one and asserts access is unchanged).
- [x] Refusal path is actionable: every denial names the machine, the missing role and the exact granting
      command; `.loop/evidence/T-0046/` shows deny → grant → attach succeeding in both directions.
- [x] **(landed in T-0059)** Auditability: every grant, revoke and refusal appends an audit row (device, machine, action,
      timestamp) through T-0018's audit log; the rows are exportable and a test asserts none is
      silently missing.
- [x] `specs/adr/0019-per-machine-device-trust.md` records the decision and the rejected alternatives:
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

### The wiring step was attempted and reverted, because it found a real gap

Wiring `SessionAuth::check` to the ledger was written, and it **broke the normal device flow**: a
device pinned *after* the daemon booted (`arreo devices issue --socket …`, which is how every test and
every operator adds one) has no grant row, so the new gate refused it. The boot backfill only covers
devices that existed when the daemon started. `crates/arreo-server/tests/audit.rs`'s remote test
failed on exactly this, which is the test doing its job.

So the gate cannot ship without **grant-on-issue**, and that turns out to be an architectural question
rather than a patch:

- `arreo devices issue` and the server half of `arreo pair` run in the **CLI process** against the
  authority directly (no daemon, and pairing may run with none). They are where a certificate is
  created, so they are where the default grant belongs.
- But the CLI **may not depend on `arreo-server`** (AGENTS.md's enforced rule: only `xtask` may), and
  `TrustLedger` lives there. So either the ledger's type moves to `arreo-core` (its store layer,
  `SessionStore::record_trust`, is already there), or the default grant is written through the daemon
  by socket verb — which fails when there is no daemon, exactly the case pairing runs in.

The first option is the one that fits the existing shape (the store layer is already in core, and the
CLI already writes device records directly). **That decision belongs in this task's remaining work,
not in a rushed patch**, and the ADR must record it: the ledger is a *store* concern with a policy
skin, and the policy skin is what the daemon owns.

Nothing was committed from the attempt; the tree is back to increment 1.

**Decisions made while landing the model, for the ADR to record:** the grant reuses `Role` (accepting
the roadmap's `operator` as a spelling of `Owner`, one value, no translation table) rather than
inventing a trust-specific role enum; a *colliding* rename is refused while a *claim* gets the
suffix (T-0057's precedent — an explicit request means the name); a refusal is built in one place; and
an unreadable ledger is a *different* failure from a missing grant (the first is a store fault, the
second a decision).

## What landed, and what did not (2026-09-11)

T-0046's mechanism landed in two increments; the three criteria it could not reach — the operator's
surface — landed in **T-0059**, which this task's own refusals named as the command to run. All eight
criteria are now met. The table below is the state at the end of the second increment, kept because it
is the record of what was still missing and why:

| Criterion | State |
| --- | --- |
| Record shape, keyed `(machine_id, device_id)` | **done** — the machine is in the primary key, so "a grant on A is not a grant on B" is a constraint; a test asserts A's grant leaves B with exactly zero rows |
| Default grant on pairing, target-side, no role from the joining side | **done** — `arreo pair` and `arreo devices issue` grant on the machine that issues, with the role they issued; the phone's hello still carries no role |
| Per-verb enforcement on the owning machine | **done** — the gate runs after authentication on every remote session (direct and relay), and the matrix, the refusal text and the no-grant/revoked cases are tested |
| Refusal names machine, role and command | **done** — one builder, asserted for the no-grant, wrong-role and revoked cases |
| The migration (existing pairings keep working) | **done** — a one-time backfill with a one-way marker; end-to-end transcript in `.loop/evidence/T-0046/backfill.txt` |
| **`arreo machines trust <device> [--machine] [--role] [--yes]`** | landed in T-0059 |
| **`arreo devices revoke <name> --machine <name>`** | landed in T-0059 |
| **Audit rows for grant / revoke / refusal** | landed in T-0059 (`trust.grant`/`trust.revoke`/`trust.refuse`, once per session for refusals) |

The enforcement wiring also found and fixed a real bug in this turn: **every early session exit
delivered nothing.** The refusal for a bad handshake — and T-0052's revocation error before its own
fix — was written into the duplex and discarded when the channel dropped, because only the *normal*
exit had the drain. The flush is now a wrapper around the whole session, so all seven exit paths
deliver their last frame.

## Closing note

Every criterion is met. The denial T-0046 built — the one that ends with a command — now names a
command that exists, and `crates/arreo-cli/tests/machines.rs` runs it and shows the grant taking
effect. The operator's surface, the audit trail and the two revocations (device-level and
machine-level) are T-0059's, with its own evidence under `.loop/evidence/T-0059/`.

## Verification

```console
cargo test -p arreo-server --test trust
cargo test --workspace
```
