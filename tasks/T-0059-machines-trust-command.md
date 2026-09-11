---
id: T-0059
title: "`arreo machines trust` — the operator's half of per-machine trust"
phase: 2
priority: 2
status: done
depends_on: [T-0046]
scope:
  - crates/arreo-core/src/proto/message.rs
  - crates/arreo-core/src/mesh/ledger.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-cli/src/machines.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-cli/tests/machines.rs
  - docs/machines.md
  - .loop/evidence/T-0059/**
---

## Goal

T-0046 landed the mechanism: a machine's grant decides what a device may do on it, the gate enforces
that per verb, and a refusal names the exact command that fixes it. **That command does not exist
yet** — the refusal is currently advice an operator cannot follow, which is the one place T-0046 is
incomplete. This task is the surface: extending, listing and cutting grants from the CLI, with the
audit rows that make the changes reviewable.

## Acceptance criteria

- [x] `arreo machines trust <device> [--machine <name>] [--role viewer|operator]` exists and is
      documented in `--help`: it prints the device fingerprint it is about to grant (so a mistyped id
      is visible before it is acted on), requires `--yes` or an interactive confirmation, and exits 0
      on success.
- [x] **An untrusted device cannot grant** (exit 5): **re-scoped — see "The write path" below.** There is
      no device-facing grant verb at all, so no device can grant *anything*, from anywhere: the console
      is the only grant path in v1. The three checks that keep this honest are asserted instead: a
      device with no grant is refused every verb; a viewer is refused the Control-class verbs; and
      `--machine <other>` is refused, because trust cannot be delegated or acted on for another machine.
- [x] `arreo devices revoke <name> --machine <name>` cuts **only that machine's** grant. The same
      device keeps working against every other machine, asserted by a test that revokes on B and shows
      A still answers. Without `--machine`, the command revokes the *device* as it does today (a
      different, account-level fact) and says which machine(s) still hold a live grant, so the
      operator is not left with a device record that says "revoked" while a machine still trusts it.
- [x] `arreo machines trust --list` shows this machine's grants with role, when granted, by whom, and
      whether it is live — sorted, and with `--json` following the schema-1 discipline (additive-only,
      a contract test that fails on a renamed key).
- [x] Every grant, revoke and **refusal** appends an audit row through T-0018's log
      (`trust.grant`, `trust.revoke`, `trust.refuse`), naming the device, the machine and the acting
      device where there is one. The refusal row is written once per session, not once per refused verb
      (a client that retries in a loop must not be able to flood the log).
- [x] **The write path (re-scoped; the reason is below).** The CLI writes through the store directly,
      exactly as `devices issue`/`revoke`/`rotate` already do, and a test proves a grant written that way
      is observed by a **running** daemon on its next verb — the property this criterion was really
      after. See "The write path, decided" below for why the socket-verb prescription was wrong.
- [x] Tests: the CLI end to end (grant, refused grant by an untrusted device, revoke on B leaving A
      working, `--list`, the audit rows), plus unit coverage for the flag parsing and exit codes.
- [x] Evidence `.loop/evidence/T-0059/`: the deny → grant → attach transcript T-0046 called for,
      in both directions.

## Notes

- The denial text T-0046 ships (`arreo machines trust dev_… --machine X --role operator --yes`) is the
  contract this task has to satisfy character for character; if the verb ends up different, the
  builder in `mesh::trust` changes with it, in the same commit.
- `--role operator` in that message and `Role::Owner` in the code are one value, two spellings already
  (ADR 0019); this is where the CLI has to accept the operator's word without leaking the other one
  into the refusal.
- Rejected: making the grant a socket verb only (the CLI would need a running daemon to administer a
  machine's trust, which is the wrong requirement for a recovery path), and writing those audit rows
  from the CLI (a client that writes its own audit row is a client that can write a false one).

## The write path, decided (2026-09-11)

This file contradicted itself: the criterion above prescribed "the ledger's write path goes over the
daemon's socket as verbs", while the Notes rejected exactly that ("the CLI would need a running daemon
to administer a machine's trust, which is the wrong requirement for a recovery path"). Resolving it by
reading the code rather than picking the sentence that was easier:

**The console path is store-direct, matching every other administrative command.** `devices_issue`,
`devices_revoke` and `devices_rotate` all call `open_authority(socket)`, which opens the machine's
store and identity directly — no socket message, no daemon. Making `machines trust` the single
exception would be a second pattern for the same job.

**Why the socket-verb prescription was wrong:**

1. **It breaks the case trust exists for.** Trust is what you reach for when a machine will not serve —
   a new device that cannot connect, a daemon that will not start. Requiring a running daemon to fix a
   machine's trust requirements makes recovery need the thing that is broken.
2. **The safety property is "one source of truth", not "one process".** The worry was that "a
   concurrent `trust` and `add` could interleave". They cannot tear anything: the ledger lives in one
   SQLite database in WAL mode, and `record_trust` is a single upsert on the primary key
   `(machine_id, device_id)`, which SQLite serializes. What was actually missing was a **busy timeout**
   — without one a second writer gets `SQLITE_BUSY` immediately — and that is now set, so a concurrent
   write waits its turn instead of failing.
3. **It is testable, so it was tested.** `a_grant_made_by_the_cli_reaches_a_running_daemon` writes a
   grant with the CLI and shows the *already running* daemon admit the device on its next verb. The same
   property holds for revocation (T-0052), and both work because the daemon reads the store rather than
   caching a grant.

**What the rejection in the Notes stands for, restated precisely:** a *device* must never write its own
grant or audit row, because a client that writes its own authorization is a client that can write a
false one. The console operator is not that client: opening this machine's store and identity is
already root-equivalent here (it is the same access that can issue certificates and read `root.key`),
so the machine recording its own administrative act is the machine acting, not a client claiming
something.

## Landing notes (2026-09-11)

All criteria met, with one re-scoped and one clarified — both recorded above rather than quietly
satisfied:

- **The write path** (criterion 6) is store-direct, and the safety property it was reaching for is
  now a test: `the_cli_cuts_and_restores_a_running_daemons_access` cuts and restores a grant with the
  daemon **running** and asserts the daemon's answer changes immediately. What the criterion's
  "one writer" worry actually needed was a **busy timeout** on the store — without it a second writer
  gets `SQLITE_BUSY` the moment the first holds the write lock, which an operator would have
  experienced as "this command randomly fails". Set to five seconds now.
- **"An untrusted device cannot grant"** is true in the strongest available form: there is no
  device-facing grant verb at all. The checks that keep it honest are asserted instead, including
  `--machine <other>` being refused — trust is local and cannot be delegated to, or by, another
  machine.

**Two things the work found, both fixed on the spot:**

1. **A viewer's refused `spawn` is not a trust refusal.** It comes from the *certificate* gate — the
   account's role is too low — so it writes no `trust.refuse` row. That is right (the fix is a
   different command: re-pair or re-issue), and my first test asserted the wrong gate. The refusal row
   is exercised by cutting a grant *mid-session*, which is the case where only this machine's ledger
   has changed.
2. **The refusal message printed the certificate's word for the role.** `--role owner` is a spelling
   nothing in `--help` or the docs mentions; the roadmap says `operator`. `Role::operator_term` now
   renders the operator's word on every user-facing surface, and the test asserts the *absence* of
   `owner` as well as the presence of `operator`, so the two cannot drift back.

Also: the refusal audit row is written **once per session** rather than per verb (a client that retries
must not be able to fill the log from outside), and each row records the machine both by name (for a
reader) and by id (so an exported row survives a rename).

## Verification

```console
cargo test -p arreo-cli --test machines
cargo test -p arreo-server --test trust
```
