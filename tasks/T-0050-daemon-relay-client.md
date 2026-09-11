---
id: T-0050
title: Daemon relay client — the daemon dials out, registers, and routes through the relay
phase: 2
priority: 2
status: proposed
depends_on: [T-0023, T-0025, T-0029, T-0030]
scope:
  - crates/arreo-server/src/relay_client.rs
  - crates/arreo-server/src/lib.rs
  - crates/arreo-server/src/main.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-cli/src/main.rs
  - docs/relay-deploy.md
  - .loop/evidence/T-0050/**
---

## Goal

Close the gap T-0029 recorded: the relay routes, but nothing on a machine dials it. This is the
daemon's outbound half of §3.2 — the daemon connects *out* to the relay (zero inbound ports, §4),
authenticates with its own device identity, keeps the session up across a network that comes and
goes, and carries its pane traffic to a peer through envelopes whose contents the relay cannot read.

## Acceptance criteria

- [ ] The daemon reads its relay configuration from one place (`relay` section: address, account id,
      and whether it is enabled) and starts a background task at boot when enabled; disabled is the
      default, because a self-hosted runtime must work with no relay at all.
- [ ] Registration uses the machine's existing device identity and certificate (T-0025) — no new key,
      no new pin — and a refused registration is reported loudly with the relay's own reason, not
      retried in a tight loop.
- [ ] Two real `arreo-server` processes, each with its own identity directory, exchange a message
      through a real `arreo-relay` on loopback, and a scan of the relay's state dir, stdout and
      stderr finds zero marker strings from that content. This is T-0029's criterion made literal,
      and the evidence lands in `.loop/evidence/T-0050/`.
- [ ] The payload the relay carries is the **daemon-to-daemon Noise session** (T-0023): the relay
      sees ciphertext, and a test asserts that the bytes the relay handled do not contain the
      plaintext marker. Reusing T-0023's channel is the point — one crypto path, not two.
- [ ] Reconnect is bounded and honest: exponential backoff with a ceiling, jitter, and a log line per
      attempt; a session that drops mid-transfer is re-established without losing the pane state the
      daemon already holds, and a relay that is simply absent costs a bounded retry, never a
      spinning task or a wedged daemon.
- [ ] Envelope delivery outcomes are acted on: `offline` means "queued at the relay once T-0030
      lands, retried otherwise", `no_such_device` is logged with the destination, and a `refused`
      outcome is treated as a bug worth surfacing rather than a transient error to retry.
- [ ] Local behavior is untouched: `--slice api`, `--slice lifecycle`, `--slice persistence`,
      `--slice tui` and `--slice theme` stay green with unchanged transcripts; with no relay
      configured the daemon's socket API is byte-for-byte the same as today.
- [ ] Evidence under `.loop/evidence/T-0050/`: the two-daemon transcript, the ciphertext assertion,
      the backoff log, and the no-relay-configured run showing an unchanged local daemon.

## Notes

- The client is `arreo_core::relay::RelayClient` (T-0029) — Apache-licensed, so the daemon never
  links the AGPL relay crate (§7, enforced by T-0035's dependency gate). This task is wiring and
  lifecycle, not protocol.
- Depends on T-0030 because "offline is normal" only has an honest answer once the relay can queue;
  until then the daemon must treat `offline` as retry-later and say so.
- The account id and relay address are operator-supplied; how a machine learns them at pairing time
  is the pairing flow's business (T-0024 grows that), not this task's. Until then, configuration is
  explicit.
- Rejected: an inbound listener as the fallback (breaks §4's zero-inbound-port posture); a second
  encryption layer for relay traffic (T-0023's channel already provides it); holding the relay
  session in the CLI (the daemon owns pane traffic, the CLI is a client of the daemon).
- Honest gap: this lands the machine-to-machine path. Remote *TUI* attach over it is T-0032, and the
  combined daemon-level slice is T-0034.

## Verification

```console
cargo test -p arreo-server relay_client
cargo test -p arreo-cli --test machines
cargo xtask e2e --slice api
```
