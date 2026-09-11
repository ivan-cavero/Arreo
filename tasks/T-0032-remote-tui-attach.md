---
id: T-0032
title: Remote TUI attach — the existing TUI drives a pane on another machine
phase: 2
priority: 3
status: done
depends_on: [T-0015, T-0023, T-0029, T-0030, T-0050]
scope:
  - crates/arreo-tui/src/client.rs
  - crates/arreo-tui/src/main.rs
  - crates/arreo-tui/src/model.rs
  - crates/arreo-tui/tests/remote.rs
  - perf-budget.toml
  - specs/adr/**
  - .loop/evidence/T-0032/**
  # Added while landing (see the re-scope): the relay *session* had to move from
  # `arreo-server` into `arreo-core`, because a client needs the same session a
  # daemon does and the dependency rule forbids `arreo-tui` from depending on
  # `arreo-server`. The daemon keeps its own half (config, identity, accept loop).
  - crates/arreo-core/src/relay/session.rs
  - crates/arreo-core/src/relay/mod.rs
  - crates/arreo-server/src/relay_client.rs
  # Added while landing: the three copies of the hex-key parser (pairing, CLI,
  # and the one this task needed) became one in core.
  - crates/arreo-core/src/identity/keys.rs
  - crates/arreo-core/src/identity/mod.rs
  - crates/arreo-cli/src/main.rs
  # Added while landing: the `relay` e2e slice, its dispatch, and the budget-row
  # reader the slice uses so `perf-budget.toml` stays the single source of truth.
  - xtask/src/relay_slice.rs
  - xtask/src/main.rs
  - xtask/src/bench.rs
---

## Goal

ROADMAP §3.7's core scenario: the TUI on the VPS drives the Pi's panes — the same client
binary, the same verbs and the same MessagePack frames (ADR 0007), just a different
transport, routed through the relay (T-0029/T-0023). A network drop is a reconnect, not a
lost session and not duplicated output.

## Acceptance criteria

- [x] `arreo-tui --remote <endpoint>` attaches to a remote daemon through the relay with **one
      client code path**: a `Transport` seam in `client.rs` (Unix-socket impl from T-0015
      unchanged, plus a QUIC/relay impl), with sidebar (panes + state + RAM), focus, attach,
      scrollback `Read{from_line}` and `send` all using the existing `Message` verbs. A bare
      machine name resolves through the directory later (T-0044, prose); this takes an endpoint.
- [x] Remote parity is asserted on the wire, not by screenshot: the same frame sequence
      (Hello→Welcome, Snapshot, Delta, Resume) is observed against the remote daemon as
      against a local one in the same run, with identical decode results for the same pane.
- [ ] **Split (see the re-scope below).** Drop = reconnect with bounded, honest backoff: full
      jitter, base 250 ms, cap 30 s, unlimited retries while the TUI is open, and `reconnecting`
      plus the next attempt in the status bar. Killing the relay connection at three points and
      reattaching renders a transcript **byte-identical** to a control run with no drop: no
      duplicated line, no gap. — The **client half is done and proven** (backoff, status line, the
      cursor-owned resume that replays an uninterrupted read exactly); the **drop case is blocked on
      T-0054**, because the relay does not tell a device that its peer disconnected, so the far end
      holds the dead stream and swallows the next handshake. Retrying harder cannot remove that
      wait, and the evidence is recorded rather than papered over.
- [ ] **Split, for the same reason as the drop criterion.** §3.14 reattach: after a simulated 15-day gap (injected clock + TTL overrides, never a real
      sleep) the client reaches a full overview in **< 3 s**, drains its queued messages in `seq`
      order, and re-running the drained batch re-executes nothing (ack + cursor, T-0030). §5's
      `cross_machine_attach_s = 3` is enforced and the reattach row it implies is added to
      `perf-budget.toml`.
- [x] Remote input is attributable: every remote `send` carries the device id and lands as an
      audit row on the machine that owns the pane (device, pane, timestamp, redacted; T-0033 owns
      the schema); a device without operator permission gets a typed error and no keystroke. Both
      halves are asserted against a real relay: the slice checks the peer's audit trail names the
      client device, and `crates/arreo-tui/tests/remote.rs` connects as a `viewer`, reads
      successfully, is refused a `send` with a typed `Error`, and shows the marker never reached the
      pane and no `send` row exists.
- [x] Local-first stays intact and failures are loud: `--slice tui` stays green with no transcript
      change; an unreachable relay is a typed error plus a visible "waiting for relay" state.
- [x] **Interactive evidence (§10.2):** scripted PTY drives the real `arreo-tui` binary
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
cargo xtask e2e --slice relay                        # 14 assertions, real relay + 2 machines + real TUI
cargo xtask e2e --slice relay --interactive-evidence # the same, writing the frames to .loop/evidence/T-0032/
cargo xtask e2e --slice tui                          # local-first, unchanged
cargo test -p arreo-tui --test remote                # the client against a real relay, in-process
```

## Re-scope (2026-09-11, while landing: the drop criterion)

**The reconnect half of the drop criterion moves to T-0054 (relay peer-disconnect signalling).**

The client side is built and proven: `Target` carries a remote daemon, the poller holds one
connection and reconnects on failure with the relay session's backoff (250 ms base, 30 s cap,
jittered), the status bar names the target and the next attempt, and the resume is cursor-owned, so a
resumed read replays an uninterrupted read line for line with no duplication and no gap.

What does **not** work is reconnecting to a peer that has not noticed the drop. The relay routes by
device id and keeps one stream per peer (ADR 0014) but never tells a device that its peer went away,
so the daemon still holds the dead stream and delivers the next handshake into it, where the Noise
layer reads it as garbage and the stream ends. The reconnect loop recovers only after the daemon
gives up that stream, which took more than 60 s in this turn's measurements — and no amount of client
retrying removes the wait. That is a wire-protocol addition plus a relay-side broadcast, with its own
failure modes (a notice racing the reconnect, a notice for a peer that already reconnected), so it is
its own task rather than a line here. Evidence for the claim is in `.loop/evidence/T-0032/`.

The §3.14 reattach criterion (a simulated 15-day gap, then a full overview in < 3 s with the inbox
drained in `seq` order) depends on the same reconnect path and moves with it.

Two smaller honest notes from landing:

- **The TUI's "attach" is `Read{from_line}`, not `Attach`.** The focused pane streams through
  incremental reads, so no `attach` row appears in the peer's audit log. That is the design (reads
  are deliberately unaudited, T-0033), and the slice asserts the session and the absence of a
  keystroke rather than a row that was never meant to exist.
- **A remote target is a connection, not a per-verb request.** An early draft opened a connection per
  poll; against the relay that is one dial and handshake per second, and it also collides with the
  one-stream-per-peer rule. The connection is now long-lived, which is what a client of a multiplexed
  transport should have been from the start.

## Re-scope (2026-09-11, during T-0029)

`depends_on` gained **T-0050** (daemon relay client). This task drives a pane on another machine,
which needs a daemon that is *connected to the relay* — the router (T-0029) routes, but nothing
dials it yet, and that wiring touches `arreo-server` rather than the relay. T-0050 is that half.
