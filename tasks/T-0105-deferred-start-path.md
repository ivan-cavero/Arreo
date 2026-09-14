---
id: T-0105
title: The start path — promote a pending update, re-exec into it, and clean up `.prev`
phase: 2
priority: 2
status: proposed
depends_on: [T-0039]
scope:
  - crates/arreo-server/src/main.rs
  - crates/arreo-server/src/handoff.rs
  - crates/arreo-core/src/update/**
  - xtask/src/update_slice.rs
  - docs/release.md
  - .loop/evidence/T-0105/**
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice update --case deferred
---

## Why this is not T-0090

T-0090 is the Windows application point, and its proof is the Windows runner — nothing in it can
be type-checked on this box. But two of the criteria it was filed with are **platform-neutral**:
the automatic promotion at start and `.prev` cleanup. Leaving provable work behind an unprovable
gate is the split this task exists to undo, so they live here, where `cargo test` and the update
slice can prove them.

## Goal

T-0039 gave the operator `arreo update --apply-now` and a marker that reports what is waiting.
That still needs a human. This task makes the machine finish the job: **a start promotes the
pending update by itself**, so a deferred update lands on the next restart with nobody typing
anything.

## Acceptance criteria

- [ ] **The promotion happens in the start path**, before the daemon binds its socket or spawns
      any PTY, under the daemon's own lock, with the pane list read at that instant (at start it
      is empty — but the list is read rather than assumed, so the rule stays the one function).
      The window rule is not re-implemented: `deferred::window_is_open`.
- [ ] **The re-exec, and the trap it must not fall into.** Promoting at start does **not** update
      the process that is starting: the running image is already loaded, and on Windows the
      service launches the *old* binary (the whole reason the swap was deferred). A start that
      promotes and then serves would give a two-restart update and a machine that still reports
      "pending". So the daemon **re-execs into the installed binary after promoting and before it
      serves** — `execv` on Unix, spawn-then-exit on Windows — and the assertion is the property
      an operator cares about: **after one restart the daemon reports the new version and no
      marker is pending.**
- [ ] **The re-exec cannot loop.** After a successful promotion the marker is gone, so a second
      start finds nothing to do. If a marker is still promotable after the re-exec, the re-exec
      failed: that is **reported and not retried** (one attempt, one line, the old binary
      serving), never a boot loop. Tested with a staged artifact that is deliberately not
      runnable by the time the start happens.
- [ ] **`.prev` is deleted once a start has succeeded with the new binary** — the rollback slot
      is for the *next* start, not for ever (T-0039 deliberately did not ship an uncalled
      function; this is its caller). Deleting it is asserted not to happen when the start path
      did not promote anything, so a rollback stays possible until an update actually lands.
- [ ] **The marker still clears only on the version confirmation** (T-0039's rule, unchanged):
      a start that promoted but whose re-exec has not yet reported the new version leaves the
      marker in place, and `arreo update --status` keeps saying so.
- [ ] **Slice**: `cargo xtask e2e --slice update --case deferred` extended with the restart
      story — stage a deferred update, stop the daemon, start it again, and assert the daemon
      reports the new version and the marker is gone, with the re-exec visible in the daemon's
      log (one line naming the version it is re-execing into).
- [ ] `docs/release.md`'s deferral section updated: the operator no longer needs `--apply-now`
      at all, and the sentence that says a start promotes it is true of this code.

## Notes

- Re-exec on Unix keeps the pid, which is the nice property the update slice's "pids unchanged"
  check likes; on Windows it is a spawn-and-exit, so the pid changes and that is documented
  rather than hidden.
- The daemon must not re-exec while it is the **incoming** side of a live handoff
  (`--handoff-from`): that process has just adopted the listener, the lock and the panes, and
  re-execing would drop all three. One `#[cfg]`-free guard, asserted by a test.
