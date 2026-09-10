---
id: T-0023
title: Noise-QUIC remote transport — pinned-device encrypted sessions over QUIC
phase: 2
priority: 2
status: proposed
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

- [ ] One QUIC bidi stream per session carrying the existing `codec::encode_frame` frames,
      `Noise_KK_25519_ChaChaPoly_BLAKE2s` first, then `Hello`/`Welcome`, then
      snapshot → delta → resume with semantics identical to the local path (§3.2).
- [ ] Tampered ciphertext is rejected: flipping one byte of any transport frame yields a
      typed `TransportError::Decrypt`, the socket closes, zero plaintext bytes reach the
      client, and the daemon registry is unchanged (pane list compared before/after).
- [ ] A replayed handshake fails: a recorded initiator flight replayed against a fresh
      connection is refused (increasing Noise-KK nonces, different server ephemeral, a
      per-peer seen-handshake guard) — no session, no pin touched; retries halt at 3/10 s.
- [ ] Local Unix-socket behavior unchanged: `--slice api`, `--slice lifecycle`,
      `--slice persistence`, `--slice tui` stay green with unchanged transcripts; the
      transport is a second connection path, never a rewrite of the local one.
- [ ] Outbound-only posture: the shipped daemon opens no inbound listener; the loopback
      listener behind `ARREO_TRANSPORT_TEST_LISTEN=1` is a documented test seam (§4).
- [ ] Overhead measured, not assumed: handshake ≤ 1 extra RTT over the local path, the 1 MB
      delta budget holds, and endpoint RSS keeps the §5 budget (≤ 120 MB @ 30 panes + 5
      clients); numbers in `.loop/evidence/T-0023/`.
- [ ] Evidence frames (handshake log, tamper/replay transcripts, RSS + latency numbers)
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
