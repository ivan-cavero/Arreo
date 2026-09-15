---
id: T-0105
title: The start path — promote a pending update, re-exec into it, and clean up `.prev`
phase: 2
priority: 2
status: done
depends_on: [T-0039]
scope:
  - crates/arreo-server/src/main.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-core/src/update/deferred.rs
  - xtask/src/update_slice.rs
  - docs/release.md
  - .loop/evidence/T-0105/**
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice update --case deferred
evidence:
  - .loop/evidence/T-0105/start-path.txt
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

## Design (decided by the planner — the contract)

The placement is the design, and it falls out of three facts about the start path:

1. **The socket lock is the only mutex that matters.** `Daemon::serve()` takes
   `ExclusiveLock` on `<socket>.lock` before it binds (probe → lock → re-probe → bind,
   T-0071). Promotion must happen under that lock — otherwise two daemons starting at once
   could both promote, or a promotion could race a live daemon's pane list. So the
   promotion lives *inside* `serve()`, not in `main.rs`'s argument parsing: after the lock
   is held and the second probe has refused a live socket, before the socket is bound,
   with the pane list read at that instant (empty at start — but read from the registry,
   not assumed, so the rule stays the one function `deferred::window_is_open`).
2. **The handoff path must not promote.** An incoming daemon (`--handoff-from`) inherits
   the listener, the lock, and live panes from the outgoing one (`serve_inherited`).
   Promoting there would swap the binary under a live cut and re-exec a process holding
   adopted panes. The guard is structural, not a flag: `serve_inherited` never calls the
   promotion; only the cold-start path in `serve()` does. One `#[cfg]`-free guard,
   asserted by a test.
3. **The re-exec is `execv`, not spawn.** On Unix the daemon replaces its own image after
   promoting and before serving, so the pid is unchanged (the update slice's "pids
   unchanged" check likes this) and there is exactly one process to supervise. No new
   dependency: `std::os::unix::process::CommandExt::exec` (gated `#[cfg(unix)]`, with a
   spawn-then-exit fallback on Windows that is documented, not hidden — the Windows proof
   is T-0090's). `check-targets` FAILS on ungated `std::os::unix`, so the gate matters.

The marker lifecycle is T-0039's rule, unchanged: promote swaps the files but does not
clear the marker; the marker clears only on the version confirmation
(`clear_after_confirm`), which the re-execed process reports on its next `--status`.
A start that promoted but whose re-exec has not yet reported leaves the marker in place.

The loop guard falls out of the same rule, not a counter: after promoting, the process
re-execs into the installed path; the new image reads the marker and asks "am I already
the pending version?" (`verify_runs(current) == pending.version`). If yes, it clears the
marker via `clear_after_confirm` and serves — never re-execs again. If the marker is still
pending and the current binary is *not* the pending version, the re-exec failed: that is
reported (one line, stderr) and **not retried** — the old binary serves. One attempt, no
boot loop, by construction rather than by promise.

## Acceptance criteria

- [x] **The promotion happens in the cold-start path**, after the socket lock is held and
      the second probe has refused a live socket, before the socket is bound, with the pane
      list read from the registry at that instant (empty at start — but read, not assumed).
      The window rule is not re-implemented: `deferred::window_is_open`. The handoff path
      (`serve_inherited`) never promotes — structural, asserted by a test.
- [x] **The re-exec, and the trap it must not fall into.** Promoting at start does **not**
      update the process that is starting: the running image is already loaded. So the
      daemon **re-execs into the installed binary after promoting and before it serves** —
      `execv` on Unix (`std::os::unix::process::CommandExt::exec`, `#[cfg(unix)]`),
      spawn-then-exit on Windows — and the assertion is the property an operator cares
      about: **after one restart the daemon reports the new version and no marker is
      pending.**
- [x] **The re-exec cannot loop.** The new image clears the marker via `clear_after_confirm`
      when it already reports the pending version and serves without re-execing. A marker
      still pending with a current binary that is *not* the pending version after a
      promotion means the re-exec failed: reported once on stderr, served as-is, never
      retried. Tested with a staged artifact that is deliberately not runnable by the time
      the start happens.
- [x] **`.prev` is deleted once a start has succeeded with the new binary** — the rollback
      slot is for the *next* start, not for ever (T-0039 deliberately did not ship an
      uncalled function; this is its caller). Deleting it is asserted not to happen when
      the start path did not promote anything, so a rollback stays possible until an update
      actually lands.
- [x] **The marker still clears only on the version confirmation** (T-0039's rule, unchanged):
      a start that promoted but whose re-exec has not yet reported the new version leaves the
      marker in place, and `arreo update --status` keeps saying so.
- [x] **Slice**: `cargo xtask e2e --slice update --case deferred` extended with the restart
      story — stage a deferred update, stop the daemon, start it again, and assert the daemon
      reports the new version and the marker is gone, with the re-exec visible in the daemon's
      log (one line naming the version it is re-execing into).
- [x] `docs/release.md`'s deferral section updated: the operator no longer needs `--apply-now`
      at all, and the sentence that says a start promotes it is true of this code.

## Notes

- Re-exec on Unix keeps the pid, which is the nice property the update slice's "pids unchanged"
  check likes; on Windows it is a spawn-and-exit, so the pid changes and that is documented
  rather than hidden.
- The daemon must not re-exec while it is the **incoming** side of a live handoff
  (`--handoff-from`): that process has just adopted the listener, the lock and the panes, and
  re-execing would drop all three. One `#[cfg]`-free guard, asserted by a test.

## Outcome

Done. The promotion runs inside `Daemon::serve()`, under the socket lock and before the
bind, with the pane list read from the registry; `serve_inherited` never promotes; the
re-exec is `execv` on Unix; the loop guard is the version confirmation; `.prev` is released
by the confirming start and by nothing else. Nine unit tests in
`daemon.rs::start_update_tests`, one integration test (`tests/deferred_start.rs`), the
update slice's restart story (7 new checks, 13 in the case), and `docs/release.md`.

**An independent review found one p1, and it was real.** Step 1 decided "the update has
taken over" from the *file the marker names* rather than from this process's own image, so a
daemon started from a different path than the marker records would announce the new version,
clear the marker and delete `.prev` while itself serving the old bytes — the update silently
lost on every later start, the rollback slot gone with it. Reproduced against the real
binary (a marker naming a stand-in reporting `arreo-server 9.9.9` made `target/debug/
arreo-server`, which reports `0.1.0`, announce 9.9.9 and destroy the rollback slot). Fixed:
the confirmation now requires the marker to name *this* process's own image (canonicalized
both sides) **and** that image to report the pending version; a marker for another binary is
left alone with one honest line. Two regression tests, one mutation-proven.

The review's other three findings: `.prev` is now released only when the marker was
*actually* cleared (p3, fixed); the older in-process test helpers were pointed at a scratch
state directory (p2, fixed — the identity check had already subsumed the live hazard, so this
is hermeticity rather than a guard); the unreachable `#[cfg(not(unix))]` re-exec arm was
deleted rather than left claiming a type-check it never got (p3, fixed). The lock-across-
`exec` argument and the handoff exclusion were both verified sound by the reviewer.

## Scope note

The fence lists `crates/arreo-server/src/main.rs`; no change was needed there — the promotion
lives inside `serve()`, which is where the lock is. Integration added
`crates/arreo-server/tests/deferred_start.rs` (the handoff exclusion the criteria demand) and
touched `crates/arreo-server/tests/{api,compat,notify}.rs` (state-dir isolation, review
finding 2). Recorded here rather than done silently.
