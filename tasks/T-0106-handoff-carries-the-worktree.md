---
id: T-0106
title: The live handoff must carry the worktree binding, or an update silently un-isolates every pane
phase: 2
priority: 1
status: done
depends_on: [T-0038, T-0091]
scope:
  - crates/arreo-core/src/proto/message.rs
  - crates/arreo-core/src/proto/codec.rs
  - crates/arreo-core/tests/compat.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-server/src/main.rs
  - crates/arreo-cli/src/update.rs
  - docs/worktrees.md
  - .loop/evidence/T-0106/**
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice handoff-abort
---

## Why this exists

Found by a `security-reviewer` pass over T-0091, **reproduced end to end** — and it fires on the
normal `arreo update` route, not an exotic one: `arreo update --server` hands the daemon over by
spawning `arreo-server --handoff-from <socket>`.

**A pane adopted across a handoff loses its worktree.** `HandoffPane` carries no worktree,
`handoff_pane()` never copies it, and `run_handoff()` builds its daemon with
`Daemon::new(socket)` — so the incoming daemon has neither the per-pane binding nor the
`[worktree]` settings. Four consequences, all reproduced:

1. the next snapshot writes `worktree = NULL` for the adopted pane — the record is gone;
2. killing it removes nothing (the kill path reads `entry.worktree()`, which is `None`), leaving
   a registered checkout no daemon will ever clean — `arreo worktrees list` shows it for ever;
3. after a `kill -9` and restart the pane comes back in the **daemon's own working directory**
   (reproduced cwd `/home/dev`) while its named worktree is still registered — i.e. the
   worktree isolation is silently off, which is the collision T-0091 exists to prevent;
4. a *new* `spawn --worktree` on the handed-off daemon ignores `[worktree] root` and uses
   `<state>/worktrees`.

## Acceptance criteria

All met; evidence in `.loop/evidence/T-0106/handoff-carries-the-worktree.txt`.

- [x] `HandoffPane` carries the worktree path, **`#[serde(default)]` and last**, so an *older*
      outgoing daemon's shorter array still decodes (the handoff direction is old→new: the
      outgoing daemon is the running binary, the incoming one is the candidate). Assert the
      decode of a manifest without the field, rather than assuming serde's sequence handling.
- [x] `handoff_pane()` copies it from the entry; `PaneEntry::adopted` sets it on the adopted
      entry. After a handoff, `git worktree list` and the store row still agree with what the
      pane's child reports as its `pwd`.
- [x] **The kill path works after a handoff**: killing an adopted pane with a clean worktree
      removes it, and a dirty one is kept and reported — the same contract as a non-adopted pane.
- [x] **A restart after a handoff restores the pane into its worktree**, not the daemon's
      directory (the reproduced defect's third half).
- [x] `run_handoff` resolves `[worktree]` from the config exactly as a fresh boot does — the
      same `--config` / `$ARREO_CONFIG` resolution, no second parser — so a new `spawn
      --worktree` on the handed-off daemon honours the configured root.
- [x] **`arreo update --server` forwards the config path to the handoff child.** It spawns
      `arreo-server --handoff-from <socket>` with no `--config` today, so an operator who
      configured the daemon with a flag gets a candidate with default settings. Either forward
      the flag or state, in `docs/worktrees.md`, exactly which spelling of the config is
      honoured across a cut — the current behaviour is neither documented nor intended.
- [x] `crates/arreo-core/tests/compat.rs` pins that the new `spawn_worktree` op tag classifies
      as a `Request` (today nothing does; the T-0091 review found the gap).
- [x] A regression test in `crates/arreo-server/tests/handoff.rs` (or the abort slice) that
      fails on the pre-fix code: spawn in a worktree → hand off → the binding survives.

## Repro (from the review, verbatim)

```console
S=target/test-scratch/T0106-sec; git init -q -b main $S/repo
printf '[worktree]\nroot = "%s/cfgroot"\nrepo = "%s/repo"\n' $S $S > $S/cfg.toml
ARREO_STATE_DIR=$S/state target/debug/arreo-server --socket $S/run/a.sock --config $S/cfg.toml &
target/debug/arreo spawn h1 /bin/sh -c "pwd > $S/run/h1-cwd.txt; sleep 900" --worktree --socket $S/run/a.sock
cd $S/repo && target/debug/arreo-server --handoff-from $S/run/a.sock --config $S/cfg.toml &
# then observe: the store row loses its worktree; kill removes nothing; a crash+restart
# puts the pane in the daemon's own directory
```

## Notes

- The handoff's own protocol window (`check_incoming_protocol`) already refuses a *newer*
  outgoing daemon, so the only direction that must decode a shorter manifest is old→new, which
  is what the `#[serde(default)]` field covers.
- Filed from `.loop/evidence/` of the review: `agent://T0091Security` (full payload, severity,
  CWE tags, and the exact commands).

## Evidence and mutations

`.loop/evidence/T-0106/handoff-carries-the-worktree.txt` — the fix, the three-part
defect, the regression test's run, and the three mutations that each redden one
criterion (M1 the manifest field, M2 `adopted`'s setter, M3 the handed-over daemon's
settings).

One writing defect worth recording: the test's first version ended with `new.wait()`,
and the handed-over daemon **never exits** (it keeps serving), so the run hung for
1200 s before being killed. It now drops the guard, which kills and reaps — the same
thing the neighbouring tests do for the incoming side.
