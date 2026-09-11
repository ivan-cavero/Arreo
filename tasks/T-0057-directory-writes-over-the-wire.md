---
id: T-0057
title: Directory writes over the wire — rename and remove, by the account that owns the name
phase: 2
priority: 2
status: proposed
depends_on: [T-0043, T-0056]
scope:
  - crates/arreo-core/src/relay/mod.rs
  - crates/arreo-core/src/relay/client.rs
  - crates/arreo-core/src/relay/session.rs
  - crates/arreo-relay/src/router.rs
  - crates/arreo-relay/tests/router.rs
  - crates/arreo-cli/src/machines.rs
  - crates/arreo-cli/tests/machines.rs
  - docs/relay-protocol.md
  - docs/machines.md
  - .loop/evidence/T-0057/**
---

## Goal

T-0043 gave the directory its rename and tombstone rules and T-0056 gave it a wire for *reading*
and for a machine asserting its own row. Nothing can rename or remove a machine over the wire, so
`arreo machines rename` and `arreo machines remove` — the two verbs an operator uses when a machine
is replaced or retired — cannot exist. This task is the write side, and the two CLI verbs on top of
it.

Split out of T-0044, which owns the CLI surface but must not invent a relay RPC inside a CLI fence.

## Acceptance criteria

- [ ] Two request kinds, versioned v1 and additive (ADR 0017's N−1 rules): `rename`
      (`RenameRequest { v, machine_id, new_name }`) and `remove` (`RemoveRequest { v, machine_id,
      force }`), each answered by the existing `directory` reply kind with the resulting row (or with
      the refusal reason).
- [ ] **Authorization is the account's, checked on the relay.** A session authenticated with an
      account certificate may rename or remove; a device with no account (no certificate) may not.
      The relay re-checks the *rename rule* rather than trusting the client: a name that is live for
      another machine is a conflict (`name_conflict` on the row, deterministic suffix, never a silent
      overwrite), and a name that is not a name is refused with the rule's own reason.
- [ ] `remove` is a tombstone, not a deletion: the row keeps its name reserved for
      `TOMBSTONE_SECS` while refusing that name to a new machine, and `force` performs T-0043's
      documented bypass. The reply names what the tombstone did (until when the name is held).
- [ ] The reply is the same `DirectoryReply` T-0056 defined — one reply type for four requests, routed
      by the request's `seq` — and a refusal is a `refused` string, never an empty list or a silent
      success.
- [ ] `RelaySession` gains `rename_machine(machine_id, new_name)` and
      `remove_machine(machine_id, force)`, on the same reserve-park-then-send path as `join_machine`
      (the reply must not be able to race its slot), with the same bounded wait and typed timeout.
- [ ] `arreo machines rename <old> <new>` and `arreo machines remove <name> [--force]` work through
      the CLI: both re-read the directory after the write and print the resulting row, exit 5 on a
      name conflict, exit 3 on an unknown name, and never leave partial state (a refused rename
      changes nothing — asserted by reading the directory back).
- [ ] `docs/relay-protocol.md` documents the two kinds and their refusal reasons; `docs/machines.md`
      loses its "not implemented yet" note for these two verbs and keeps it for `add` (which needs the
      join handoff, T-0058).
- [ ] Evidence `.loop/evidence/T-0057/`: a two-machine transcript (rename onto a live name refused
      with nothing changed; rename to a free name observed by a second reader; remove then a rejoin
      getting the suffix; `--force` reclaiming immediately).

## Notes

- Why the relay decides and not the machine that asked: the directory is the account's, the relay owns
  the only copy, and the collision rule needs to see every name at once (T-0043's `claim_locked` runs
  inside one `IMMEDIATE` transaction for exactly this reason). A client-side check would race.
- Rejected: letting a machine rename only itself (the operator's case is renaming a machine that is
  not answering — that is when a name is wrong), and a separate `delete` that removes the row
  (T-0043's tombstone exists so a returning machine cannot steal a name that was just retired).
- Honest gap: no role distinction — any account device may rename or remove. That is the same door
  T-0044's `add` and T-0046's grants have in v1 (the certificate means the owner paired it), and it is
  recorded rather than implied.

## Verification

```console
cargo test -p arreo-relay --test router
cargo test -p arreo-cli --test machines
```
