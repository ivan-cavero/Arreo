---
id: T-0051
title: Daemon relay wiring — config, boot task, pane traffic, two-daemon e2e
phase: 2
priority: 2
status: proposed
depends_on: [T-0050]
scope:
  - crates/arreo-server/src/relay_client.rs
  - crates/arreo-server/src/main.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-server/tests/relay_daemon.rs
  - docs/relay-deploy.md
  - .loop/evidence/T-0051/**
---

## Goal

Make the daemon actually *use* the relay session T-0050 landed: read its relay settings from one
place, dial out at boot when enabled, carry a pane's traffic to a peer machine, and prove it with
two real `arreo-server` processes on loopback — the daemon half of ROADMAP §3.2's remote path, and
the literal form of the criterion T-0029 recorded.

## Acceptance criteria

- [ ] The daemon reads its relay configuration from one place (a `relay` section: address, account
      id, this machine's peer device, and whether it is enabled) and starts the session as a
      background task at boot when enabled. **Disabled is the default**, and with it disabled the
      daemon's socket API is byte-for-byte unchanged — a self-hosted runtime must work with no relay
      at all.
- [ ] Registration uses the machine's existing device identity and certificate (T-0025) — no new
      key, no new pin — and a refused registration is reported loudly with the relay's own reason,
      never retried in a tight loop.
- [ ] Two real `arreo-server` processes, each with its own identity directory and its own socket,
      exchange a message through a real `arreo-relay` on loopback; a scan of the relay's state dir,
      stdout and stderr finds zero plaintext marker strings from that content, and both daemons'
      local socket APIs keep working throughout.
- [ ] The carried payload is the **daemon-to-daemon Noise session** (T-0023) over T-0050's stream:
      the relay sees ciphertext, and the test asserts the plaintext marker never reaches it.
- [ ] A dropped session is re-established by the daemon's own loop without losing the pane state it
      already holds; a relay that is simply absent costs bounded retries and never wedges the daemon
      or its local socket.
- [ ] Delivery outcomes are acted on: `queued` is logged with the queue depth, `no_such_device` is
      logged with the destination, and a `refused` outcome is surfaced as a bug rather than retried
      as a transient error.
- [ ] Local behavior is untouched: `--slice api`, `--slice lifecycle`, `--slice persistence`,
      `--slice tui` and `--slice theme` stay green with unchanged transcripts.
- [ ] `docs/relay-deploy.md` documents the daemon side: the config section, what the daemon logs at
      each step, and the two-machine setup with its honest gaps.
- [ ] Evidence under `.loop/evidence/T-0051/`: the two-daemon transcript, the ciphertext assertion,
      the reconnect log, and the no-relay-configured run showing an unchanged local daemon.

## Notes

- Split out of T-0050, which landed the transport (session + stream). This task is wiring and
  lifecycle; the protocol and the crypto are already proven.
- Rejected: an inbound listener as the fallback (breaks §4's zero-inbound-port posture); a second
  encryption layer for relay traffic (T-0023's channel already provides it); holding the relay
  session in the CLI (the daemon owns pane traffic; the CLI is a client of the daemon).
- Honest gap: this lands the machine-to-machine path. Remote *TUI* attach over it is T-0032, and the
  combined daemon-level slice is T-0034. How a machine learns its account id at pairing time is the
  pairing flow's business (T-0024 grows that); until then the configuration is explicit.

## Verification

```console
cargo test -p arreo-server --test relay_daemon
cargo xtask e2e --slice api
cargo xtask e2e --slice lifecycle
```
