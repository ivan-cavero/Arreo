---
id: T-0057
title: Directory writes over the wire — rename and remove, by the account that owns the name
phase: 2
priority: 2
status: done
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

- [x] Three request kinds, versioned v1 and additive (ADR 0017's N−1 rules): `rename`
      (`RenameRequest { v, machine_id, new_name }`), `remove` (`RemoveRequest { v, machine_id }`) and
      `stale` (`StaleRequest { v }`, T-0043's bulk prune), each answered by the existing `directory`
      reply kind.
- [x] **The rules stay in the directory and the relay applies them; the client is never trusted to.**
      A rename onto a live name is refused with the conflicting name and changes nothing (T-0043's
      `Directory::rename` — *not* the suffix rule, which is for claims: an operator who names a
      machine means that name, so a conflict is an answer, not a suffix). A name that is not a name
      is refused with the rule's own reason. An unknown `machine_id` is `NoSuchMachine`, which the CLI
      reports as exit 3.
- [x] `remove` is a tombstone, not a deletion: the name is held for `TOMBSTONE_SECS` and refused to a
      new machine that whole time, and the reply's row carries `tombstone_until_ms` so the caller can
      say until when. The bulk form is T-0043's `remove_stale` — exactly the stale set, idempotent —
      and the reply lists the rows it pruned (names included) so "what it reclaimed" is answerable
      without a second read.
- [x] The reply is the same `DirectoryReply` T-0056 defined — one reply type for four requests, routed
      by the request's `seq` — and a refusal is a `refused` string, never an empty list or a silent
      success.
- [x] `RelaySession` gains `rename_machine(machine_id, new_name)`, `remove_machine(machine_id)` and
      `prune_stale()`, on the same reserve-park-then-send path as `join_machine` (the reply must not
      be able to race its slot), with the same bounded wait and typed timeout.
- [x] `arreo machines rename <old> <new>` and `arreo machines remove <name> [--stale] [--force]` work
      through the CLI: both re-read the directory after the write and print the resulting row, exit 5
      on a name conflict, exit 3 on an unknown name, and never leave partial state (a refused rename
      changes nothing — asserted by reading the directory back). `--stale` is the bulk prune (and
      prints every reclaimed name); `--force` is the confirmation that an *online* machine may be
      tombstoned — a CLI-side check against the row's presence, so removing a machine that is
      answering right now takes one deliberate word, and no directory rule is invented for it.
- [x] `docs/relay-protocol.md` documents the two kinds and their refusal reasons; `docs/machines.md`
      loses its "not implemented yet" note for these two verbs and keeps it for `add` (which needs the
      join handoff, T-0058).
- [x] Evidence `.loop/evidence/T-0057/`: a two-machine transcript (rename onto a live name refused
      with nothing changed; rename to a free name observed by a second reader; remove, then a rejoin
      under a *different* key getting the suffix because the tombstone holds the plain name; `--stale`
      pruning exactly the stale set and being idempotent; `--force` tombstoning a machine that is
      online).

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

## Landing notes (2026-09-11)

**The rules stayed where they live.** The relay decodes, calls `Directory::rename` / `remove` /
`remove_stale`, and maps the outcome to a refusal string — it does not re-implement any of T-0043's
rules, and the client is never asked its opinion about whether a name is free.

**The ownership check is the authorization, and it is one answer.** The `machine` rows are keyed by
`machine_id`, which is unique across accounts, so without a check any account's device could rename or
tombstone another account's machine by guessing an id. Writes now require the machine to appear in the
session's own account, and the refusal is the same sentence an unknown id gets — so it is not an
oracle for "this machine exists somewhere else". Proven by removing the check and watching the
cross-account test go red.

**A rename conflict is refused, not suffixed.** T-0043's suffix rule belongs to *claims* (a machine
asks for a name it may not get). An operator who renames a machine means that name, so a conflict is
an answer: refused, both names untouched. The task's first draft said "suffix" and the landed
`Directory::rename` disagreed — the code was right.

**`remove --force` is a CLI-side confirmation, not a directory rule.** The directory's rule is about
names (a tombstone holds one); whether the operator meant to tombstone a machine that is answering is
about intent, so the check reads the row's presence and refuses without the word. `--stale` is
T-0043's bulk prune, exposed as the verb T-0043's own criteria named.

**Refusing flags beats ignoring them.** `--json` and `--offline` belong to the read verbs; a write
cannot answer from memory, and the write verbs print one line that is not a contract. Both are refused
with a reason rather than accepted and quietly ignored — the same judgment the first version of
`machines` applied to `add`.

**Found while proving it:** the tombstone flag was derived from presence, so a machine removed minutes
ago (presence `online`, name held) showed no flag at all. It now comes from the *row's*
`tombstone_until_ms` — a fact about the row, not about liveness — and the cache mirrors that field so
a cached tombstoned row cannot lose it.

## Verification

```console
cargo test -p arreo-relay --test router
cargo test -p arreo-cli --test machines
```
