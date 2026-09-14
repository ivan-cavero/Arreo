---
id: T-0092
title: The two ubuntu handoff tests fail on CI — exit-1 takeover + daemon never up
phase: 2
priority: 1
status: proposed
depends_on: [T-0038]
scope:
  - crates/arreo-cli/tests/update_server.rs
  - crates/arreo-cli/src/update.rs
  - crates/arreo-server/src/handoff.rs
  - crates/arreo-server/src/daemon.rs
  - .loop/evidence/T-0092/**
verify:
  - cargo test -p arreo-cli --test update_server
  - cargo test --workspace
---

## Goal

On `ubuntu-latest`, 10/12 `update_server` tests pass and two fail deterministically
(log 2026-09-14, quoted verbatim in evidence):
`a_running_daemon_is_handed_over_to_the_new_binary` (new daemon exits 1 before takeover;
note the new deferred-update output — `update pending … applies at next restart` with
`--apply-now`/`--status` — the test may be asserting live-handoff behavior on a path
that now defers) and `a_candidate_that_fails_the_handoff_changes_nothing` (daemon never
comes up on its socket).

## Acceptance criteria

- [ ] Both tests green on the `ubuntu-latest` runner, not just locally. The fix states
      what the failure was (product bug vs stale assertion vs CI environment) — "passes
      now" without a cause is not done.
- [ ] If the deferred-update semantics changed the contract, the test asserts the new
      contract (pending + `--apply-now` + `--status`) instead of the old live-handoff
      one — with the behavior difference named in the test, not silent.
- [ ] No weakening: timeouts raised only with a measured justification; no deleted
      assertions. A test that fails on CI and passes locally is evidence about its
      assumptions, and the fix names the assumption (T-0063's rule).
- [ ] Full `arreo-cli` suite + workspace battery green locally.

## Verification

```console
cargo test -p arreo-cli --test update_server
# On the runner: ubuntu-latest test leg green.
```
