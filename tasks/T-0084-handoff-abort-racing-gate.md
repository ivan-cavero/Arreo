---
id: T-0084
title: The handoff-abort slice's "After" kill point races the commit — the freeze signal cannot distinguish pre- from post-commit
phase: 2
priority: 3
status: proposed
depends_on: []
scope:
  - xtask/src/handoff_abort_slice.rs
  - .loop/evidence/T-0084/**
verify:
  - cargo xtask e2e --slice handoff-abort
---

## What is broken

`--slice handoff-abort`'s kill point `After` (wait for all 8 pane descriptors at the incoming,
then SIGKILL the incoming, then assert: no `handoff` commit row, the old daemon still serving,
all panes alive and contiguous) fails roughly half the runs with the commit row present, the old
daemon gone and every pane dead. The slice's own narrative says it: "killed 239–283 ms after the
cut began, with 8/8 pane terminals and the manifest arrived ... the panes were still paused and
the outgoing daemon was still serving, so the commit was withheld" — and then the bundle fails.

**The race, traced to the code:** the freeze signal the slice uses to prove "the commit has not
happened" (`attach.newest().elapsed() >= FREEZE_MS`) stays true **after** the commit too — the
outgoing's comment says it: "the pumps stay parked for ever" on commit. So the guard can pass
with the cut already committed (or about to commit in the next few ms). The incoming's commit
byte follows its readiness signal (serve_inherited owns the listener, lock checked) — a 2–10 ms
tail after the last descriptor lands — and the slice's kill lands in the same window: 1 ms
polling + a few µs of checks vs a commit that is 2–10 ms away under quiet load and arbitrarily
delayed under the battery's parallel load. It is a coin flip, and it was a coin flip **before**
either T-0077 or T-0072: reproduced at `a7abcd8` (3/6 full-file failures), no handoff code
changed since.

## Evidence

- `a7abcd8` (pre-T-0077, pre-T-0072): `relay_daemon`/handoff suites green but
  `--slice handoff-abort` fails 3/6 runs with the same two/seven bundle failures.
- `d9a6773` (T-0077 only): 3/3 passes — the process-group spawn shifts the lottery.
- Current (T-0077 + T-0072): ~50% fail (2 failures and 7 failures both observed; one full
  42-check pass captured this turn).
- Failing timelines: `killed 239.6 ms after the cut began` and `283.0 ms` — the freeze detection
  itself waits `FREEZE_MS` (60 ms) after the outgoing parks the pumps (which happens **before**
  the manifest), so the kill cannot be moved earlier by tightening the slice's own spin.

## What "correct" looks like

The abort property the point exists to prove — *a cut interrupted at the latest moment before
the commit leaves the old daemon whole* — needs a signal that can actually distinguish
pre-commit from post-commit. Three candidate directions, in order of preference:

1. **A positive pre-commit signal from the outgoing daemon.** The commit is one marker byte on
   the transfer socket; the slice cannot see that socket. A tiny, honest product hook (e.g. the
   outgoing daemon writing the handoff audit row **before** sending the descriptors — moving the
   "in flight" marker ahead of the transfer so the slice can assert its absence while the cut is
   provably unfinished) would make the point deterministic for every run, not just quiet ones.
2. **Kill point `After` becomes `Everyone-but-the-commit`**: assert the bundle on a kill landed
   the instant 8/8 are detected with 1 ms polling and no pre-kill checks — and accept that the
   point samples "8/8 arrived" rather than "commit not sent" (the bundle then asserts the abort
   *property*, which holds whenever the kill wins the race).
3. **Merge `After` into `Mid`** (kill on freeze + ≥1 descriptor) and document that the
   "all-descriptors-then-kill" sample point is not deterministically reachable with the current
   observable surface.

Prior art: the slice already documents this exact surprise once ("Without this the point can
land *after* the commit — measured, on the first run of this slice"), and point `Mid` already
learned the same lesson for the `.handoff` socket ("requiring it here raced the cut on a healthy
run — measured").

## Notes

- Do **not** fix this by making the slice tolerant of a committed cut (asserting "either the
  old daemon is whole or a commit happened") — that erases the ADR 0021 §2c property the point
  exists to pin.
- Do **not** attribute it to T-0072 in the ledger: the `a7abcd8` reproduction (before either
  change) is in the task's evidence.
- The slice passes 41/41 on a healthy run — the failure is the sampling, not the product
  contract, and the update/chain slices keep the swap covered meanwhile.