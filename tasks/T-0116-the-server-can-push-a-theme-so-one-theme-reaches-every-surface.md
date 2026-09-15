---
id: T-0116
title: The server can push a theme, so one theme reaches every surface
phase: 3
priority: 2
status: done
depends_on: [T-0016, T-0104]
scope:
  - crates/arreo-core/src/theme/**
  - crates/arreo-core/src/proto/message.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-cli/src/main.rs
  - docs/themes.md
  - .loop/evidence/T-0116/**
evidence:
  - .loop/evidence/T-0116/theme-push.txt
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

- [x] A socket verb asks for a theme by name (and variant), and the reply carries the theme
      **as the resolved token map** — see the Design note below for why that and not the
      on-disk document.
- [x] The client applies it: the TUI can render a theme it received rather than only one it
      found on disk, and `arreo theme export <name>` prints exactly what the verb returns, so
      the round trip is inspectable from the CLI.
- [x] An unknown theme name is a typed refusal naming the name; a machine with no theme
      configured answers with the built-in default rather than an error (the default is a
      working answer, not a failure).
- [x] **Depth fallback survives the wire** (T-0016's property): a phone on a 16-colour terminal
      and one on truecolor must get the right degradation from the same document, asserted at
      both depths.
- [x] N−1 per T-0028: the new fields are serde-defaulted, and the compat slice still passes in
      both directions.
- [x] `docs/themes.md` records the wire shape and the one-document rule.

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

## Outcome

Done. `Message::Theme { v, name, variant }` → `Message::ThemeReply { v, theme: ThemeTokens }`,
where `ThemeTokens { name, variant, tokens }` carries **every value a literal color in the
theme file's own spelling** — resolved, never a `defs` reference, never the on-disk document.
The Design section above pinned that shape before dispatch, and the worker's third mutation is
the proof it matters: shipping the document's own values reddens three server tests and both
TUI received-theme checks, because **the client has no resolver** — which is the whole point.

An unknown name is a typed refusal naming it and listing the built-ins; an unconfigured machine
answers with the built-in default; the verb gates as `Verb::Read` and writes no audit row (reads
are deliberately unaudited); `size_of::<Message>() == 112` still holds. The TUI renders a theme
it *received* (fetching on a fresh connection, applying in the UI loop), `arreo theme export`
prints exactly what the verb returns, and the slice grew from 33 to 43 checks with a
pushed-theme fixture at both depths. Three mutations, all RED, none left green.

**Integration the planner did:** the FFI's `WireMessage` mirrors `Message` variant for variant
with an exhaustive match, so T-0116's two new variants were a compile error in
`crates/arreo-core-ffi/src/codec.rs`. That tripwire is a feature — it forces a decision rather
than letting the mirror fall behind — so it was honoured: `WireThemeTokens`, both variants,
both match arms, and a round-trip test in `tests/theme_mirror.rs` (the exhaustive match catches
a *missing* variant, not a *wrong* mapping; a mutation that drops the variant reddens it). Also
three clippy findings the workers were correctly told not to chase.

Full report, the wire shape, the three mutations and the pre-fix red: `.loop/evidence/T-0116/`.
