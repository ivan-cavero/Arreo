---
id: T-0109
title: The store has no downgrade guard, and T-0091 is the first feature whose data cannot be re-derived
phase: 2
priority: 4
status: done
depends_on: [T-0018, T-0091]
scope:
  - crates/arreo-core/src/store.rs
  - crates/arreo-core/tests/store.rs
  - docs/release.md
  - .loop/evidence/T-0109/**
verify:
  - cargo test -p arreo-core --test store
---

## Why this exists

Found by the `security-reviewer` pass over T-0091, reproduced by simulating exactly the two
statements a v9 binary executes against a v10 store. `migrate()` unconditionally writes the
*running* binary's `SCHEMA_VERSION` into `meta`, and the `worktree` column is added only
forward. So a v9 binary opening a v10 store treats the column as extra data it ignores, rewrites
the version to 9, and its 7-column `save_topology` INSERT drops every recorded worktree on the
first snapshot — with no refusal and no log line. The next v10 binary then reads a v9 store with
no worktree values and restores every pane into the shared directory: the collision the feature
exists to prevent, silently.

This is **pre-existing** store behaviour — no minimum-version check has ever existed — and the
review says so. It is filed because T-0091 is the first feature whose stored data cannot be
re-derived from anything else: `program`/`args`/`scrollback` survive a downgrade because they are
re-recorded every snapshot, while a worktree path is machine state that a v9 binary does not know
to carry. "Old binary loses a column" changes from harmless to behaviour-changing here.

## Acceptance criteria

- [x] `open` refuses a store whose `schema_version` is **newer** than this binary's
      `SCHEMA_VERSION`, with a typed error naming both numbers and the remedy (run the newer
      binary, or start from a fresh state directory). Refusing beats silently truncating a store
      the operator's data came from.
- [x] The refusal is a *typed* variant of `SessionError`, not a string, and the daemon's boot
      path reports it in the same loud style as the other store failures (no silent empty start).
- [x] Forward migration is unchanged and still tested: a v9 store opens, gains the column, and
      its legacy rows keep program/args with `worktree` NULL.
- [x] A test that fails without the guard: a store stamped `SCHEMA_VERSION + 1` is refused, and
      the file is **left as it was** (not rewritten to the older version — the current behaviour
      is the destructive half).
- [x] `docs/release.md` records the rule where the N−1 protocol window is discussed: the store's
      window is forward-only, and what an operator sees if they run an older binary against a
      newer store.

## Notes

- Not a T-0091 regression; T-0091 only made the consequence visible. Filed as its own unit rather
  than folded into T-0106/T-0107 because the fix is in the store, not in the worktree path.
- Report: `agent://T0091Security` (T-0091-04, CWE-1188).

## Outcome

`migrate` reads the store's version **first, before any DDL or write**, and refuses a
store from the future with a typed `SessionError::SchemaTooNew { path, found,
supported }` naming both numbers and the remedy. Three decisions inside that, each with
a reason worth keeping:

- **`>` and not `>=`**, or every restart of a current machine would refuse its own
  store. The boundary is a test case.
- **The read moved above the `CREATE TABLE IF NOT EXISTS` batch**, so the refusal is a
  no-op rather than "mostly a no-op". A fresh store reads as version 0 and proceeds.
- **Not corruption, deliberately.** `is_store_corruption` matches only sqlite error
  codes, so `open` returns this straight through — no heal, no quarantine. That is the
  important half: quarantining *renames the store aside*, and here the store is
  perfectly good while the binary is the one that is too old. Renaming aside the newest
  data on a machine would be the worst possible response, and the test asserts no
  `.corrupt-*` file appears.

The daemon's boot path needed no change to be loud — the device authority loads from the
same store, so the refusal arrives through an arm that already exits with both lines.
Verified against a real binary: the daemon *refuses to start*, names both versions and
the remedy, binds no socket, and leaves the file unchanged.

## Evidence

`.loop/evidence/T-0109/store-downgrade-guard.txt` — the finding, the fix, the real-daemon
transcript, and the two mutations. The second mutation is the one that matters: with the
guard removed, a real daemon against a store stamped `99` leaves it at `('10',)` (the
silent rewrite the finding describes); with the guard in place the same run leaves
`('99',)`.

One finding considered and dismissed while writing, recorded so nobody re-files it:
`has_column` **errors** on a store with no `panes` table, so a hypothetical v9-stamped
store without one fails to open. Not a defect — no real store can be in that state (v2
creates `panes`), so a guard would defend a shape the migration itself makes impossible.
It surfaced only because a first draft of the test built exactly that impossible store;
the test now asserts the version boundary instead.
