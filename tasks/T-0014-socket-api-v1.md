---
id: T-0014
title: Socket API v1 — read · send · wait · spawn · split · attach · metrics verbs
phase: 1
priority: 3
status: todo
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

- [ ] Verbs implemented + documented with examples: `read · send · wait · spawn · split ·
      attach · metrics · panes list`.
- [ ] `wait` supports state conditions (`wait --state question --timeout 5m`) — the agent
      orchestration primitive.
- [ ] `docs/agent-skill.md`: teaches any CLI harness how to drive Arreo (dogfood: the loop
      agent itself uses these verbs to manage its own workers).
- [ ] Every verb has an integration test over the real socket (no mocks-only).

## Verification

```console
cargo xtask e2e --slice api
```
