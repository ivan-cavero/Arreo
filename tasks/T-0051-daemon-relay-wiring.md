---
id: T-0051
title: Daemon relay wiring — config, boot task, pane traffic, two-daemon e2e
phase: 2
priority: 2
status: done
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

- [x] The daemon reads its relay configuration from one place (a `relay` section: address, account
      id, this machine's peer device, and whether it is enabled) and starts the session as a
      background task at boot when enabled. **Disabled is the default**, and with it disabled the
      daemon's socket API is byte-for-byte unchanged — a self-hosted runtime must work with no relay
      at all.
- [x] Registration uses the machine's existing device identity and certificate (T-0025) — no new
      key, no new pin — and a refused registration is reported loudly with the relay's own reason,
      never retried in a tight loop.
- [x] Two real `arreo-server` processes, each with its own identity directory and its own socket,
      exchange a message through a real `arreo-relay` on loopback; a scan of the relay's state dir,
      stdout and stderr finds zero plaintext marker strings from that content, and both daemons'
      local socket APIs keep working throughout.
- [x] The carried payload is the **daemon-to-daemon Noise session** (T-0023) over T-0050's stream:
      the relay sees ciphertext, and the test asserts the plaintext marker never reaches it.
- [x] A dropped session is re-established by the daemon's own loop without losing the pane state it
      already holds; a relay that is simply absent costs bounded retries and never wedges the daemon
      or its local socket. (The base delay is capped at 30 s; jitter adds up to 25% on top, so the
      largest printed delay is ~37.5 s.)
- [x] Delivery outcomes are acted on: `queued` is logged with the queue depth, `no_such_device` is
      logged with the destination, and a `refused` outcome is surfaced as a bug rather than retried
      as a transient error.
- [x] Local behavior is untouched: `--slice api`, `--slice lifecycle`, `--slice persistence`,
      `--slice tui` and `--slice theme` stay green with unchanged transcripts.
- [x] `docs/relay-deploy.md` documents the daemon side: the config section, what the daemon logs at
      each step, and the two-machine setup with its honest gaps.
- [x] Evidence under `.loop/evidence/T-0051/`: the two-daemon transcript, the ciphertext assertion,
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

## Landing notes (2026-09-11)

**The daemon's relay identity is the one it already has.** No new key and no new storage: the
machine's own certificate is the file `arreo pair` already wrote
(`identity/devices/<bare-hex-id>.cert`), and `own_identity` reads exactly that. The relay therefore
authenticates the same identity the machine already presents — which is the point of "no new key, no
new pin".

**A relay peer runs the *same* session loop.** `serve_peer` completes a Noise handshake over the
relay stream and then calls `serve_session` — the identical loop the local socket and the direct
transport use — behind the same per-verb gate. So a device that reaches this daemon through the
relay has exactly the permissions it has locally, decided by the same code and the same authority.
That is what makes the relay a *transport*, not a second door.

**The probe is deliberately read-only.** When `peer` is configured, the daemon opens a session and
asks for the pane list, logging the count. It asks for the list rather than anything mutable because
a boot-time probe must not change the peer's machine; it is the seed of remote attach (T-0032), which
will attach instead of list. It is bounded by `PROBE_TIMEOUT` (15 s) so a peer that accepts and goes
quiet cannot leave a task waiting for the daemon's lifetime.

### A race the tests exposed, and the product weakness behind it

The two-daemon test failed intermittently: A's probe reported "the relay does not know that device"
because B had not reached the relay yet. The test was flaky, but the *cause* was a product
weakness — the probe ran **once per session**, so a machine that booted before its peer logged a
failure and never tried again until its own relay session dropped. Fixed: the probe now retries
`PROBE_ATTEMPTS` (5) times with doubling delays (1 s, 2 s, 4 s, 8 s) and gives up with one clear
line; a genuinely absent peer still costs one line rather than an endless retry. Verified stable
across repeated runs.

### A real defect the tests exposed, in code this task did not write

`reload()` deliberately accepts a certificate *file* with no store row ("a cert file with no record
still counts as a pinned device as long as the certificate verifies"), and `check_verb` goes through
the index — so the per-verb gate accepts such a device. But the daemon's handshake resolver asked
`devices()`, which lists the *store* only, and therefore refused it before the gate ever ran. One
question, two answers.

Fixed in `arreo-core`: `DeviceAuthority::device(id)` reads the index (the same source the gate uses),
and the resolver uses it. A test pins the property — a certificate file alone is pinned for both
doors — because the failure mode is a device that authorizes but cannot connect, which is invisible
until someone hand-copies a certificate.

**A test-harness lesson worth keeping:** the two-daemon test pins each machine's peer with
`arreo devices issue`, the product's own door, rather than writing certificate files. The authority's
index does accept a verifying certificate *file* with no store row (both doors agree by
construction — that is the fix above), so the reason is different and simpler: a hand-written file
is a state no product command produces. It carries no serial bookkeeping and no durable record, so
revocation and retirement have nothing to name. A test should set up what the product sets up.

**Four findings from the docs review that became code fixes**, not prose: the stale comment above
(my own change had made it false), `own_identity`'s error naming the identity *directory* instead of
`device.key`, no log line for an attempt in progress (so "a log line per attempt" was only true at
the attempt's *end*), and the backoff wording — the 30 s ceiling applies to the base and jitter is
added on top, so the largest printed delay is ~37.5 s. All four are fixed; the last is now stated
precisely in the task file and ADR 0014 rather than rounded.

**Honest gaps:** the probe lists panes rather than attaching (T-0032 owns attach); a reconnect loses
the Noise session, so a new stream and a new handshake follow; a relay peer must be pinned in the
authority's store (a certificate file alone is not enough for the daemon's gate); and `peer` being
optional means a serve-only machine still has to be *connected* to the relay for the relay to route
to it.

## Evidence (2026-09-11)

- `.loop/evidence/T-0051/daemon.txt` — 4 acceptance tests with two real `arreo-server` processes,
  their own identity directories, sockets and config files, a real relay on loopback: the message
  exchange with a plaintext scan of the relay's state and logs, both local sockets still serving, a
  relay that cannot be reached leaving the daemon serving, no configuration meaning no relay, and an
  incomplete configuration refused by name — plus the T-0050 transport tests and the authority tests.
- `.loop/evidence/T-0051/gates.txt` — workspace suite, clippy, fmt, supply chain, cross-target,
  e2e slices and bench.
