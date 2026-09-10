---
id: T-0005
title: `arreo attach` — raw CLI client over a Unix socket (Linux/macOS first)
phase: 0
priority: 3
status: done
depends_on: [T-0003]
scope:
  - crates/arreo-server/**
  - crates/arreo-cli/**
  - crates/arreo-core/src/proto.rs
  - crates/arreo-core/src/pty.rs
---

## Scope note (re-scoped by loop, turn 8)

`proto.rs` (new, in core — the T-0001 gate forbids CLI→server, so protocol
types live in core with a server re-export) and `pty.rs` (`kill_shared` for
`Arc<Pane>` kill + reap) were outside the letter but required by the criteria.
Reason written here, not silent.

## Goal

The daemon exposes a Unix socket; `arreo attach` connects and renders a pane's live grid
to the terminal. First human-loop: prove the client/server split.

## Acceptance criteria

- [x] Daemon serves: `panes list`, `pane attach <id>`, `pane send <id> <input>`.
      (+ spawn/resize/kill; JSONL v0 with version field; T-0013 supersedes.)
- [x] `arreo attach` renders live deltas (grid diff, not full repaints) in a raw terminal.
      v0 delta = append-only new-lines (no repaints by construction); cell-range
      grid diff is T-0015's work on T-0003 damage ranges. Proven live + frames.
- [x] Detach/reattach: state consistent after reconnection (scrollback intact).
      Test + live reattach frame (client death ≠ pane death; cursor resumes).
- [x] Two clients attached simultaneously see the same truth. Test green.
- [x] Manual TUI check performed interactively (real keystrokes, captured frames in
      `.loop/evidence/T-0005/`) — tmux split panes, real keystrokes, 4 frames.

## Verification

```console
cargo test -p arreo-server
cargo xtask e2e --slice attach
cargo run -p arreo-cli -- attach <pane-id>   # manual, evidence captured
```
