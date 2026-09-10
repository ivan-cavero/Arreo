---
id: T-0011
title: Fixture recorder — capture real agent sessions into deterministic .pty replays
phase: 0
priority: 2
status: todo
depends_on: [T-0002]
scope:
  - crates/arreo-cli/**
  - crates/arreo-core/src/fixtures/**
  - fixtures/**
---

## Goal

`arreo record <command> -o fixtures/foo.pty`: run a command in a PTY, capture raw byte
stream + timing, store as a replayable fixture. Every state-detection claim (T-0004) is
backed by a real recorded session, replayed deterministically forever.

## Acceptance criteria

- [ ] Record tool: byte-accurate capture with timestamps; replay feeds bytes at recorded
      pacing (or accelerated for tests).
- [ ] Recorded sessions for: Claude Code permission prompt (question), streaming tool
      output (working), idle shell, vim (complex VT abuse), one non-English locale.
- [ ] Fixtures are small (strip/round timestamps), committed, and CI-stable.
- [ ] Privacy check: recorder flags secret-shaped content before committing a fixture.

## Verification

```console
cargo xtask e2e --slice fixtures
```
