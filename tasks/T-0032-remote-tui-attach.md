---
id: T-0032
title: Remote TUI attach — the existing TUI drives a pane on another machine
phase: 2
priority: 3
status: proposed
depends_on: [T-0015, T-0023, T-0029, T-0030, T-0050]
scope:
  - crates/arreo-tui/src/client.rs
  - crates/arreo-tui/src/main.rs
  - crates/arreo-tui/src/model.rs
  - crates/arreo-tui/tests/remote.rs
  - perf-budget.toml
  - specs/adr/**
  - .loop/evidence/T-0032/**
---

## Goal

ROADMAP §3.7's core scenario: the TUI on the VPS drives the Pi's panes — the same client
binary, the same verbs and the same MessagePack frames (ADR 0007), just a different
transport, routed through the relay (T-0029/T-0023). A network drop is a reconnect, not a
lost session and not duplicated output.

## Acceptance criteria

- [ ] `arreo-tui --remote <endpoint>` attaches to a remote daemon through the relay with **one
      client code path**: a `Transport` seam in `client.rs` (Unix-socket impl from T-0015
      unchanged, plus a QUIC/relay impl), with sidebar (panes + state + RAM), focus, attach,
      scrollback `Read{from_line}` and `send` all using the existing `Message` verbs. A bare
      machine name resolves through the directory later (T-0044, prose); this takes an endpoint.
- [ ] Remote parity is asserted on the wire, not by screenshot: the same frame sequence
      (Hello→Welcome, Snapshot, Delta, Resume) is observed against the remote daemon as
      against a local one in the same run, with identical decode results for the same pane.
- [ ] Drop = reconnect with bounded, honest backoff: full jitter, base 250 ms, cap 30 s, unlimited
      retries while the TUI is open, and `reconnecting` plus the next attempt in the status bar.
      Killing the relay connection at three points (during snapshot, mid-delta, idle) and
      reattaching via `Resume{last_line}` renders a transcript **byte-identical** to a control run
      with no drop: no duplicated line, no gap.
- [ ] §3.14 reattach: after a simulated 15-day gap (injected clock + TTL overrides, never a real
      sleep) the client reaches a full overview in **< 3 s**, drains its queued messages in `seq`
      order, and re-running the drained batch re-executes nothing (ack + cursor, T-0030). §5's
      `cross_machine_attach_s = 3` is enforced and the reattach row it implies is added to
      `perf-budget.toml`.
- [ ] Remote input is attributable: every remote `send` carries the device id and lands as an
      audit row on the machine that owns the pane (device, pane, timestamp, redacted; T-0033 owns
      the schema); a device without operator permission gets a typed error and no keystroke.
- [ ] Local-first stays intact and failures are loud: `--slice tui` stays green with no transcript
      change; an unreachable relay is a typed error plus a visible "waiting for relay" state.
- [ ] **Interactive evidence (§10.2):** scripted PTY drives the real `arreo-tui` binary
      against a real remote daemon through a locally-run relay — real key events, frame
      captures, and the drop/reconnect transcript under `.loop/evidence/T-0032/`.

## Notes

- The remote transport already lives in `arreo-core` (T-0023), which `arreo-tui` depends on,
  so this task adds no crate edge and no new dependency; the TUI keeps knowing nothing about
  QUIC beyond "there is a byte-stream transport that speaks `Message`".
- Why a transport seam and not a second remote client: §3.7's "uniform protocol, three roles"
  means VPS→Pi is the same code path as phone→VPS; a remote-only client would fork the framing,
  the resume semantics and every future verb. Rejected: driving the CLI over SSH.
- Resume semantics are recorded in an ADR (next free number in `specs/adr/`): the client owns the
  cursor, `Resume` replays from it, and exactly-once rendering dedupes an at-least-once stream.
- Honest gaps: no name→endpoint resolution, no cross-machine fan-in (T-0045), no per-machine
  role grants (T-0046) — this consumes an endpoint and an authorized device; a drop longer than
  the relay's retention (T-0030) loses queued commands and says so.

## Verification

```console
cargo xtask e2e --slice relay
cargo xtask e2e --slice relay --interactive-evidence
cargo xtask e2e --slice tui
```

## Re-scope (2026-09-11, during T-0029)

`depends_on` gained **T-0050** (daemon relay client). This task drives a pane on another machine,
which needs a daemon that is *connected to the relay* — the router (T-0029) routes, but nothing
dials it yet, and that wiring touches `arreo-server` rather than the relay. T-0050 is that half.
