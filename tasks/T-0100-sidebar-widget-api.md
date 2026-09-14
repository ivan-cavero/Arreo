---
id: T-0100
title: Sidebar widgets — a declarative render API with a tick budget
phase: 4
priority: 4
status: proposed
depends_on: [T-0098, T-0015]
scope:
  - crates/arreo-plugin-api/**
  - crates/arreo-tui/src/**
  - docs/plugins.md
  - xtask/src/tui_slice.rs
  - .loop/evidence/T-0100/**
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice tui
---

## Goal

ROADMAP §3.15's first UI contribution point: "sidebar widgets — small declarative components
under/over the agent list (e.g. a token-spend meter, a CI status strip)".

One sentence: a plugin contributes **data** for a widget and the core renders it, so a plugin
can never corrupt the render loop or a PTY.

## Acceptance criteria

- [ ] The widget interface is declarative: a plugin returns a bounded tree of text/cells (with
      theme **tokens**, not raw colors), and the TUI renders it. No plugin code runs in the
      render path — the tree is built off-thread and the renderer consumes a snapshot.
- [ ] Placement is declared in the manifest (above/below the agent list) and the user can hide a
      widget without uninstalling the plugin.
- [ ] The tick budget is enforced: a widget that exceeds it is throttled, rendered with its last
      good frame, and flagged in `arreo plugins doctor` — the UI keeps drawing.
- [ ] A widget returning malformed data (wrong arity, oversized string, unknown token) is
      refused and replaced with a one-line "widget failed" cell; the TUI never panics and never
      renders a corrupt frame. Each malformation is a test.
- [ ] A real `hello widget` component in the repo renders in the tui slice with a frame capture
      under `.loop/evidence/T-0100/` — the Phase 4 exit criterion's own words.
- [ ] Theme tokens: a widget may define extra tokens and a theme may style them; an unknown
      token falls back to a documented default rather than rendering nothing.
