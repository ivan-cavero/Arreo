---
id: T-0108
title: The kill path decides "dirty" from a status read taken while the child may still be running
phase: 2
priority: 3
status: proposed
depends_on: [T-0091]
scope:
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-core/src/worktree.rs
  - .loop/evidence/T-0108/**
verify:
  - cargo test -p arreo-server --test worktree
  - cargo xtask e2e --slice worktree
---

## Why this exists

Found by the same `security-reviewer` pass, reported as a **hypothesis** (code path shown, no
deterministic reproduction — the window is small and the test child really did die). It is filed
because the outcome it threatens is the one thing T-0091 forbids absolutely: deleting an agent's
uncommitted work.

The kill arm kills, waits at most 5 s, **discards the result** (`let _ =
entry.pane.wait_timeout(…)`), then calls `worktree::remove(…, force = false)` — whose dirty check
is a single `git status --porcelain` read followed immediately by `git worktree remove`. So:

- if the child outlives the 5 s window, or the process tree is not fully dead (portable-pty
  signals the session leader, not grandchildren in other process groups), then both the
  classification **and** the removal run against a live process;
- any file the child writes between the `git status` read and the `git worktree remove` is
  deleted with the checkout.

The daemon's own comment asserts "the child is dead by now"; the code does not enforce it.

## Acceptance criteria

- [ ] The kill path **uses** the wait result: a child that is still alive after the bounded wait
      means the worktree is **kept** (and the operator told), never removed. Deriving "dirty"
      from a read taken while a known-live process writes is not a decision, it is a race.
- [ ] The narrow race inside `worktree::remove` (status read, then remove) is closed for the
      caller that matters: either re-check immediately before `git worktree remove` and refuse on
      a change, or have the caller hold the child's death as a precondition. Say which, and why,
      where the code lives.
- [ ] A test that fails on the pre-fix behaviour: a pane whose child ignores SIGTERM and
      outlives the wait (e.g. a script that traps the signal and keeps writing) must leave its
      worktree **present** with the file it wrote.
- [ ] The doc line about "the child is dead by now" either becomes true or is corrected.

## Notes

- `wait_timeout` already returns the exit state (T-0052's work); this is discarding it for the
  removal decision.
- The dirty refusal itself is verified working — a killed pane with an untracked file keeps its
  checkout and logs the file by name. This task is about the window around it, not the rule.
- Report: `agent://T0091Security` (T-0091-03, CWE-367).
