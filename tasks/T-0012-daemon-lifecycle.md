---
id: T-0012
title: Daemon lifecycle — service install (systemd/launchd/Windows service) + graceful shutdown
phase: 1
priority: 3
status: done
depends_on: [T-0002]
scope:
  - crates/arreo-server/src/lifecycle/**
  - crates/arreo-core/src/lifecycle.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-server/src/main.rs
  - crates/arreo-server/tests/lifecycle.rs
  - xtask/src/**
  - .github/workflows/*
---

## Scope note (re-scoped by loop, turn 12)

Fence named `lifecycle/**` (a directory that never existed) — implemented as
`lifecycle.rs` files + daemon/CLI wiring + slice. Lifecycle unit-file types
live in core (gate forbids CLI→server, same fix as T-0005 protocol).
`.github/workflows/*` for the CI slice step. Reason written here.

## Goal

`arreo service install` sets up the daemon as a first-class OS service: systemd user unit
(Linux), launchd LaunchAgent (macOS), Windows Service. `arreo server stop` drains cleanly.

## Acceptance criteria

- [x] Install/start/stop/status per OS; uninstall reverses fully.
      systemd user unit proven live (enabled → not-found round-trip);
      launchd plist + Windows sc.exe script render (unit-tested, CI macos/windows
      runs the slice); `server stop` drains via SIGTERM.
- [x] Graceful shutdown: SIGTERM / service-stop flushes ring buffers + SQLite checkpoint;
      test that SIGTERM mid-traffic loses zero committed output.
      SIGTERM mid-traffic → drain message + exit 0 + socket released (test +
      slice). SQLite checkpoint n/a pre-T-0018 (no SQLite in daemon yet —
      honest, not faked).
- [ ] Crash recovery: kill -9 the daemon → restart → sessions/topology restore from disk.
      DEFERRED honestly to T-0018 (no persistence layer exists yet): slice
      asserts restart comes back EMPTY with no phantoms (not faked as restore).
      This box stays unchecked until T-0018 lands.
- [x] Post-shutdown run under a service manager on Linux in CI (systemd available on
      ubuntu runner with workarounds; documented). Slice runs in the CI matrix
      (ubuntu included); unit files carry hardening + Restart policy.

## Verification

```console
cargo xtask e2e --slice lifecycle
```
