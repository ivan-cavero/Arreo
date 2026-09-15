---
id: T-0116
title: The server can push a theme, so one theme reaches every surface
phase: 3
priority: 2
status: proposed
depends_on: [T-0016, T-0104]
scope:
  - crates/arreo-core/src/theme/**
  - crates/arreo-core/src/proto/message.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-cli/src/main.rs
  - docs/themes.md
  - .loop/evidence/T-0116/**
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice theme
---

## Goal

ROADMAP §3.5: "**themes from the server's JSON** (one theme, every surface)". The theme engine
(T-0016) is real and has a JSON parser for the brand document (`theme/brand.rs`), but themes
reach a client by *discovering files in a directory* (`Loader::discover`) — a filesystem, which
a phone does not have and a browser will not. The server cannot send one.

One sentence: a client can ask the machine for its theme and render it, so the same palette
reaches the TUI, the phone and (later) the browser without a file on any of them.

## Acceptance criteria

- [ ] A socket verb asks for a theme by name (and variant), and the reply carries the theme
      **as the resolved token map** — see the Design note below for why that and not the
      on-disk document.
- [ ] The client applies it: the TUI can render a theme it received rather than only one it
      found on disk, and `arreo theme export <name>` prints exactly what the verb returns, so
      the round trip is inspectable from the CLI.
- [ ] An unknown theme name is a typed refusal naming the name; a machine with no theme
      configured answers with the built-in default rather than an error (the default is a
      working answer, not a failure).
- [ ] **Depth fallback survives the wire** (T-0016's property): a phone on a 16-colour terminal
      and one on truecolor must get the right degradation from the same document, asserted at
      both depths.
- [ ] N−1 per T-0028: the new fields are serde-defaulted, and the compat slice still passes in
      both directions.
- [ ] `docs/themes.md` records the wire shape and the one-document rule.

## Design (decided by the planner — the contract)

**The wire shape is the resolved token map, not the on-disk theme file.** Probed before
dispatching, because the first draft of this criterion named the wrong format:

- There are **two** JSON formats in the theme engine, and they are not the same thing.
  `theme/schema.rs::RawTheme` is the *on-disk theme file*: `$schema`, `defs` (named color
  references), and `theme` (per-token, per-variant values, where a value may be a `defs`
  reference or a literal). `theme/brand.rs` parses the *brand document* (§2's token list),
  which is a third thing again — a design artifact, not a user theme.
- `RawTheme` is `Deserialize` only — there is no serializer — and sending it would push three
  client-side burdens onto every surface: the `defs` reference resolution, the token-name
  validation (`NUMERIC_TOKENS`, unknown-token refusals), and the per-variant `RawValue`
  unwrapping. A phone or a browser would need the resolver, and each surface would be a place
  the resolution could diverge — the opposite of "one theme, every surface".
- The engine already separates those steps: `schema::resolve(name, raw, variant)` produces a
  `BTreeMap<String, Color>`, and `Theme::new(name, variant, depth, colors)` is a theme. **That
  resolved map is what crosses the wire**: `(name, variant, tokens: map<token, color>)`.
- **T-0104's boundary already proves the client half exists**: `theme_from_tokens`,
  `ThemeHandle::tokens`, `ThemeHandle::with_depth`, `color_parse` and `color_quantize` are
  exported, so a client builds the theme from the received tokens and applies its own depth
  locally. That is also what makes the depth-fallback criterion below satisfiable *at the
  client* rather than at the server: the same document degrades differently on a 16-colour
  phone and a truecolor one, which is the property T-0016 built.
- The document is therefore **resolved, not raw**: a server that cannot resolve a theme it
  found on disk refuses with the schema error's own sentence (T-0016's `SchemaError` is already
  typed and specific), rather than shipping a broken document for every client to fail on
  differently.

`arreo theme export <name>` prints exactly what the verb returns, so the round trip is
inspectable from the CLI and the wire shape is testable without a phone.

## Notes

- This is the first thing in Phase 3 that a *browser* also needs (Phase 4's dashboard), so the
  shape is worth getting right once — which is why it is pinned here rather than left to the
  worker.
- If the resolved-map shape turns out to be insufficient for some surface, the finding is
  recorded in the task file rather than a second format being added beside it.
