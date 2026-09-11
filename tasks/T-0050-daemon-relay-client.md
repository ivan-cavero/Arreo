---
id: T-0050
title: Relay stream and session — an encrypted byte stream to a peer through the relay
phase: 2
priority: 2
status: done
depends_on: [T-0023, T-0025, T-0029, T-0030]
scope:
  # Re-scoped on split (see "Re-scope" below): this task is the transport half.
  # The daemon wiring it was split from is T-0051.
  - crates/arreo-core/src/relay/client.rs
  - crates/arreo-core/src/relay/mod.rs
  - crates/arreo-server/src/relay_client.rs
  - crates/arreo-server/src/lib.rs
  - crates/arreo-server/tests/relay_client.rs
  - .loop/evidence/T-0050/**
---

## Goal

Close the gap T-0029 recorded: the relay routes, but nothing on a machine dials it. This task lands
the transport half — a **relay session** (dial, register, keep the session up, reconnect with
bounded backoff) and a **relay stream**: an `AsyncRead + AsyncWrite` byte stream to one peer device
carried in relay envelopes, which is what lets T-0023's Noise channel run over a *message*
transport. The daemon's use of it (config, boot task, pane traffic, the two-real-daemon e2e) is
T-0051.

## Acceptance criteria

- [x] `RelayStream` implements `AsyncRead + AsyncWrite` and carries bytes to one peer device through
      relay envelopes; writes larger than one envelope are chunked, and a zero-length write is a
      no-op rather than an envelope.
- [x] T-0023's Noise channel runs over it unchanged: two devices complete a `SecureChannel` handshake
      with the relay in the middle, exchange data both ways, and each end sees the *other's* pinned
      key — one crypto path, not a second implementation.
- [x] The relay sees ciphertext: with the real relay running, a scan of its state directory, stdout
      and stderr finds zero plaintext marker strings from the data exchanged, while the two peers
      see them byte-identical.
- [x] Delivery failure ends the stream instead of losing bytes: when the relay reports an envelope
      as anything but `delivered`, the affected stream errors rather than silently dropping the
      chunk. A stream that quietly loses a chunk is worse than one that fails.
- [x] Reconnect is bounded and honest: the backoff policy is a pure, tested function
      (exponential with a ceiling and jitter), a refused registration is reported with the relay's
      own reason and is not retried in a tight loop, and an absent relay costs bounded retries.
- [x] No daemon behavior changes in this task: nothing in `main.rs` or `daemon.rs` is touched, so
      `--slice api`, `--slice lifecycle`, `--slice persistence`, `--slice tui` and `--slice theme`
      stay green with unchanged transcripts.
- [x] Evidence under `.loop/evidence/T-0050/`: the two-peer Noise-over-relay transcript, the
      ciphertext scan, the backoff table, and the delivery-failure transcript.

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

## Re-scope (2026-09-11, on starting the work)

**Split into T-0050 (this) and T-0051 (daemon wiring).** The task as written spanned three
deliverables that fail for different reasons: a byte-stream adapter over a message transport, the
daemon's configuration and boot lifecycle, and a two-real-daemon end-to-end proof. Each is a
session's work, and the first is the hard one — it is where the interesting failure modes live
(chunking, backpressure, and the fact that a relay envelope is a *message* while `SecureChannel`
wants a *stream*).

The line is drawn at the daemon's boundary: this task ends when two peers can hold an encrypted byte
stream through a real relay, proven against the real relay binary. T-0051 starts there and adds the
`relay` config section, the boot task, peer traffic from the daemon's panes, and the two-real-daemon
transcript.

**Consequence for the fence:** `crates/arreo-core/src/relay/**` was added. The stream adapter is
protocol-level (it is the thing any client of the relay needs, and the mobile core will need it
too), it is Apache-licensed like the rest of the relay protocol, and putting it in the daemon crate
would have made a third party reimplement it. The *session* stays in `arreo-server`, because
lifecycle and policy are the daemon's business.

**Honest note on "one crypto path":** the adapter exists precisely so T-0023's `SecureChannel` is
reused rather than a second Noise implementation appearing. That is why the criterion is phrased as
"the Noise channel runs over it unchanged".

## Landing notes (2026-09-11)

**Split, and the split is recorded above.** This task landed the transport: `RelaySession` (dial,
peer multiplexing, delivery attribution, reconnect policy) and `RelayStream` (the byte stream), with
the daemon wiring it was split from now T-0051.

**Three defects found by the tests, all real:**

1. **Splitting a duplex does not close it.** `RelayStream` splits its inner duplex so two tasks can
   own the two directions — but `tokio::io::split` keeps the underlying stream alive behind an
   `Arc`, so dropping one half leaves the other end open. A stream whose bytes had been refused by
   the relay therefore never told the caller: it recorded the failure and then waited forever on a
   duplex that could not close. The reader task now shuts the write direction explicitly. This is
   the same class as T-0023's "swallowed failure reason" — a failure that is *recorded* is not the
   same as a failure that is *reported*.
2. **A peer's first chunks arrived before anything was listening.** The reader task creates a peer
   entry on first contact; the first version dropped the receiver immediately, so the very first
   chunks of a handshake — the ones that matter most — were discarded. The peer table now parks the
   receiver until `stream_to` takes it.
3. **A vanished relay took half a minute to notice.** QUIC's default idle timeout is 30 s, so a
   daemon whose relay died would keep writing into a dead connection for that long before its
   reconnect loop could start. The client endpoint's idle timeout is now 15 s (keep-alives hold a
   live connection open; the idle timer is what catches the dead one), which makes "the relay is
   absent" a bounded, quick retry. Measured: 35 s before, ~15 s after.

**My own test bug, worth recording because the symptom misleads:** `SecureChannel::connect`'s third
argument is the id *we* announce, and I passed the peer's. The failure surfaced as `UnknownPeer` —
which reads like a key or pinning problem and sends you looking at the crypto. The test now says so
in a comment.

**Not changed:** `main.rs` and `daemon.rs` were not touched, so the local path is untouched by
construction; the five e2e slices confirm it. `crates/arreo-core/src/transport/quic.rs` gained the
idle-timeout constant (the one non-fence file), because the criterion "a relay that is simply absent
costs bounded retries" needs the connection to notice.

**A design change the tests forced, and the finding that came with it.** The first shape gave each
stream direction a task over one `tokio::io::duplex`. A peer that stopped reading then left the read
task blocked inside a `write_all` — the stream had recorded its failure but could never report it,
which is the same "recorded is not reported" class as T-0023's swallowed error. The read direction is
now polled straight off its channel (no task, no intermediate buffer) and only the write direction
keeps a duplex.

That rewrite was motivated by a slow-peer test I then **deleted**, because the test premise was
wrong and the honest finding is more interesting: **the relay's per-connection queue is the same size
as the session's per-peer bound, and QUIC flow control means the relay cannot deliver faster than the
local consumer drains — so through the relay, the session's own overflow branch is close to
unreachable.** It stays as a defensive bound (the alternative to a bound is unbounded memory, and to
failing loudly a silent gap), and it is recorded as "could not break it" rather than claimed as
tested. What *is* tested is the user-visible half: a sender whose bytes the relay could not deliver
is told, and that stream fails instead of gapping.

**Honest gaps:** the session multiplexes peers over one relay connection, so one slow peer's
backpressure is felt on the shared outbound queue; a reconnect yields a new stream, so the Noise
session above it is re-established rather than resumed; a relay that vanishes is noticed by the idle
timer (15 s) rather than immediately, which is the price of not probing.

## Evidence (2026-09-11)

- `.loop/evidence/T-0050/relay_client.txt` — 5 acceptance tests against the real relay binary (the
  full Noise-KK session through the relay with the ciphertext scan, chunking of a 200 KB payload, a
  zero-length write, a delivery failure ending the stream, a refused registration carrying the
  relay's reason, a vanished relay noticed in bounded time, and the backoff table) plus the core
  protocol tests and the whole relay suite.
- `.loop/evidence/T-0050/gates.txt` — workspace suite, clippy, fmt, supply chain, cross-target,
  e2e slices and bench.
- `specs/adr/0014-relay-stream.md` — why the transport is adapted rather than the layers above it.
