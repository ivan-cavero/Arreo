# ADR 0002: VT emulation — `alacritty_terminal` + append-only scrollback log

- Status: accepted (2026-09-10, T-0003)
- Context: every client (TUI, phone, CLI) must render the same live grid with
  cursor, colors, and alt-screen awareness; scrollback must survive 1M-line
  replays inside the ≤ 3 MiB per-pane budget (ROADMAP §3.1, §5).
- Decision: `alacritty_terminal` 0.24.2 `Term<VoidListener>` (viewport only,
  `scrolling_history = 0`) fed via `vte::ansi::Processor`, one byte at a time;
  scrollback is an append-only per-pane log file with a lazy 64-line-mark
  offset index; dirty ranges from `Term::damage()`; alt-screen appends pause.
- Why this one:
  - Proven embed pattern (freya-terminal, teksilo, fresh-editor per ROADMAP):
    ECMA-48/CPR/OSC edge cases already handled, maintained upstream.
  - Viewport-only grid + file log separates hot RAM (flat) from history
    (disk): 1M-line replay holds ≤ 12 KiB VT RAM, proven by test.
  - `damage()` gives O(viewport) dirty ranges for the TUI/phone delta sync
    (ROADMAP §3.1 grid diffing) with no extra bookkeeping.
- Alternatives rejected:
  - Hand-rolled ANSI parser: years of escape-sequence edge cases to re-learn;
    every full-screen app (vim/less/htop) is a new bug (rejected: correctness).
  - `Term` with built-in scrollback history: duplicates the log in RAM,
    unbounded under replay (rejected: memory budget).
  - Screen-scraping the T-0002 `RingBuffer`: loses cursor, colors,
    alt-screen — exactly the signals P4 state detection needs (rejected: data).
- Version note: `polling = "=3.7.0"` pinned — polling 3.11 pulls rustix 1.x
  while alacritty's `rustix-openpty` needs rustix 0.38; the mix breaks
  `tty/unix.rs` on rustc 1.98. Revisit on alacritty upgrade (any 0.25+).
- Consequences: `feed` is O(input bytes); `page` seeks the log (fast after
  `spill_to_disk`); over-long partial lines (> 64 KiB) are dropped+counted;
  C0 controls stripped from the log so binary garbage can't corrupt history.
