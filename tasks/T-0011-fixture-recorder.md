---
id: T-0011
title: Fixture recorder — capture real agent sessions into deterministic .pty replays
phase: 0
priority: 2
status: done
depends_on: [T-0002]
scope:
  - crates/arreo-cli/**
  - crates/arreo-core/src/fixtures/**
  - crates/arreo-core/src/pty.rs
  - crates/arreo-core/tests/pty.rs
  - crates/arreo-core/tests/fixtures.rs
  - fixtures/**
---

## Scope note (re-scoped by loop, turn 3)

`crates/arreo-core/src/pty.rs` was not in the original fence, but byte-exact
recording is impossible without it: `Pane::drain()` returns decoded line text
(escapes normalized away), so a vim fixture recorded through it would be lossy.
The addition is minimal and additive — a capped 1 MiB raw journal on
`RingBuffer` + `raw_snapshot()` on `Pane`, no behavior change to existing
methods (all 9 T-0002 tests still green). Reason written here, not silent.

## Goal

`arreo record <command> -o fixtures/foo.pty`: run a command in a PTY, capture raw byte
stream + timing, store as a replayable fixture. Every state-detection claim (T-0004) is
backed by a real recorded session, replayed deterministically forever.

## Acceptance criteria

- [x] Record tool: byte-accurate capture with timestamps; replay feeds bytes at recorded
      pacing (or accelerated for tests).
- [x] Recorded sessions (scripted equivalents — no agent-CLI auth available in this
      environment; scenarios cover the same VT shapes T-0004 consumes): permission-style
      prompt (`question-permission.pty`, `[y/n]`), streaming tool output
      (`working-stream.pty`), idle shell (`idle-shell.pty`), real vim session with
      escapes (`vim-edit.pty`, driven through a real PTY with keystrokes), multibyte
      locale (`locale-utf8.pty`, `日本語 ✓`). Native Claude Code/Codex/Pi adapter
      fixtures land in T-0017 with harness access.
- [x] Fixtures are small (all < 4 KiB; timestamps rounded to 10 ms), committed, and
      CI-stable (deterministic-replay gate test).
- [x] Privacy check: recorder flags secret-shaped content before committing a fixture
      (`scan_secrets`; `arreo record` refuses without `--allow-secrets`).

## Verification

```console
cargo xtask e2e --slice fixtures
```
