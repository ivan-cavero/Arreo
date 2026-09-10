---
id: T-0014
title: Socket API v1 — read · send · wait · spawn · split · attach · metrics verbs
phase: 1
priority: 3
status: done
depends_on: [T-0005, T-0013]
scope:
  - crates/arreo-server/src/api/**
  - crates/arreo-cli/**
  - docs/agent-skill.md
---

## Goal

The agent-native surface (Herdr's proven design + `metrics`): one socket API for humans,
scripts, and agents. This is where Arreo stops being a demo and becomes a runtime.

## Acceptance criteria

- [x] Verbs implemented + documented with examples: `read · send · wait · spawn · split ·
      attach · metrics · panes list`. All 8 on framed MessagePack; CLI verbs live;
      `docs/agent-skill.md` documents each with executed examples.
- [x] `wait` supports state conditions (`wait --state question --timeout 5m`) — the agent
      orchestration primitive. Server-side watch (50 ms polls, exact-once answer,
      loud timeout error). Proven live: question in 2.5 s, timeout exit 1.
- [x] `docs/agent-skill.md`: teaches any CLI harness how to drive Arreo (dogfood: the loop
      agent itself uses these verbs to manage its own workers). Every example executed
      verbatim against the live daemon this turn.
- [x] Every verb has an integration test over the real socket (no mocks-only).
      `arreo-server/tests/api.rs` (4 tests: handshake, verbs, wait, metrics) +
      `cargo xtask e2e --slice api` runner; chaos/lifecycle slices re-greened
      post-cutover.

## Verification

```console
cargo xtask e2e --slice api
```
