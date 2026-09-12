---
id: T-0073
title: TUI exit may shut the daemon down — opt-in, default off, graceful only
phase: 2
priority: 3
status: proposed
depends_on: [T-0012, T-0015]
scope:
  - crates/arreo-tui/src/**
  - crates/arreo-cli/src/main.rs
  - docs/tour.md
  - xtask/src/tui_slice.rs
  - xtask/src/main.rs
  - .loop/evidence/T-0073/**
verify:
  - cargo test --workspace
  - cargo clippy --workspace --all-targets -- -D warnings
  - cargo fmt --all -- --check
  - cargo xtask e2e --slice tui
---

## Goal

The TUI is a client, not an owner (T-0015) — quitting it must leave the daemon alone
**by default**. Add an explicit opt-in so closing the TUI can also drain-stop a *local*
daemon: `arreo-tui --shutdown-on-exit` (+ `tui.exit_kills_daemon` config key, flag wins).

## Acceptance criteria

- [ ] Default unchanged: `q`/`Esc` quits the TUI, daemon + panes untouched (existing tui
      slice keeps passing unmodified).
- [ ] Opt-in path sends the graceful drain-stop (T-0012 `server stop` semantics: flush
      rings + checkpoint, exit 0) — never `kill -9`, never unlink-while-serving.
- [ ] Guards, all loud: refuses on `--machine`/remote targets (local socket only);
      with live panes, attached other clients, or remote sessions it confirms
      interactively (`yes/no`, `--yes` for scripts) naming what would die.
- [ ] Config + flag: `tui.exit_kills_daemon = false` default; `--shutdown-on-exit` /
      `--no-shutdown-on-exit` override for the run; `--help` documents both.
- [ ] Proven on a pty: (a) default quit leaves daemon serving, (b) flag quit with no
      panes stops it cleanly, (c) flag quit with a live pane prompts and aborts on `n`.

## Verification

```console
cargo test --workspace
cargo xtask e2e --slice tui
```
