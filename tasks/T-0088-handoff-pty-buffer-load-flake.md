---
id: T-0088
title: The handoff pty-buffer test is load-sensitive — it fails inside a full workspace run, not alone
phase: 2
priority: 3
status: proposed
depends_on: []
scope:
  - crates/arreo-server/tests/handoff.rs
  - .loop/evidence/T-0088/**
verify:
  - cargo test -p arreo-server --test handoff
  - cargo test --workspace
---

## What is observed

`a_child_that_fills_the_terminal_buffer_while_paused_is_not_left_blocked` failed once
inside `cargo test --workspace` (the parallel run of ~40 test binaries) while the same
suite passes **3/3 in isolation** and the next full workspace run was **816 passed /
0 failed**. The test is timing-sensitive by construction — it fills a pty's buffer while
the pump is paused and asserts the child is not left blocked — so a loaded box changes
what it observes.

## Why it is worth a task rather than a shrug

This repository's rule is that a gate which fails intermittently is a gate nobody can
point at. Two precedents are already recorded and fixed: the `handoff-abort` slice's
racing kill point (T-0084) and the relay exchange test's racing pane-count probe. Both
were "a check that samples a fact that is still moving". This is the same class one layer
down, in a unit-style test, and the fix is likely the same shape: **wait for the fact
with a bounded deadline rather than asserting it once**, or assert the property in a way
that does not depend on how fast the box is.

## Acceptance criteria

- [ ] The failure is reproduced deterministically: a loop of `cargo test --workspace`
      runs (or the single test under synthetic load) that shows the failure, with the
      transcript kept under `.loop/evidence/T-0088/`.
- [ ] The assertion is changed so it waits for the fact it is asserting (a bounded
      deadline with a named reason), or the property is restated in a load-independent
      way — **not** by loosening it into something that cannot fail.
- [ ] A mutation check: the weakened-assertion temptation (e.g. removing the deadline so
      it always passes) is shown to redden the test.
- [ ] `cargo test -p arreo-server --test handoff` green, and one full `cargo test
      --workspace` green with the fix in place.

## Notes

- The evidence today is thin on purpose: one failure in ~3 full runs plus 3/3 clean
  isolated runs (recorded in the ledger, turn 79). Reproducing it properly is the first
  criterion, not a formality.
- If reproduction turns out to need a loaded box, say so and pin the load in the test
  itself (a spawn that busy-loops for the duration) so the check is honest about the
  condition it needs.
