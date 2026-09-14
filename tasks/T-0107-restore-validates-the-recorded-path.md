---
id: T-0107
title: Restore must validate a recorded worktree path against the configured root, not trust it
phase: 2
priority: 2
status: done
depends_on: [T-0091]
scope:
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-core/src/worktree.rs
  - crates/arreo-core/src/store.rs
  - crates/arreo-server/tests/worktree.rs
  - docs/worktrees.md
  - .loop/evidence/T-0107/**
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice worktree
---

## Why this exists

Found by a `security-reviewer` pass over T-0091 and **reproduced twice**. `restore_worktree_dir`
derives both the pane name and the worktree *root* from the recorded path
(`file_name()` and `parent()`) and hands them to `worktree::ensure`, which does
`create_dir_all(root)` and `git worktree add <root>/<name>`.

The `is_safe_pane_id` gate therefore proves nothing on this route: the name came from an
absolute path's last component, so it is trivially "safe". Nothing compares the recorded path
against `worktree_root(settings)` — the containment that `path_for(root, pane) = root.join(pane)`
gives the spawn route for free has no counterpart here. Reproduced:

- a row naming `<abs>/outside/nested/x` made the daemon **create that directory** outside the
  configured root and register it as a real worktree on `arreo/x`;
- a row naming the repository's **main checkout** restored the pane with the main checkout as
  its cwd — the isolation silently off, with no error anywhere.

Reachability: it needs write access to `<socket>.db` (same uid, mode 0600) — but it *also* fires
without an attacker whenever a recorded path is legitimately outside the current root (a changed
`[worktree] root`, an old row), which is exactly why "the row says so" is not a permission.

`docs/worktrees.md` also claims restore "re-ensures the worktree by pane id"; it re-ensures by
recorded path. The doc is wrong either way and must end up matching the code.

## Acceptance criteria

- [x] The recorded path is **validated against the configured root** before it is used:
      `ensure`'s containment becomes an explicit check on the restore route (the recorded path
      must be `path_for(root, name)` for the configured root), and a path outside it is refused
      with a loud line naming the pane, the recorded path and the configured root.
- [x] What happens to a row whose path is outside the root is a **decision, not a fallback**:
      either skip the pane (recorded as a skip, naming why) or re-create it under the *configured*
      root from its branch. Pick one, write it in `docs/worktrees.md`, and say why in the task
      file — silently "using the recorded path anyway" is the defect.
- [x] A recorded path that is the **main checkout** (or any directory that is not a registered
      worktree of the repository) is refused, never used as a cwd.
- [x] `require_checkout` (or whatever replaced it) is not the only guard: the test must fail on
      a row that points *outside* the root at a path that does not exist yet, since that is the
      case that creates directories rather than merely entering them.
- [x] The docs sentence about "by pane id" is corrected.
- [x] Regression tests for both reproduced cases, in `crates/arreo-server/tests/worktree.rs`.

## Notes

- The store is a file the operator (or anything running as that uid) can edit; this is the
  "never trust a recorded path" rule, not a hardening nicety, because a path that arrives from a
  file is data — the same class as the machine-name rules of T-0043 and the T-0066 bootstrap.
- Full report: `agent://T0091Security` (T-0091-02, CWE-22/CWE-73, with the exact repro).

## The decision, and why

**A record from a different root is refused and the pane is skipped — never
re-created under the configured root.** Both options were on the table; refusal
wins because repair is the more dishonest failure: re-creating the checkout under
the new root would start the agent in a *different set of files* while the checkout
it was actually using sat in the old place with whatever it had not committed. The
refusal is prose naming the record, the path and the configured root, on the
already-wired `skip_loudly` path, so the operator decides: re-spawn the pane, or
point `[worktree] root` back at the directory its checkout is in.

The rule this task ends up applying is narrower than "never trust a file":
**a path from a record may be used to *find* something, never to *create* something.**
That is why the *remove* path keeps reading name and root out of the record — it can
only act on a worktree git reports as registered, and can never create — and why the
handoff manifest's worktree field (T-0106) is not a new boundary.

## Evidence

`.loop/evidence/T-0107/recorded-path-validation.txt` — the defect, the fix and its
two pieces of path math (`normalize` for the lexical `..` case a text comparison
would accept, `resolved` for the symlinked root that must *not* be refused), 15 unit
tests and 6 integration tests, and the mutation that restores the pre-fix body and
reddens both integration tests with the reviewer's own symptoms.

One self-inflicted test bug found while writing: the first version asserted the
worktree list did not contain the substring `outside` — which the repository's own
path contains, because that is what the test named its scratch directory. It now
asserts on the `arreo/` branch, which is what actually identifies an Arreo worktree.
