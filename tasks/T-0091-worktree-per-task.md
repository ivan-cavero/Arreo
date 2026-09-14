---
id: T-0091
title: Worktree-per-task — a pane that owns its own git worktree
phase: 4
priority: 1
status: proposed
depends_on: [T-0002, T-0014, T-0018]
scope:
  - crates/arreo-core/src/worktree.rs
  - crates/arreo-core/src/pty.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-server/src/persist.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-server/tests/worktree.rs
  - xtask/src/worktree_slice.rs
  - xtask/src/main.rs
  - docs/worktrees.md
  - .loop/evidence/T-0091/**
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice worktree
---

## Goal

ROADMAP §6 Phase 4 opens with "worktree-per-task module", and its exit criterion is "a team
uses worktrees + mobile approvals as their main loop". Today every pane shares one working
directory, so two agents editing one checkout collide — the problem every multi-agent harness
hits first.

One sentence: `arreo spawn --worktree <name>` gives a pane its **own** `git worktree` on its
own branch, so two agents on one machine cannot touch each other's files.

## Acceptance criteria

- [ ] `arreo spawn <id> <program> --worktree [name]` creates (or reuses) a git worktree and
      starts the pane with that directory as its cwd. The worktree lives under one root the
      config names (`[worktree] root`, default `<state>/worktrees`), one directory per pane id.
- [ ] Branch naming is deterministic and collision-free: `arreo/<pane-id>` off the repo's
      current HEAD, and a second spawn of the same pane id reuses the existing worktree rather
      than failing or forking a second one.
- [ ] Not a git repository → refused **before** any directory is made, naming the path and the
      reason. A `--worktree` that silently fell back to the shared cwd would be the collision
      the feature exists to prevent.
- [ ] `arreo worktrees list` shows pane id, path, branch and dirty state; `arreo worktrees
      remove <pane>` refuses a **dirty** worktree without `--force` (naming the files) and
      never touches the main checkout.
- [ ] Killing a pane removes its worktree only when it is clean; a dirty one is kept and
      reported, because deleting an agent's uncommitted work is the one unrecoverable thing
      this feature can do.
- [ ] Persistence: a restored pane (T-0018) comes back in its **own** worktree, and a worktree
      whose directory vanished is reported and re-created rather than starting the pane in the
      wrong place.
- [ ] Slice `cargo xtask e2e --slice worktree` on real git: two panes, two worktrees, disjoint
      file edits proven by writing the same filename in both and reading both back; a dirty
      remove refused; a clean remove done; the main checkout untouched (`git status` before and
      after).

## Notes

- `git worktree add` is the only mechanism: a copy would not share the object store, and a
  branch-and-stash dance is what worktrees exist to replace. Shell out to `git` (already a
  hard dependency of the test battery, and no new crate for plumbing we do not own).
- This is the prerequisite for T-0092 (diff review), which reads a worktree's diff.
- Fence note: the TUI's worktree key is T-0074's surface and lands with the diff pane, not here.
