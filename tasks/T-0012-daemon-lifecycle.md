---
id: T-0012
title: Daemon lifecycle — service install (systemd/launchd/Windows service) + graceful shutdown
phase: 1
priority: 3
status: todo
depends_on: [T-0002]
scope:
  - crates/arreo-server/src/lifecycle/**
  - xtask/src/**
---

## Goal

`arreo service install` sets up the daemon as a first-class OS service: systemd user unit
(Linux), launchd LaunchAgent (macOS), Windows Service. `arreo server stop` drains cleanly.

## Acceptance criteria

- [ ] Install/start/stop/status per OS; uninstall reverses fully.
- [ ] Graceful shutdown: SIGTERM / service-stop flushes ring buffers + SQLite checkpoint;
      test that SIGTERM mid-traffic loses zero committed output.
- [ ] Crash recovery: kill -9 the daemon → restart → sessions/topology restore from disk.
- [ ] Post-shutdown run under a service manager on Linux in CI (systemd available on
      ubuntu runner with workarounds; documented).

## Verification

```console
cargo xtask e2e --slice lifecycle
```
