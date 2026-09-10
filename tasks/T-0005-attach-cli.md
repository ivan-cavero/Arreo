---
id: T-0005
title: `arreo attach` — raw CLI client over a Unix socket (Linux/macOS first)
phase: 0
priority: 3
status: todo
depends_on: [T-0003]
scope:
  - crates/arreo-server/**
  - crates/arreo-cli/**
---

## Goal

The daemon exposes a Unix socket; `arreo attach` connects and renders a pane's live grid
to the terminal. First human-loop: prove the client/server split.

## Acceptance criteria

- [ ] Daemon serves: `panes list`, `pane attach <id>`, `pane send <id> <input>`.
- [ ] `arreo attach` renders live deltas (grid diff, not full repaints) in a raw terminal.
- [ ] Detach/reattach: state consistent after reconnection (scrollback intact).
- [ ] Two clients attached simultaneously see the same truth.
- [ ] Manual TUI check performed interactively (real keystrokes, captured frames in
      `.loop/evidence/T-0005/`) — not just unit green.

## Verification

```console
cargo test -p arreo-server
cargo xtask e2e --slice attach
cargo run -p arreo-cli -- attach <pane-id>   # manual, evidence captured
```
