---
id: T-0076
title: Modern, efficient, accessible TUI on the brand palette
phase: 2
priority: 1
status: proposed
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

- [ ] Palette truth: `arreo` built-in == BRAND §2 tokens (`bg #121110`, `text #EDE7DC`,
      `primary #6FD3E8`, `sand #D9C6A5`, working cyan, question amber `#E8B45A`,
      blocked orange `#FF9E64`, done sage `#8FD19E`, error `#E5636F`, idle `#6E6A64`)
      at truecolor depth, with the light-variant derivation rule from BRAND; any drift
      fails a unit test that reads both files.
- [ ] State is never color-only: every state has dot shape + text label (`◉ question`,
      etc.); a grayscale/NO_COLOR snapshot stays fully readable (frame captures in
      evidence: truecolor + 256 + 16 + NO_COLOR).
- [ ] Efficient: T-0015's steady-state budgets stay green (deltas only, idle transcript
      < 4 KiB, no clear-screen); 30-pane wall keeps first frame < 300 ms per
      perf-budget.toml; no per-tick full repaints (asserted, not eyeballed).
- [ ] Comfortable: visible focus indicator distinct from selection, stable layout on
      state change (no jump), small-terminal degradation (80×24 usable, narrower
      clamps instead of glitching), keyboard-complete with on-screen key hints,
      blink/pulse off switch (`tui.reduce_motion` + NO_COLOR implies still).
- [ ] Contrast: text/background and state/background pairs meet WCAG AA (4.5:1) at
      truecolor, computed in a test — not a screenshot opinion.
- [ ] Interactive evidence: scripted pty frame captures (overview, question pulsing,
      wall focus, theme picker, degraded depths) under `.loop/evidence/T-0076/`.

## Verification

```console
cargo test --workspace
cargo xtask e2e --slice tui
cargo xtask e2e --slice theme
```
