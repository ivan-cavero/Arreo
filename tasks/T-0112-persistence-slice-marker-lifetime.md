---
id: T-0112
title: The persistence slice's plain-pane marker is outlived by its own harness waits
phase: 4
priority: 3
status: done
depends_on: []
scope:
  - xtask/src/persistence_slice.rs
  - .loop/evidence/T-0112/**
verify:
  - cargo xtask e2e --slice persistence
evidence:
  - .loop/evidence/T-0112/persistence-marker.txt
---

## Why this exists

Found while verifying T-0093, and proven to be a slice defect rather than a product one.

`cargo xtask e2e --slice persistence` failed deterministically — twice — with

```
[info] persistence/live: pre-crash pane pids: []
[FAIL] persistence/live: the plain pane did not come back after the restart
```

while the product behaviour was right: the store's `panes` row for `live-plain` was intact and the
daemon logged `restored 3 pane(s)`. The failing check is the *process* one, and the process it
looks for is spawned as `echo plain-fallback-marker && sleep 60` (`persistence_slice.rs:457`).

## Root cause

The marker has a **fixed 60 s lifetime**; the check that looks for it sits *after* the post-restart
waits for pi and opencode to make a resumed turn, and those waits are bounded by
`HARNESS_RUN_TIMEOUT = 300 s` (`persistence_slice.rs:334`). On any run where the harness turns take
more than a minute the marker is already gone and a correctly-restored pane is reported missing.
The pre-crash capture shows the same race one line earlier: `pre-crash pane pids: []`.

## Acceptance criteria

- [x] The marker outlives every wait that precedes the check that looks for it: `sleep 60` →
      `sleep 600`, comfortably past `HARNESS_RUN_TIMEOUT` and still bounded, so a leaked process
      cannot outlive the run.
- [x] Proven by the one-token experiment: with `sleep 600`, `pre-crash pane pids: [1860665]` (the
      marker is found pre-crash, where it was `[]`) and the slice is **16 passed, 0 failed**.
- [x] Attribution recorded: this is the slice's timing, not the daemon's behaviour, and not caused
      by T-0093 (whose only code that runs in this slice is the classification tick, which writes
      nothing without a `[notify]` section).

## Notes

- The other slice failure in the same batch run — `handoff-abort`, 2 failures — is **contention**:
  run alone it is 41/41. Its stage kills daemons by SIGKILL at sub-second deadlines and does not
  tolerate a neighbour slice's leaked processes. Recorded in the ledger rather than "fixed".
- No unit test covers this: it is an xtask surface with a real daemon and real harnesses, which is
  exactly why the defect lived as a slice-level race. The slice *is* the test.
