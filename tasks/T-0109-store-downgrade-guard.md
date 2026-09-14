---
id: T-0109
title: The store has no downgrade guard, and T-0091 is the first feature whose data cannot be re-derived
phase: 2
priority: 4
status: proposed
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

- [ ] `open` refuses a store whose `schema_version` is **newer** than this binary's
      `SCHEMA_VERSION`, with a typed error naming both numbers and the remedy (run the newer
      binary, or start from a fresh state directory). Refusing beats silently truncating a store
      the operator's data came from.
- [ ] The refusal is a *typed* variant of `SessionError`, not a string, and the daemon's boot
      path reports it in the same loud style as the other store failures (no silent empty start).
- [ ] Forward migration is unchanged and still tested: a v9 store opens, gains the column, and
      its legacy rows keep program/args with `worktree` NULL.
- [ ] A test that fails without the guard: a store stamped `SCHEMA_VERSION + 1` is refused, and
      the file is **left as it was** (not rewritten to the older version — the current behaviour
      is the destructive half).
- [ ] `docs/release.md` records the rule where the N−1 protocol window is discussed: the store's
      window is forward-only, and what an operator sees if they run an older binary against a
      newer store.

## Notes

- Not a T-0091 regression; T-0091 only made the consequence visible. Filed as its own unit rather
  than folded into T-0106/T-0107 because the fix is in the store, not in the worktree path.
- Report: `agent://T0091Security` (T-0091-04, CWE-1188).
