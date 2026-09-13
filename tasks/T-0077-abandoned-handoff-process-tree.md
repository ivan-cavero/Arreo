---
id: T-0077
title: A timed-out handoff leaves the candidate's descendants behind
status: done
priority: 3
depends_on: []
phase: 2
---

# Goal

Kill the whole process tree of a handoff candidate that has to be abandoned, not
just the process this command spawned.

## Why this exists

Found by the adversarial pass on `arreo update --server` (T-0038 stage 1), with a
deliberately hanging candidate:

```console
$ arreo update --server --from ./hang-server --socket $S --timeout-secs 2
update: the handoff did not complete within 2s (the outgoing daemon, pid 93485, was still running at the last check)
update: nothing was installed; the daemon serving .../a.sock is still running (pid 93485)
exit=1
took 2s
hung child killed? NO
```

`Child::kill` signals one pid. The candidate was a `sh` script whose `sleep 60`
survived its parent — so a candidate that spawns anything before hanging leaks it.
The timeout path is the only place this command abandons a process it started.

**Exposure today is nil**, and that is why this is not urgent: an `arreo-server`
waiting on a handoff has adopted nothing and spawned nothing, so there is no tree
to leak. It stops being nil at **stage 2**, when the incoming daemon adopts PTY
masters before committing — a timed-out candidate would then be a process holding
live agents' terminals with no daemon serving them. That is the state ADR 0021's
whole design exists to make unreachable, arrived at through the one path that
abandons a process.

## Scope fence

`crates/arreo-cli/src/update.rs` (the timeout path in `wait_for_takeover`),
`crates/arreo-cli/Cargo.toml` (the group-kill syscall), and
`crates/arreo-cli/tests/update_server.rs` — **amended 2026-09-13**: the regression
test belongs beside the existing hanging-candidate test in the file that already
owns this verb's contract, rather than in a new file for one assertion.

## Acceptance criteria

- [x] The candidate is spawned into its **own process group**
      (`std::os::unix::process::CommandExt::process_group`, std, no new dependency).
- [x] On timeout the whole group is signalled, not the leader alone; the test
      spawns a candidate that forks a child and asserts **no** descendant survives.
- [x] The existing behaviour is unchanged on the happy path: a candidate that
      takes over is never signalled, and its process group is irrelevant to it.
- [x] If the group-kill needs a syscall the crate does not have, the dependency
      decision is recorded in the ledger per AGENTS.md — and the alternative
      (a supervisor process, or a `kill` subprocess) is named with its rejection
      reason rather than skipped.

## Verification

The repro is a three-line fixture: a script that answers `--version` and then
sleeps, run against a live daemon with `--timeout-secs 2`.

## Dependency decision (2026-09-13)

`rustix = { version = "0.38", features = ["process"] }` added to `crates/arreo-cli`.
**No crate enters the graph**: rustix 0.38.44 is already compiled for this target
through arreo-core's PTY adoption (`crates/arreo-core/Cargo.toml`, same version and
feature set), so there is no new supply-chain entry and nothing for `cargo vet` to
weigh. Rejected alternatives, named rather than skipped:

- **A `kill` subprocess** (`kill -9 -<pgid>`): resolves through `PATH`, which is
  exactly the thing a compiled-in safety check must not depend on — an attacker- or
  accident-controlled `kill` in front of it would decide whether a process holding
  live agents' terminals dies.
- **Hand-rolled `libc::kill`**: a *new* dependency (libc is not in the CLI's graph)
  plus `unsafe` for one syscall that rustix already wraps safely.

Spawn-side (`process_group(0)`) needs no dependency at all: it is
`std::os::unix::process::CommandExt`, stable since 1.64.

## Findings

- **The timeout path is the only place this command abandons a process**, which is
  why the fix belongs here rather than in a general "kill a tree" utility: nothing
  else in the update path starts a process at all.
- **A finding with no exposure today can still be load-bearing tomorrow.** Filed
  now, before stage 2 makes it real, because "the incoming daemon holds PTY masters
  and nothing is serving them" is exactly the failure this whole feature is built to
  prevent — and it would arrive by a path (a timeout) that reads as benign.
