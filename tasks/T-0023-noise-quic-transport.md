---
id: T-0023
title: Noise-QUIC remote transport — pinned-device encrypted sessions over QUIC
phase: 2
priority: 2
status: done
depends_on: [T-0013, T-0018, T-0025]
scope:
  - crates/arreo-core/src/transport/**
  - crates/arreo-server/src/transport.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-cli/src/main.rs
  - Cargo.toml
  - crates/arreo-core/Cargo.toml
  - crates/arreo-server/Cargo.toml
  - specs/adr/**
---

## Goal

The remote path of ROADMAP §3.2: the same length-delimited MessagePack frames (T-0013) over
QUIC (`quinn`) inside a Noise-KK handshake (`snow`) authenticating pinned ed25519 device
keys — devices, not CAs (§3.3, §4). Sessions are client-initiated (zero inbound ports); the local Unix socket path (T-0014) stays the same-machine default.

## Acceptance criteria

- [x] One QUIC bidi stream per session carrying the existing `codec::encode_frame` frames,
      `Noise_KK_25519_ChaChaPoly_BLAKE2s` first, then `Hello`/`Welcome`, then
      snapshot → delta → resume with semantics identical to the local path (§3.2).
- [x] Tampered ciphertext is rejected: flipping one byte of any transport frame yields a
      typed `TransportError::Decrypt`, the socket closes, zero plaintext bytes reach the
      client, and the daemon registry is unchanged (pane list compared before/after).
- [x] A replayed handshake fails: a recorded initiator flight replayed against a fresh
      connection is refused (increasing Noise-KK nonces, different server ephemeral, a
      per-peer seen-handshake guard) — no session, no pin touched; retries halt at 3/10 s.
- [x] Local Unix-socket behavior unchanged: `--slice api`, `--slice lifecycle`,
      `--slice persistence`, `--slice tui` stay green with unchanged transcripts; the
      transport is a second connection path, never a rewrite of the local one.
- [x] Outbound-only posture: the shipped daemon opens no inbound listener; the loopback
      listener behind `ARREO_TRANSPORT_TEST_LISTEN=1` is a documented test seam (§4).
- [x] Overhead measured, not assumed: handshake ≤ 1 extra RTT over the local path, the 1 MB
      delta budget holds, and endpoint RSS keeps the §5 budget (≤ 120 MB @ 30 panes + 5
      clients); numbers in `.loop/evidence/T-0023/`.
- [x] Evidence frames (handshake log, tamper/replay transcripts, RSS + latency numbers)
      under `.loop/evidence/T-0023/` with no key material in them (existing secret scan).

## Notes

- `quinn` over `quiche`: pure Rust, tokio-native, no BoringSSL/OpenSSL C dependency — it
  cross-compiles for the mobile core and protects the daemon size budget. Over hand-rolled
  reliability: congestion control, stream muxing and connection migration are where
  guessing costs §5's latency budgets.
- `snow` over TLS-PKI: §4 pins devices, so a CA hierarchy is overhead plus another parser
  to fuzz. Over plain Noise-over-UDP: we would rebuild what QUIC gives us, and mobile
  network hops (§3.2 resume) need connection migration.
- `quinn-hyphae`/TLS-replaced-by-Noise inside QUIC is experimental (patched quinn fork on
  the security-critical path). Implemented shape: Noise-KK inside QUIC streams, QUIC's TLS
  reduced to a transport-only ephemeral cert deliberately not the trust anchor. Cost: one
  extra round trip, measured; if it breaks the < 1 s attach budget the ADR records rustls
  raw-public-key verification (iroh-style, still device-authenticated) as the fallback.
- Honest gaps: Noise does not authenticate QUIC's transport layer (a MITM can drop or
  tamper, never forge or read); 0-RTT/tickets are acceleration only — no security decision
  depends on resumption (§3.14). Relay routing and presence belong to the relay v0 work
  (sibling range, prose soft dependency — not a `depends_on` id).

## Verification

```console
cargo test -p arreo-core transport
cargo xtask bench
cargo xtask e2e --slice api
cargo xtask e2e --slice lifecycle
```

The `transport` slice (tamper/replay end-to-end, real sockets) is wired by T-0027.

## Evidence (2026-09-11)

- `.loop/evidence/T-0023/overhead.txt` — release numbers: QUIC connect 3.0 ms,
  QUIC+Noise 5.1 ms (Noise adds ~2.1 ms, one round trip), 1 MB through the
  transport 9.7 ms, RSS 22.7 MB with a listener plus five live sessions.
- `.loop/evidence/T-0023/negatives.txt` — 15 core transport tests + 5 daemon
  wiring tests, all green on real streams and real loopback sockets.
- `.loop/evidence/T-0023/transcripts.txt` — replay refusal, tamper refusal, and
  the secret scan over the evidence directory.
- `.loop/evidence/T-0023/gates.txt` — 236 workspace tests, clippy clean, fmt
  clean, vet/deny/audit green, `check-targets` 1 PASS + 1 SKIP.

Defects found and fixed while landing this (all real, all now covered by tests):

1. **The responder never learned who called.** `SecureChannel::accept` handed the
   resolver the raw 32-hex wire announcement, while the authority, the audit log
   and the CLI all name devices `dev_<hex>` — so a correct resolver refused every
   legitimate device. The seam now takes a parsed `DeviceId`, and `accept`
   returns the id alongside the channel (the authenticated X25519 key cannot
   yield the ed25519 fingerprint the id is derived from).
2. **A recorded first flight re-established a session.** KK's first message
   depends only on the responder's *static* key, so decryption cannot catch a
   replay: a fresh responder answers it and believes in a session with an absent
   device. The `FlightGuard` refuses a repeated authentic flight within a window.
3. **The pump could not answer a request.** Decrypted plaintext was flushed only
   at the top of the loop, so a reply sat buffered while the loop blocked in
   `select!` — a request/response exchange deadlocked, and the test suite hung
   with it. Every test exchange is now timeout-bounded so a regression fails
   loudly instead of stalling the suite.
4. **An impossible frame length stalled the session.** The `u16` prefix is
   necessarily outside the seal, so a rewritten length made the pump wait for a
   frame that could never complete. Lengths no seal can produce now end the
   channel immediately (`MAX_FRAME_BYTES`); a plausible-but-short one is
   indistinguishable from a frame in flight and is bounded by the transport's
   idle timeout (documented in the module).
5. **One quiet peer wedged the listener.** Accept and handshake were one call, so
   a peer that connected and then said nothing held the accept loop for the
   handshake timeout. `accept_connection` now returns the connection and the
   caller runs the handshake in its own task; a test proves a live device still
   connects while another stalls.
6. **The pump swallowed its failure reason.** A tampered stream surfaced to the
   consumer as a bare EOF. `SecureChannel` now reports the typed
   `TransportError::Decrypt` (as an `io::Error` carrying it) at that EOF, and the
   tamper test asserts the type rather than only the absence of plaintext.

Design decision: the Noise static is **derived from the pinned ed25519 identity**
rather than distributed as a second key — ADR `0011-remote-transport.md`. The
transport dependencies (`quinn`, `rustls`, `rcgen`, `snow`) are behind a default
`transport` feature, because `ring` compiles C: dropping the feature keeps a
genuinely C-free surface for `check-targets` to type-check foreign targets.
