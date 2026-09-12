---
id: T-0079
title: The wall takes ~9 s to paint 30 panes — the poller is serial and blocks per pane
status: proposed
priority: 1
depends_on: [T-0015, T-0040]
phase: 2
---

# Goal

A wall of 30 agents paints its first frame in under 300 ms (`perf-budget.toml`'s
`tui_attach_30panes_ms`). Today it does not paint at all within four seconds.

## Why this exists

Found by T-0076's worker while trying to *assert* that budget row, and confirmed by reading
the poller. It is not a rendering problem: the frame the TUI builds is cheap, and T-0076
proved the repaint path paints zero cells while idle. The cost is the **poll loop**.

`crates/arreo-tui/src/main.rs::poll_summaries` iterates panes **serially** and makes four
round-trips per pane:

- `Message::MetricsReq` (RAM),
- `Message::MetricsHistory` (the sparkline series),
- and `state_for`, which issues **up to two blocking `Message::Wait` calls with
  `timeout_ms: 150`** — one for `Question`, one for `Blocked`.

So a 30-pane pass where no pane is in a target state — i.e. **every pane is working**, which
is the normal state of a wall of agents — costs `30 × (150 + 150 + 2 round-trips)` ≈ **9 s**
before the first frame can be correct. Measured by the T-0076 worker: with 30 panes the first
poll pass had not finished after 4 s, with 0/29 ids on screen.

**Why this matters more than a budget row.** The product's headline scenario is watching a
wall of agents. A wall that takes nine seconds to appear, and re-polls every tick with the
same serial cost, is not slow — it is unusable at exactly the scale it exists for. The budget
row was recorded in Phase 0 and marked `phase0 = false` (recorded, not yet enforced), so
nothing failed and nothing warned: the row was aspirational until this worker tried to enforce
it. **That is the finding underneath the finding**: a recorded-but-unenforced budget is a
budget nobody is checking, and the first person to check it found a nine-second hole.

## Scope fence

`crates/arreo-tui/src/main.rs` (the poll loop), `crates/arreo-tui/src/client.rs` if the
session needs a batched call, and — if the fix needs a new socket verb — `crates/arreo-core/src/proto/message.rs`
plus the daemon's handler in `crates/arreo-server/src/daemon.rs`. `xtask/src/tui_slice.rs` for
the enforcement, and `perf-budget.toml` to flip the row to enforced once it passes.

## Acceptance criteria

- [ ] The 30-pane first frame is measured in the `tui` slice and asserted against
      `perf-budget.toml`'s `tui_attach_30panes_ms` **read from the file, not copied** — and the
      row is flipped to enforced (`phase0 = true`) in the same change.
- [ ] The measured first frame is under the row's 300 ms with 30 panes each emitting a marker
      stream (the shape that makes every `Wait` time out — the case that fails today).
- [ ] The fix is named for what it is: the poll pass must not serialize per-pane blocking waits.
      The acceptable shapes are (a) one batched request that returns every pane's state, RAM and
      series in a single round-trip, or (b) concurrent per-pane requests with the pass's total
      time bounded independently of pane count. A poll that is still `O(panes)` round-trips must
      justify why that is not `O(panes)` **time**.
- [ ] **The steady-state cost does not regress**: T-0015's idle-delta budget (< 4 KiB, no
      clear-screen) stays green, and the counting-backend test that asserts zero cells repaint
      while idle stays green. A fix that makes the first frame fast by polling everything every
      tick is not a fix.
- [ ] The `Wait`-based state resolution is reconsidered rather than merely parallelised: a
      blocking wait with a timeout is how the loop *asks* a question it could be *told* the
      answer to. State whether a push/event shape is the right answer, and if it is out of scope,
      say what the cheap version is and what it costs.
- [ ] A test with **one** pane proves nothing here; the test must have enough panes that a serial
      pass cannot pass it (state the number and why).

## Verification

```console
cargo xtask e2e --slice tui        # the new assertion, reading perf-budget.toml
cargo xtask bench                  # the neighbouring rows must not regress
```

## Findings

- **The cost is per-pane blocking waits, not rendering.** `state_for` asks `Wait` with a 150 ms
  timeout for each candidate state, serially, per pane; a working pane is in neither target
  state, so it pays the full timeout twice. Nine seconds is arithmetic, not a flake.
- **A recorded budget that nothing enforces is a claim, not a law.** This row has been in
  `perf-budget.toml` since Phase 0 with `phase0 = false`; the first attempt to enforce it found
  the breach. Worth asking which other `phase0 = false` rows are in the same position.
- **The TUI worker removed the check rather than leaving it red, and said so** — the right call
  for a red slice, and the reason this task exists rather than a silent gap. The wrong part was
  its report's claim that "`cargo xtask bench` still owns the row": bench does not measure it, so
  before this task nothing did. Corrected in the ledger.
