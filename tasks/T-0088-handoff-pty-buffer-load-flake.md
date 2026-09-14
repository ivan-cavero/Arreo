---
id: T-0088
title: The handoff pty-buffer failure was a real defect — a poll mid-line split one written line into two
phase: 2
priority: 3
status: done
depends_on: []
scope:
  - crates/arreo-core/src/pty.rs
  - crates/arreo-core/tests/pty.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-server/tests/api.rs
  - crates/arreo-server/tests/handoff.rs
  - xtask/src/chaos/giant_line.rs
  - .loop/evidence/T-0088/**
verify:
  - cargo test -p arreo-server --test handoff
  - cargo test --workspace
---

## What was observed

`a_child_that_fills_the_terminal_buffer_while_paused_is_not_left_blocked` failed once
inside `cargo test --workspace` (the parallel run of ~40 test binaries) while the same
suite passed **3/3 in isolation** and the next full workspace run was **816 passed /
0 failed**. It was filed as a load-sensitive flake.

## What it actually was

**Not load. A real defect in the ring buffer, caught by a test that counts exact lines.**

`Pane::drain()` called `RingBuffer::flush_partial()`, which *took* the unterminated
trailing line out of `pending` and pushed it into the lines ring. Any poll landing
between two writes of one line therefore split that line permanently:

```
program writes "burst-2|aaaa…"   ring: [ ..., "burst-2|aaaa…" ]   pending: ""
program writes "bbbb…\n"         ring: [ ..., "burst-2|aaaa…", "bbbb…" ]
```

One written line, two ring entries — for ever, for **every** reader: `arreo read`,
`attach` (the ring is the source of its deltas), the TUI's pane view, the handoff
manifest, and the persisted snapshot. The test caught it because it counts exact lines
(480) and got 479 + 1 fragment.

Reproduction needed the *parallel test-binary* shape of a workspace run, not synthetic
CPU load (6 busy loops × 6 runs were all green; 3 concurrent `--test handoff` plus
`--test relay_daemon` gave 1 failure in 24 pre-fix, 0 in 24 post-fix). See
`.loop/evidence/T-0088/ring-partial-split.txt`.

## The fix

- `RingBuffer::lines_with_partial()` — the reader's view: the terminated lines plus the
  unterminated trailing line as a trailing element. Non-mutating. The partial has to be
  *visible* (a harness's question without a newline is the product's flagship signal)
  but it must never *enter* the ring.
- `Pane::drain()` uses it; `RingBuffer::flush_partial` is deleted (with `drain` no longer
  mutating it had no production user, and the two must not disagree about what a line is).
- `Pane::ring_len()` — the count of **terminated** lines, the number a line-indexed
  reader may trust.
- `stream_attach`'s cursor advances only past terminated lines when the tail is a partial,
  so the completion is re-fetched at its own index and arrives whole instead of being
  skipped as already delivered.

## Acceptance criteria

- [x] The failure is reproduced: 3 concurrent `--test handoff` + 1 `--test relay_daemon`,
      8 rounds — 1 failure in 24 pre-fix ("479 of 480", the exact original symptom), 0 in
      24 post-fix. Transcript under `.loop/evidence/T-0088/`.
- [x] The property is restated so it does not depend on how fast the box is: the partial
      is visible to a reader and never materialised, and a poll between two writes of one
      line does not split it (`crates/arreo-core/tests/pty.rs`), plus the same sequence
      through a real daemon and a streaming client (`crates/arreo-server/tests/api.rs`).
      The old test that pinned the materialisation (`unterminated_prompt_line_is_visible_
      after_flush`) asserted the defect and was replaced, not re-pinned.
- [x] Mutation check: `ring_len` counting the partial → `a_line_written_in_two_pieces_
      arrives_once_and_whole` FAILS; `drain` materialising the partial → same test FAILS;
      the full pre-fix behaviour restored → the handoff pty-buffer test fails again with
      "479 of 480".
- [x] `cargo test -p arreo-server --test handoff` green (40 passed) and `cargo test
      --workspace` green with the fix in place.

## Notes

- The task's own framing ("load-sensitive flake") was wrong; the file now says what it
  actually was. The lesson is the one T-0084 recorded one layer up: a check that counts a
  fact the product is still mutating will eventually catch the product, not the load.
- The partial is not lost and the ring is intact — both asserted, not argued.
