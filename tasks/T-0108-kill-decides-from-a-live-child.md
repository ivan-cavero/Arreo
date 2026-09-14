---
id: T-0108
title: The kill path decides "dirty" from a status read taken while the child may still be running
phase: 2
priority: 3
status: done
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

- [x] The kill path **uses** the wait result: a child that is still alive after the bounded wait
      means the worktree is **kept** (and the operator told), never removed. Deriving "dirty"
      from a read taken while a known-live process writes is not a decision, it is a race.
- [x] The narrow race inside `worktree::remove` (status read, then remove) is closed for the
      caller that matters: either re-check immediately before `git worktree remove` and refuse on
      a change, or have the caller hold the child's death as a precondition. Say which, and why,
      where the code lives.
- [ ] A test that fails on the pre-fix behaviour: a pane whose child ignores SIGTERM and
      outlives the wait (e.g. a script that traps the signal and keeps writing) must leave its
      worktree **present** with the file it wrote.
- [x] The doc line about "the child is dead by now" either becomes true or is corrected.

## Notes

- `wait_timeout` already returns the exit state (T-0052's work); this is discarding it for the
  removal decision.
- The dirty refusal itself is verified working — a killed pane with an untracked file keeps its
  checkout and logs the file by name. This task is about the window around it, not the rule.
- Report: `agent://T0091Security` (T-0091-03, CWE-367).

## Outcome — and one criterion that cannot be met as written

The kill path now **uses** the wait result: `still_running_after_kill(exit)` returns
the operator's line when `wait_timeout` came back `None` — still running after the
bound — and the checkout is then **kept and reported**. The decision is a function so
the rule is checkable on its own (the branch is not reachable on demand), and the call
site stays wiring: the same shape as `update::deferred::window_is_open`.

**Criterion 3 stays unticked, and the reason is a measurement rather than an
omission.** The criterion asks for a pane "whose child ignores SIGTERM and outlives
the wait". No child can: `kill_shared` sends **SIGKILL**, which cannot be trapped —
probed through the real `Pane` with a `trap '' TERM` shell, and it dies anyway
(`Some(Exited(1))`, `/proc` gone). `None` is reachable only by a process in
**uninterruptible sleep** (delivered signal, process stuck in a syscall) or by a
signal that was never delivered at all (a poisoned killer mutex) — neither of which a
test can produce deterministically. So no test can fail on the pre-fix behaviour for
this branch, and inventing one would be the fake the rules forbid.

What is proven instead:

- the **decision**, both ways, in `daemon::kill_tests`, mutation-proven: making the
  `None` arm return `None` reddens
  `a_process_that_outlives_the_wait_keeps_its_worktree`;
- the **guard for the residual window** — a descendant that outlives its leader and
  writes later — is **git's own refusal**, measured:
  `git worktree remove <dirty>` → `fatal: … contains modified or untracked files, use
  --force to delete it`, exit 128. The pre-check in `worktree::remove` is the *message*
  layer, not the guard, and that is now documented where the code lives, together with
  the rule that follows: **nothing may pass `force = true` on a path a live process
  could be writing to.**
- the **observable contract** — a killed pane never loses uncommitted work — was
  already pinned by `a_killed_pane_leaves_a_dirty_worktree_and_removes_a_clean_one`
  (T-0091). No second test was added for it: it would assert the same behaviour
  through the same path, which is padding.

## Evidence

`.loop/evidence/T-0108/` — `measured.txt` (the two facts, run verbatim: git's refusal
and the kill probe) and `kill-path-decision.txt` (the finding, the fix, and why the
hypothesis was half wrong).
