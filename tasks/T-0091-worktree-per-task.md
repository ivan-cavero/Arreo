---
id: T-0091
title: Worktree-per-task — a pane that owns its own git worktree
phase: 4
priority: 1
status: done
depends_on: [T-0002, T-0014, T-0018]
scope:
  - crates/arreo-core/src/worktree.rs
  - crates/arreo-core/src/pty.rs
  - crates/arreo-core/src/proto/message.rs
  - crates/arreo-core/src/proto/codec.rs
  - crates/arreo-core/src/relay/config.rs
  - crates/arreo-core/src/store.rs
  - crates/arreo-server/src/**
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
uses worktrees + mobile approvals as their main loop". Every pane shared one working directory,
so two agents editing one checkout collide — the problem every multi-agent harness hits first.

One sentence: `arreo spawn <id> <program> --worktree [name]` gives a pane its **own** `git
worktree` on its own branch, so two agents on one machine cannot touch each other's files.

## Acceptance criteria

- [x] `arreo spawn <id> <program> --worktree [name]` creates (or reuses) a git worktree and
      starts the pane with that directory as its cwd. One root, named by the config
      (`[worktree] root`, default `<state>/worktrees`), one directory per pane id.
- [x] Branch naming is deterministic and collision-free (`arreo/<pane-id>` off the repo's current
      `HEAD`), and a second spawn of the same pane id **reuses** the existing worktree rather
      than failing or forking a second branch.
- [x] Not a git repository → refused before any directory is made, naming the path and the
      reason (`WorktreeError::NotARepo`). No silent fallback to the shared cwd.
- [x] `arreo worktrees list` shows pane id, path, branch, dirty state and liveness; it works with
      **no daemon** (liveness `null`, note on stderr so `--json` stays parseable).
      `arreo worktrees remove <pane>` refuses a dirty worktree without `--force`, naming the
      files, and never touches the main checkout.
- [x] Killing a pane removes its worktree only when it is clean; a dirty one is **kept** and
      reported (the daemon prints one line naming the pane and the files), because deleting an
      agent's uncommitted work is the one unrecoverable thing this feature can do.
- [x] Persistence: a restored pane comes back in its **own** worktree (store schema v10 carries
      the path), and a worktree whose directory vanished while the daemon was down is re-created
      from its branch rather than starting the pane in the wrong place.
- [x] Slice `cargo xtask e2e --slice worktree`: 9 checks on real git — two panes, two worktrees,
      disjoint file edits proven by writing the same filename in both and reading both back, the
      child's own `pwd`, the main checkout untouched, the dirty refusal naming the files, the
      live refusal (told apart from the dirty one), the clean remove with the branch kept, and
      `list --json` with and without a daemon.

## Scope note (what this touched beyond the original fence, and why)

The fence listed five files. The work needed four more, each for a reason worth recording:

- **`crates/arreo-core/src/proto/message.rs` + `codec.rs`** — the request had to reach the
  daemon. A **new variant** (`SpawnWorktree`), not a field on `Spawn`, because `rmp-serde`
  encodes a struct as a positional array and one more element would be unreadable by an older
  server — the N−1 window (T-0028/ADR 0017) forbids it.
- **`crates/arreo-core/src/store.rs`** — the pane record (schema **v10**) carries the worktree
  path, so a restore does not re-derive a machine's decision. The alternative was a worktree
  record in the daemon, which would be a second source of truth for "which directory is this
  pane in".
- **`crates/arreo-core/src/relay/config.rs`** — `[worktree] root`/`repo` in the file that
  already holds `[relay]` and `[tui]`: one parser, as T-0044 established.
- **`crates/arreo-core/src/pty.rs`** — `Pane::spawn_in_dir`, and with it a **defect fix**: a
  missing working directory was not an error, because `portable-pty` drops a `cwd` that is not
  a directory and falls back to the process's home. Now refused
  (`PtyError::NoSuchDirectory`).
- **`crates/arreo-server/src/{transport,relay_client,lib,main}.rs`** — the settings had to reach
  the remote and relay doors, or `spawn --worktree` would mean something different depending on
  which door the request arrived at.

## Verification

```console
cargo test --workspace                        # 842 passed, 0 failed
cargo test -p arreo-core --lib worktree       # 12 passed
cargo test -p arreo-server --test worktree    # 4 passed
cargo xtask e2e --slice worktree              # 9 passed, 0 failed
```

Evidence: `.loop/evidence/T-0091/worktree-per-task.txt` — the five proofs (unit, integration,
slice, the product surface by hand, and the restore path with a `kill -9`), the two defects a
worker found while wiring, and the five mutations.

## Notes

- `git worktree add` is the only mechanism: a copy would not share the object store, and a
  branch-and-stash dance is what worktrees exist to replace. No new crate.
- Prerequisite for T-0092 (diff review), which reads a worktree's diff.
- The TUI's worktree key is T-0074's surface and lands with the diff pane, not here.
- **`Message` is 112 bytes and is pinned by a test.** The first cut of the new variant took it
  to 136 (the variant duplicated `Spawn`'s fields, and the enum is as large as its largest
  variant); the payload is now boxed (`SpawnSpec`) and
  `the_message_enum_stays_at_its_budget` fails if a future variant raises it. T-0086 recorded
  the same number for the same reason.
