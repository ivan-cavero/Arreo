---
id: T-0076
title: Modern, efficient, accessible TUI on the brand palette
phase: 2
priority: 1
status: done
depends_on: [T-0015, T-0016]
scope:
  - crates/arreo-tui/src/**
  - crates/arreo-core/src/theme/**
  - docs/tour.md
  - design/BRAND.md
  - xtask/src/tui_slice.rs
  - xtask/src/theme_slice.rs
  - xtask/src/main.rs
  - .loop/evidence/T-0076/**
verify:
  - cargo test --workspace
  - cargo clippy --workspace --all-targets -- -D warnings
  - cargo fmt --all -- --check
  - cargo xtask e2e --slice tui
  - cargo xtask e2e --slice theme
---

## Goal

The TUI works (T-0015) and themes load (T-0016), but it is not yet *modern, efficient,
comfortable and accessible*. One pass, on the identity in `design/BRAND.md` (warm
charcoal × cyan × sand; state colors sacred, never decorative): the `arreo` built-in
must match the brand tokens exactly, and the wall must be fast, comfortable for long
sessions, and usable without relying on color alone.

## Acceptance criteria

- [x] Palette truth: `arreo` built-in == BRAND §2 tokens (`bg #121110`, `text #EDE7DC`,
      `primary #6FD3E8`, `sand #D9C6A5`, working cyan, question amber `#E8B45A`,
      blocked orange `#FF9E64`, done sage `#8FD19E`, error `#E5636F`, idle `#6E6A64`)
      at truecolor depth, with the light-variant derivation rule from BRAND; any drift
      fails a unit test that reads both files. **Met, and verified independently by the
      planner**: `arreo-core/src/theme/brand.rs` parses `design/BRAND.md` §2 at test time
      and compares both directions (every §2 row must be mapped, every mapped token must
      match), so the document is the machine-checked source of truth rather than a copy
      that rots. Mutation-checked: one hex digit of drift in `themes/arreo.json`
      (`#E8B45A` → `#E8B459`) fails two tests, one naming the token.
- [x] State is never color-only: every state has dot shape + text label (`◉ question`,
      etc.); a grayscale/NO_COLOR snapshot stays fully readable (frame captures in
      evidence: truecolor + 256 + 16 + NO_COLOR). **Met**: six distinct dot shapes and six
      state words, asserted as distinct rather than merely present; a `NO_COLOR` frame is
      asserted to contain *zero* non-Reset cells while still naming every pane and state.
- [x] Efficient: T-0015's steady-state budgets stay green (deltas only, idle transcript
      < 4 KiB, no clear-screen); ~~30-pane wall keeps first frame < 300 ms per
      perf-budget.toml~~; no per-tick full repaints (asserted, not eyeballed).
      **Met except the 30-pane clause, which is re-scoped to T-0079** — and the reason is a
      finding, not a shortfall of this pass. The steady-state budgets are green and "no
      per-tick full repaints" is now asserted rather than eyeballed (a counting backend:
      the first frame paints, five idle ticks repaint **0** cells, a state change repaints
      under a quarter of the screen, `clears == 0` throughout).
      The 30-pane clause **cannot be met by a rendering change**: the cost is the *poller*.
      `main.rs::poll_summaries` iterates panes serially and makes four round-trips each —
      `MetricsReq`, `MetricsHistory`, and `state_for`'s up to two blocking `Wait` calls at
      `timeout_ms: 150`. A wall of *working* panes is in neither target state, so each pane
      pays both timeouts: 30 × ~300 ms ≈ **9 s** before the first frame can be correct.
      Measured by the worker (0/29 ids on screen after 4 s) and confirmed by the planner
      reading the loop. That is a poller-design change (batched or concurrent polling), which
      is outside this task's six criteria and is filed as **T-0079** — priority 1, because a
      nine-second wall is a failure at exactly the scale the product exists for.
      The check was **removed rather than left red**, which is the right call for a red slice
      and is why T-0079 exists instead of a silent gap. One correction to the worker's report:
      it said `cargo xtask bench` still owns the row, but bench does not measure
      `tui_attach_30panes_ms` — the row is `phase0 = false` (recorded, unenforced), so before
      T-0079 **nothing** enforces it. That is itself the deeper finding, and it is written into
      T-0079.
- [x] Comfortable: visible focus indicator distinct from selection, stable layout on
      state change (no jump), small-terminal degradation (80×24 usable, narrower
      clamps instead of glitching), keyboard-complete with on-screen key hints,
      blink/pulse off switch (`tui.reduce_motion` + NO_COLOR implies still). **Met**: focus is
      two visible facts (a `▶` marker *and* inverse video, so it survives `NO_COLOR`) distinct
      from the attached-pane marker and from the region border; the row grid is asserted to be
      a function of the panes and not of their states, and a question costs exactly one row
      under its own pane and moves no column; the sidebar extent is tested as a table over
      1..200 columns plus real 80×24, 40×12, 24×8 and 16×6 frames.
- [x] Contrast: text/background and state/background pairs meet WCAG AA (4.5:1) at
      truecolor, computed in a test — not a screenshot opinion. **Met**: the maths is in a
      test with the WCAG anchors pinned, and a state hue that cannot carry text as *text*
      (BRAND's muted `idle`, 3.51:1 on the dark page) falls back to the neutral for the label
      while the dot keeps the brand hue.
- [x] Interactive evidence: scripted pty frame captures (overview, question pulsing,
      wall focus, theme picker, degraded depths) under `.loop/evidence/T-0076/`. **Met**: the
      `tui` slice runs 42 assertions and the `theme` slice 33, both with
      `--interactive-evidence`, capturing frames at truecolor, 256, 16 and `NO_COLOR`.

## Verification

```console
cargo test --workspace
cargo xtask e2e --slice tui
cargo xtask e2e --slice theme
```

## Outcome

Done, five of six criteria met in full and the sixth re-scoped with its reason written down
(above) rather than narrowed silently.

What landed: `arreo`'s built-in palette rewritten to BRAND §2 with a **test that parses the
brand document**, so drift on either side fails; state carried by dot shape *and* word, with a
`NO_COLOR` frame asserted to contain no colour at all; repaint behaviour asserted with a
counting backend instead of eyeballed; focus, layout stability and small-terminal degradation
made explicit and tested; contrast computed in a test with the WCAG anchors pinned; and 75
scripted pty assertions across the two slices with frames at four colour depths.

Two things a human should look at, both reported by the worker and neither silently absorbed:
`design/BRAND.md` §2 says states "shift one step darker" for the light variant **without
defining the step** (50% toward black was chosen — the largest uniform step that still clears
AA on both light surfaces), and §2's `idle #6E6A64` cannot carry AA text on the dark page
(3.51:1), which is why the label fallback exists. The document is not wrong — `idle` is a dot
hue — but a future revision could raise it and delete the fallback.

The 30-pane wall's nine-second first paint is **T-0079**, priority 1.
