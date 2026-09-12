---
id: T-0074
title: TUI manages everything — agents and servers without leaving the wall
phase: 2
priority: 2
status: proposed
depends_on: [T-0015, T-0044, T-0045, T-0059]
scope:
  - crates/arreo-tui/src/**
  - docs/tour.md
  - docs/machines.md
  - xtask/src/tui_slice.rs
  - xtask/src/mesh_slice.rs
  - xtask/src/main.rs
  - .loop/evidence/T-0074/**
verify:
  - cargo test --workspace
  - cargo clippy --workspace --all-targets -- -D warnings
  - cargo fmt --all -- --check
  - cargo xtask e2e --slice tui
---

## Goal

The TUI today is read + attach + search (T-0015/T-0061). It must manage the fleet:
agents (spawn/kill/send, local and `--machine`) and servers (list/add/rename/remove,
trust) — reusing the `arreo-core::mesh` resolve/session the CLI already uses (the TUI
may not depend on the CLI binary crate). No new protocol verbs: the socket API already
has them (T-0014); this is client wiring + keys + confirmations.

## Acceptance criteria

- [ ] Agents from the sidebar: spawn (program+args prompt), kill (confirm naming the
      pane), send-to-pane (prompt, audited as the device), working locally and against
      `--machine <name>` with the CLI's exit-code honesty mapped to one-line messages.
- [ ] Servers from the TUI: machines list with presence, add (invite code flow, T-0058),
      rename/remove (T-0057 semantics incl. `--force` for online), trust grant/revoke
      (T-0059 semantics incl. fingerprint confirm) — same refusals the CLI prints,
      no silent divergences.
- [ ] Safety: destructive keys confirm (`kill`, `remove`, `revoke`); viewer role renders
      actions disabled with the reason instead of failing after the keypress.
- [ ] Keys documented in `--help`/tour; every key has a keyboard equivalent (mouse-first
      stays, keyboard-complete per T-0015).
- [ ] Interactive evidence: scripted pty drives the real `arreo-tui` binary — spawn →
      list → send → kill locally plus machines list by name — with frame captures;
      unit tests alone do not close this task.

## Verification

```console
cargo test --workspace
cargo xtask e2e --slice tui
```
