# ADR 0019 — Per-machine device trust: the machine decides who may use it

Status: accepted (T-0046, model landed; enforcement wiring in the same task)
Context: ROADMAP §3.7 (multi-machine mesh), §4 (the authorization row), ADR 0009
(device identity), ADR 0012 (machine directory), T-0025 (roles and the verb
policy), T-0056 (the directory over the wire), T-0057 (directory writes)

## The problem

An account is a set of machines and a set of devices. The certificate a device
gets at pairing (ADR 0009) says what the *account* considers it to be, and it is
the same on every machine of the account. ROADMAP §3.7 asks for something the
certificate cannot express:

> each machine independently pairs its devices (a phone paired to the VPS is *not*
> automatically paired to the Pi — explicit per-machine grant, or
> `arreo machines trust <device>` to extend). Roles (viewer/operator) evaluated on
> the machine that owns the agents, never at the relay.

The failure mode without it is concrete: the first device that touches a machine
would be trusted by it, and one mistaken click on one machine would unlock every
machine in the account.

## Decision

**Two facts, two homes, and the machine is part of the grant's identity.**

| Fact | Where it lives | Who decides | Same on every machine? |
| --- | --- | --- | --- |
| what a device **is** in the account | its certificate, issued at pairing | the account root | yes |
| what a device **may do on this machine** | that machine's own store | that machine | **no** |

- The grant is keyed `(machine_id, device_id)`, stored in the machine's own
  SQLite store (schema v7, `machine_trust`). The machine is in the **primary
  key**, so "a grant on A is not a grant on B" is a key constraint rather than a
  policy someone could forget to apply.
- `machine_id` is the machine's root key (`MachineId::from_key`) — the same
  definition the directory row uses (T-0056). One answer to "which machine is
  this", shared by the directory and the trust ledger.
- **Absence is refusal.** A device with no row is refused; there is no default
  role, no "unknown means viewer", no fallback to the certificate's role.
- A revocation **keeps the row** (`revoked_at_ms`) rather than deleting it: the
  question a revocation is asked afterwards is "who had access, and when did it
  stop", and a deleted row cannot answer it. Re-granting revives the row, which
  is the ordinary way an operator undoes a revocation.
- Both gates apply, in order: **who are you** (the certificate: pinned, not
  revoked, verifies under the account root) then **may you, here** (this
  machine's grant). A device can be perfectly valid in the account and have no
  access at all to a particular machine — which is the whole point.
- The grant **reuses `Role`**, and `Role::parse` accepts the roadmap's
  `operator` as a spelling of `Owner`. §4 and §3.7 say "viewer/operator" while
  ADR 0009's certificates say "owner" for the same thing; accepting both at the
  door keeps **one** value and avoids a translation table, which is where the
  "one fact, two spellings" defect hides.
- A **denial is built in exactly one place** (`mesh::trust::denial_message`) and
  always names the machine, the role the verb needs, and the exact command that
  fixes it. A refusal assembled per call site is three chances to forget the one
  part the operator needs.

## Rejected alternatives

**One account-wide trust list at the relay.** Rejected: it is the thing §3.7
exists to prevent. One compromised relay, or one mistaken click anywhere, would
unlock every machine — and it moves the decision to a party that does not own the
agents.

**A acting as trust broker: when A attaches to B, A's grants are copied to B.**
Rejected for the same reason as "auto-extend": it makes the first touch a grant.
This is deliberately *not* automated even though it would be more convenient —
the convenience is the vulnerability.

**Copying the certificate's role into each machine's store at pairing time.**
Rejected: a global role per machine drifts, because the account can change what a
device is while the copy stays behind. The account's role and the machine's grant
are different questions and must be able to have different answers.

**Trust by name rather than by id.** Rejected: names are directory metadata and
are renameable (T-0043/T-0057). A grant that followed a name would follow a
rename to a different machine.

**A separate trust-specific role enum (`viewer | operator`).** Rejected in favour
of accepting both spellings of the existing one: a second enum whose only job is
to be translated is a mapping table with a place to hide, and "does an operator
have Control?" would then have two answers.

**Deleting the row on revocation.** Rejected: it makes "was this device ever
trusted here, and when did that stop" unanswerable, and it makes a re-grant
indistinguishable from a first grant in any later reading of the store.

## The migration, which is part of the decision

Before this ADR, a paired device could use the machine that paired it; after it,
a device with no row cannot. The rows do not exist for anything paired earlier,
so an upgrade would lock out every existing device — and the symptom would read
as "the machine stopped trusting me" rather than as a migration.

So the daemon performs a **one-time backfill**: on first boot with this code, every
device already pinned gets the default grant, and a one-way `trust_initialized`
marker records that it happened. Both directions matter:

- Without the backfill, an upgrade breaks every existing pairing.
- Without the marker, an operator who revokes **every** device leaves a
  legitimately empty ledger, and a backfill that ran again would silently restore
  the access that was just taken away.

The default grant from a fresh pairing is `operator` (Control) on the machine that
paired it, and on that machine only — the device proved possession of its key to
that machine, which is what authorizes it *there*.

## Consequences

- **A machine's trust is not visible to the account.** There is no way to ask the
  relay "which devices may use machine B"; that is a question for B. This is a
  deliberate loss of convenience for the model §3.7 asks for.
- **Revocation is per machine and must be repeated per machine.** Cutting a
  device off from every machine in the account is N operations, not one. The
  alternative is an account-wide list, which is rejected above.
- **An unreadable ledger is not a refusal.** A missing row is a decision; a store
  fault is a failure to decide. Collapsing them would mean either ending sessions
  on a transient SQLite error or, far worse, treating an unreadable store as a
  grant. The two are distinct types (`LedgerError::Refused` vs `Store`).
- The ledger adds one indexed read per remote verb — the same shape and cost as
  the certificate check already on that path.

## Honest gaps

- v1 has no separate admin role and no per-verb grants: a grant is a role, and the
  role's capabilities decide. §4's `admin` is a Team-tier role.
- Grants are not propagated anywhere, so a device's access differs between
  machines with no way to see the fleet's picture in one place.
- The backfill grants every pre-existing device `operator`, not the least
  privilege it might have had. It could not do better: nothing recorded what these
  devices were doing before, and guessing narrower would break working setups.
