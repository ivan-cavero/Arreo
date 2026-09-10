---
id: T-0016
title: Theming engine — truecolor JSON themes, capability fallback, /theme picker
phase: 1
priority: 4
status: done
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

- [x] Theme loader + validation (unknown token = error, not silent).
      `arreo_core::theme::{schema,loader}`: `defs` + semantic `theme` tokens in the
      opencode shape (bare value or `{dark, light}`, `"none"` = terminal default).
      Unknown token → `UnknownToken` with the nearest valid name; dangling def →
      `UnknownDef` naming the missing def; bad value → `BadColor` with the token and
      file. Hierarchy: embedded → `ARREO_THEME_DIR` → `$XDG_CONFIG_HOME/arreo/themes`
      → `<project>/.arreo/themes` → `./.arreo/themes`, later wins, partial themes
      inherit the rest of `arreo`. Unit tests in `theme::schema`/`theme::loader`.
- [x] Color-depth detection: truecolor/256/16 + NO_COLOR; quantization tested against
      fixtures of legacy Terminal.app output (no glitched escapes).
      `Depth::detect_from` over an injectable env, covering COLORTERM=truecolor/24bit,
      `-direct`, 256, plain xterm, unset TERM (conhost), `dumb`, and NO_COLOR-beats-all.
      `Color::fg_sequence` is the contract: `38;2;R;G;B` only at truecolor, `38;5;N`
      with `N ≥ 16` at 256, `N ≤ 15` at 16 colors, and *no bytes at all* under
      NO_COLOR. The slice asserts those bytes against three real terminal shapes plus
      a NO_COLOR shape driven on a pty (`.loop/evidence/T-0016/*.raw`).
- [x] Built-ins: `arreo`, `tokyonight`, `catppuccin`, `gruvbox`, `system` (minimum).
      Embedded via `include_str!`; `Catalog::builtin_names()` is asserted to contain all
      five in both variants at every depth. `system` emits only ANSI indices + `none`
      backgrounds (the blend-with-your-terminal theme).
- [x] Visual evidence: same theme rendered in iTerm-shaped fixture, Windows Terminal shape,
      and 256-color fallback — screenshots in `.loop/evidence/T-0016/`.
      Frames per shape (`iterm-truecolor`, `windows-terminal-256`, `legacy-16color`,
      `no-color`) plus the reference-HTML screenshots
      (`reference-*.webp`, rendered from the generated HTML) and the raw escape
      transcripts (`*.raw`) the byte-exact assertions run against.
- [x] The same theme file renders the TUI sidebar *and* a reference HTML (shared tokens).
      `theme::reference_html` is generated from the resolved token table the TUI reads,
      and `xtask e2e --slice theme` cross-checks the HTML's `data-color` labels against
      the SGR bytes the TUI actually emitted for the visible state token.

## Notes

- Themes are data in `arreo-core`, not TUI objects (ADR 0008): §3.12's "one theme, every
  surface" needs the same table for mobile later. `crates/arreo-tui/src/theme.rs` is the
  only file that knows ratatui exists.
- Depth is a property of the *terminal*, not the theme file: resolution is cached per
  (theme, variant, depth) and `Theme::color` quantizes on read.
- The TUI takes `--theme`, `--variant` (dark/light) and `--depth`
  (truecolor|256|16|none) so a user (and the slice) can pin what detection found.
- `/theme` in the search prompt and `t` both open the picker; a theme that fails to load
  reports its error *in* the picker and leaves the current theme untouched.

## Verification

```console
cargo xtask e2e --slice theme
cargo xtask e2e --slice theme --interactive-evidence
```

Last run: 27 passed, 0 failed (2026-09-11).
