---
id: T-0016
title: Theming engine — truecolor JSON themes, capability fallback, /theme picker
phase: 1
priority: 4
status: todo
depends_on: [T-0015]
scope:
  - crates/arreo-core/src/theme/**
  - crates/arreo-tui/**
  - themes/**
---

## Goal

opencode-compatible theme JSON (defs + semantic tokens, dark/light, "none" tokens), loaded
from built-in → user → project → cwd; truecolor-first with 256/16 quantized fallback;
`/theme` picker in the TUI.

## Acceptance criteria

- [ ] Theme loader + validation (unknown token = error, not silent).
- [ ] Color-depth detection: truecolor/256/16 + NO_COLOR; quantization tested against
      fixtures of legacy Terminal.app output (no glitched escapes).
- [ ] Built-ins: `arreo`, `tokyonight`, `catppuccin`, `gruvbox`, `system` (minimum).
- [ ] Visual evidence: same theme rendered in iTerm-shaped fixture, Windows Terminal shape,
      and 256-color fallback — screenshots in `.loop/evidence/T-0016/`.
- [ ] The same theme file renders the TUI sidebar *and* a reference HTML (shared tokens).

## Verification

```console
cargo xtask e2e --slice theme
```
