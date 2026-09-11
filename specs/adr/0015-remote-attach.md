# ADR 0015 — Remote attach: one client, a transport seam, and who owns the cursor

Status: accepted (T-0032)
Context: ROADMAP §3.7, §5 (`cross_machine_attach_s`), ADR 0007 (one protocol), ADR 0011
(Noise-QUIC), ADR 0014 (relay stream)

## The decision

**A remote TUI is the same client over a different transport.** `arreo-tui` gains a `Target`
(local socket, or a daemon on another machine reached through the relay) and nothing else changes:
the same `Message` frames, the same msgpack codec, the same verbs, the same resume semantics. The
sidebar, the focused-pane stream, the wall and the theme picker do not know which transport carried
their bytes.

**The cursor belongs to the client, not the daemon.** The UI keeps a per-pane `from_line` in its
subscription and sends `Read { from_line }`. The daemon's scrollback ring is the source of truth for
*content*; the client is the source of truth for *how much it has rendered*. A resume is therefore a
read from a cursor, not a session the daemon has to remember, and the transcript is assembled the
same way whether or not a connection ever dropped.

**A drop is a reconnect, never a lost session.** The TUI holds one connection and reconnects when it
ends, with the relay session's own backoff (exponential, 250 ms base, 30 s ceiling, jittered). The
cursors are not reset by a reconnect, so the resumed read replays exactly what an uninterrupted read
saw: no duplicated line, no gap. The status bar says `reconnecting`, names the target and the next
attempt.

**The connection is long-lived, and that is a correctness requirement, not an optimization.** The
far end multiplexes **one stream per peer device** (ADR 0014). Opening a second session for the same
device while the first is held does not create a second stream — it hands the new handshake to the
old stream, where it is swallowed. So a client must close before it redials, and the TUI's reconnect
loop is strictly sequential.

**The remote target pins the peer's key.** The relay routes by device id, and the relay decides
where an envelope goes, so the device id alone says nothing about who answers. The Noise handshake
proves the peer holds the private key for the key the pairing flow pinned (`server.key`); trusting
the relay's routing instead would be trusting the relay.

## What this rules out

- **A remote-only client.** §3.7's "uniform protocol, three roles" means VPS→Pi is the same code
  path as phone→VPS. A second client would fork the framing, the resume semantics and every future
  verb.
- **Driving the CLI over SSH.** It would put the protocol in a shell, make the transport a text
  stream nobody can type-check, and lose the per-verb authorization the daemon already applies to
  every session.
- **A connect-per-verb loop on a remote target.** Each verb would be a relay dial and a handshake;
  at a one-second cadence with a focus that is one handshake per second, and it would multiply the
  one-stream-per-peer hazard by the number of verbs.
- **Truncate-on-export or reader-side redaction.** Not this ADR's subject, but the same principle
  from T-0033 applies to the peer's audit rows: redaction happens at write, so no reader can
  un-redact.

## Consequences

- `arreo-core` gains `relay::session` (moved from `arreo-server`, T-0050's code): the session is
  what *both* ends of §3.7 need, and the dependency rule forbids `arreo-tui` from reaching into
  `arreo-server`. The daemon keeps what is the daemon's — config, identity, the accept loop.
- A remote target needs four things on disk: this device's `device.key`, its certificate under
  `devices/<bare-hex>.cert`, the pinned `server.key`, and an account name. All four are what
  `arreo pair --join` writes; `--identity` names the directory.
- `perf-budget.toml`'s `cross_machine_attach_s` (3 s) is enforced by the `relay` slice, which reads
  the row's number from the file rather than holding a copy. Measured on this box: **406 ms** from
  the keypress to the peer's output on screen.
- The TUI's `--socket` stays the default and the local path is unchanged: `--slice tui` is green
  with no transcript change.

## Known limitation (T-0054)

A client that vanishes is not noticed by the far end until one of its reads or writes fails, and the
relay does not tell a device that its peer went away. So after an abrupt drop the daemon still holds
the dead stream and hands the next handshake to it, where it is swallowed; the reconnect loop
recovers only once the daemon gives up that stream. Retrying harder cannot remove the wait — the
relay must signal peer disconnects, which is T-0054. Until it lands, the reconnect path is exercised
against a *closed* session (which the far end does notice) and the drop case is not claimed.
