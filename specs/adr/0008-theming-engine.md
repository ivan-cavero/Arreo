# ADR 0008: theming — themes are data in core, quantized at read time

- Status: accepted (2026-09-11, T-0016)
- Context: ROADMAP §3.12 promises opencode-compatible JSON themes, truecolor-first
  output with a 256/16 fallback, a `system` theme that blends with the terminal,
  and "one theme, every surface". T-0015 shipped a TUI whose every color already
  went through a single `Theme` seam, deliberately minimal so this task could
  replace the loader without touching widget code. Three ways to build it:
  colors baked per-widget, colors resolved per-render, or colors resolved once
  into a table the surfaces read.
- Decision: the engine lives in `arreo-core::theme` (color model + capability
  detection, schema + validation, loader + hierarchy), and the TUI owns only the
  ratatui adapter (`crates/arreo-tui/src/theme.rs`). A theme file is
  `defs` + semantic `theme` tokens in the opencode shape; tokens may be a bare
  value or `{dark, light}`; `"none"` means "inherit the terminal". Resolution
  happens **once per (theme, variant, depth)** into a `BTreeMap<String, Color>`;
  `Theme::color(token)` quantizes on read, so a depth change is a re-quantize,
  not a reload. Depth is detected from `NO_COLOR`/`TERM`/`COLORTERM` and is
  overridable (`--depth`, `--variant`, `--theme`). Indexed colors always go out
  as `38;5;N` (crossterm's form, and what indexed terminals parse); the depth
  decides the *index range* — `N ≥ 16` at 256 colors, `N ≤ 15` at 16 colors —
  and `NoColor` produces no SGR at all. Built-ins are `include_str!`-embedded
  (`arreo`, `tokyonight`, `catppuccin`, `gruvbox`, `system`); disk themes load
  from `ARREO_THEME_DIR` → `$XDG_CONFIG_HOME/arreo/themes` →
  `<project>/.arreo/themes` → `./.arreo/themes`, later wins, and a partial theme
  inherits the missing tokens from `arreo`. Unknown tokens, dangling `defs`
  references and malformed colors are load errors that name the file, the token
  and (for typos) the nearest valid token. The reference HTML is generated from
  the same resolved table (`reference_html`), and the e2e slice compares the
  colors in that HTML with the SGR bytes the TUI actually emitted.
- Why this one:
  - **Core, not the TUI.** §3.12's "one theme, every surface" is the requirement;
    colors that only the TUI can reach would have to be re-implemented for the
    mobile clients. Core also already owns every other piece of shared state, and
    a theme has no I/O beyond reading its own files.
  - **Resolve once, quantize on read.** Quantizing per render would put a
    nearest-color search in the frame loop, and a 16-color terminal would pay it
    for every cell. Caching the table and quantizing at the accessor keeps the
    loop free of palette math while keeping depth a pure function of the theme.
  - **Errors, not defaults.** A silently-dropped typo means a user edits a theme,
    sees "nothing happened", and has no path forward. The loader fails loudly
    with the token name and gives the nearest match.
  - **The HTML is generated from the same table.** The acceptance criterion is
    "the same theme renders the sidebar *and* a reference HTML (shared tokens)";
    generating both from one resolved map makes drift structurally impossible
    rather than a review burden.
- Alternatives rejected:
  - **`ratatui::style::Color` as the internal type**: drags a render-loop library
    into the data model, and cannot express `"none"` (inherit) or an unquantized
    RGB value — rejected; the adapter maps at the boundary.
  - **Truecolor escapes always, let the terminal downgrade**: terminals do not
    downgrade; they print garbage or ignore the sequence (the exact "glitched
    output" §3.12 forbids). Rejected.
  - **Per-widget color constants (T-0015's shape, extended)**: every new widget
    becomes a place to forget a theme; rejected when T-0016 landed the table.
  - **Loading themes from a single fixed directory**: §3.12 specifies the
    user → project → cwd ladder (a repo can pin its own look); rejected.
- Consequences: `crates/arreo-tui/src/theme.rs` is the only file that knows
  ratatui exists; `ThemeState` also owns the catalog, so the picker lists what is
  installed. The `system` theme emits only ANSI indices and `none` backgrounds —
  it is the proof that the indexed path is not a degraded afterthought. New
  surfaces (mobile, web) consume `Theme::color`/`state_color` directly. Adding a
  token means adding it to `COLOR_TOKENS` and to each built-in that wants a
  non-default value; the loader, the picker, the HTML and the slice all pick it
  up with no further wiring. `cargo xtask e2e --slice theme` is the gate: 27
  assertions over the engine, three real terminal shapes on a pty, and the
  shared-token HTML.
