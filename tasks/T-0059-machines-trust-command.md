---
id: T-0059
title: "`arreo machines trust` — the operator's half of per-machine trust"
phase: 2
priority: 2
status: proposed
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

- [ ] `arreo machines trust <device> [--machine <name>] [--role viewer|operator]` exists and is
      documented in `--help`: it prints the device fingerprint it is about to grant (so a mistyped id
      is visible before it is acted on), requires `--yes` or an interactive confirmation, and exits 0
      on success.
- [ ] **An untrusted device cannot grant** (exit 5): `--set`-style verbs are authorized by the caller's
      own grant on that machine. A device that is not trusted there may read nothing and write
      nothing, and the refusal is the same one it would get for any verb.
- [ ] `arreo devices revoke <name> --machine <name>` cuts **only that machine's** grant. The same
      device keeps working against every other machine, asserted by a test that revokes on B and shows
      A still answers. Without `--machine`, the command revokes the *device* as it does today (a
      different, account-level fact) and says which machine(s) still hold a live grant, so the
      operator is not left with a device record that says "revoked" while a machine still trusts it.
- [ ] `arreo machines trust --list` shows this machine's grants with role, when granted, by whom, and
      whether it is live — sorted, and with `--json` following the schema-1 discipline (additive-only,
      a contract test that fails on a renamed key).
- [ ] Every grant, revoke and **refusal** appends an audit row through T-0018's log
      (`trust.grant`, `trust.revoke`, `trust.refuse`), naming the device, the machine and the acting
      device where there is one. The refusal row is written once per session, not once per refused verb
      (a client that retries in a loop must not be able to flood the log).
- [ ] The ledger's write path goes over the daemon's socket as verbs (not by opening the store from
      the CLI twice over): the CLI and the daemon must agree on *one* writer for a grant, or a
      concurrent `trust` and `add` could interleave. Whatever the design, it is recorded in the ADR
      0019 follow-up note with the reason.
- [ ] Tests: the CLI end to end (grant, refused grant by an untrusted device, revoke on B leaving A
      working, `--list`, the audit rows), plus unit coverage for the flag parsing and exit codes.
- [ ] Evidence `.loop/evidence/T-0059/`: the deny → grant → attach transcript T-0046 called for,
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

## Verification

```console
cargo test -p arreo-cli --test machines
cargo test -p arreo-server --test trust
```
