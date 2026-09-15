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
      **as JSON in the same shape `brand.rs` already parses** — one document format, so the
      server, the TUI and the phone agree by construction rather than by three renderers.
- [ ] The client applies it: the TUI can render a theme it received rather than only one it
      found on disk, and `arreo theme export <name>` prints the JSON the server would send, so
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

## Notes

- This is the first thing in Phase 3 that a *browser* also needs (Phase 4's dashboard), so the
  shape is worth getting right once.
