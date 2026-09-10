---
id: T-0039
title: Windows deferred update — swap only in the no-live-pane window
phase: 2
priority: 4
status: proposed
depends_on: [T-0018, T-0037, T-0038]
scope:
  - crates/arreo-server/src/handoff/windows.rs
  - crates/arreo-server/src/lifecycle.rs
  - crates/arreo-core/src/update/**
  - crates/arreo-cli/src/update.rs
  - xtask/src/update_slice.rs
  - xtask/src/main.rs
  - .github/workflows/ci.yml
  - docs/release.md
  - .loop/evidence/T-0039/**
---

## Goal

§3.13's fallback, made honest: on Windows the update stays safe but is not zero-cut. This
task defines exactly why the cut is deferred there, what a Windows user sees, and the one
window in which the swap is allowed — so "never forced while agents run" is enforced by
code, not promised in a doc.

## Acceptance criteria

- [ ] `docs/release.md` records the deferral evidence: ConPTY pseudoconsole handles *are*
      inheritable (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), but the read loop, conduit pipes and
      child bookkeeping are process-local, and a running image can only be renamed, never
      replaced — so a cut needs a restart regardless. Deferred on evidence, with the revisit
      condition, not on taste.
- [ ] Safe-window rule implemented as one function: **the only window is `live_panes == 0`**,
      evaluated while holding the daemon lock immediately before the swap, never in the same
      tick as a pane spawn. `arreo update --apply-now` exits non-zero while any pane lives and
      names them; at zero panes it performs a plain restart (nothing to hand off).
- [ ] Deferred path: verified (T-0036) → staged as `arreo-server.next` → promoted when the
      window opens → `arreo status`, `arreo update --status` and the TUI banner show
      `update pending v0.2.0 → v0.3.0 (applies at next restart)`. If the file is still locked
      at zero panes, the fallback is a persistent pending marker plus a warning naming the
      version — never a silent `DELAY_UNTIL_REBOOT`.
- [ ] A verified-but-unpromotable stage (truncated after verification, or a failed version
      check) refuses promotion, keeps the current binary serving, reports why, and does not
      retry in a loop.
- [ ] Windows Service integration: stop/start through `windows-service` control codes rather
      than signals; a service-started daemon promotes the staged binary before it binds the
      socket or spawns any PTY.
- [ ] `.prev` is deleted on the next successful start, and the pending marker clears only
      after `arreo status --version` confirms the new version.
- [ ] Windows CI proof: `cargo xtask e2e --slice update --case windows-deferred` with 3 live
      panes → pids unchanged across stage and swap, status reports pending, `--apply-now`
      refuses, and after the panes exit a restart serves the new version.

## Notes

- Nothing here forks update logic: verification, staging, the channel index, `.prev` and
  rollback are the same `arreo-core::update` code the Unix path uses; only the *application
  point* differs. A Windows-only update implementation is the bug this task exists to prevent.
- Why this is not half-dead: the swap happens before the daemon serves, so there is no
  interval with two daemons claiming the socket and none in which the binary path is missing.
- Crate surface: the Windows branch is a module under `arreo-server/src/handoff/` so the
  "unsupported on this platform" answer and the deferred procedure live next to the Unix
  path they replace — one place to read the platform matrix.
- Honest gap: real proof runs on GitHub's Windows runners (§3.11 anti-ported-later rule);
  Wine is a fast local smoke only and never a shipping claim (`docs/cross-os.md`).
- Phase 5 canary/staged rollout through the relay is out of scope; the pending marker is the
  hook that work will use.

## Verification

```console
cargo xtask e2e --slice update --case windows-deferred   # windows runner
cargo test -p arreo-core update
```
