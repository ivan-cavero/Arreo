---
id: T-0024
title: SPAKE2 pairing — 30-second code pairing without a password or LAN trust
phase: 2
priority: 2
status: proposed
depends_on: [T-0013, T-0025]
scope:
  - crates/arreo-core/src/pairing/**
  - crates/arreo-core/tests/pairing.rs
  - crates/arreo-relay/src/pairing.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-core/Cargo.toml
  - crates/arreo-relay/Cargo.toml
---

## Goal

The ritual behind "pair in 30 seconds" (§1, §3.3): `arreo pair` prints a short human code
and a QR; a phone or another machine types or scans it and joins — no password to type, no
SSH, no assumption that the LAN is friendly. SPAKE2 (RFC 9382) over the relay mailbox turns
the low-entropy code into a strong authenticated channel, so an active MITM gets exactly
one guess. Output: a device keypair pinned with a cert issued by T-0025, and a burned code.

## Acceptance criteria

- [ ] `arreo pair` emits a 4-word code from a 256-word list (~32 bits) plus an
      `arreo://pair?...` QR carrying the relay URL, session id and server static public
      key; no key material in the code; `--json` mode for the slice. The word list is
      committed so codes are testable offline.
- [ ] Wrong code → failure with no partial state: key confirmation fails, both processes
      exit nonzero, and the device identity dir is byte-identical before/after (hash of the
      tree); no keypair, no pin, no cert, exactly one `pairing_failed` audit row.
- [ ] A captured transcript cannot be replayed to pair: recorded SPAKE2 flights replayed
      into a fresh session are refused (session id is single-use, nonces repeat), and a
      second use of a correct code fails — one wrong guess burns the code server-side.
- [ ] The window is time-bounded and single-use: default 300 s (`ARREO_PAIR_TTL_SECS`,
      test-injectable), one active pairing session per server by default (a second
      concurrent `arreo pair` refuses loudly); after expiry the session dies and the QR is
      dead. Asserted with a 1 s TTL in the test, never a 5-minute sleep.
- [ ] Real process boundary: `crates/arreo-core/tests/pairing.rs` drives the real `arreo`
      binary as the server and a second real process as the phone, meeting over the relay
      mailbox on a loopback socket — no in-process mocks. T-0027's `--slice pairing`
      re-asserts the same exchange end-to-end with evidence frames.
- [ ] Guess budget enforced, not hoped: one guess per session (burned on failure), and the
      mailbox refuses a session id that already completed, so an offline attacker holding
      the full transcript still needs the code.
- [ ] Audit rows for pairing success and failure (device id, session id, timestamp), with
      evidence under `.loop/evidence/T-0024/`.

## Notes

- `spake2` (RFC 9382, ed25519 group) over hand-rolling a PAKE: PAKEs are exactly where
  hand-written crypto dies. Rejected: SRP (older, murkier provenance); code-as-PSK under
  plain Noise (low-entropy PSK leaks an offline verifier); Noise-XX keyed by the code (same
  offline-guess hole). Reuses the ed25519 dependency T-0025 introduces.
- Magic Wormhole heritage: the mailbox only orders opaque SPAKE2 messages. The code never
  leaves the two devices, so the relay and any LAN sniffer see ciphertext plus a session id
  — the "relay cannot read" claim (§3.4, §4) holds for pairing too.
- This task owns only the *pairing mailbox* in `arreo-relay` (store, single-use, TTL).
  Routing, presence and the durable per-device inbox are the relay v0 work (sibling range,
  soft dependency in prose — not a `depends_on` id); they must not weaken these guarantees.
- Honest gaps: mailbox admission is minimal (session id + server static key) and the
  in-memory mailbox is not durable — a relay restart mid-pairing fails the pairing loudly
  (the client re-runs `arreo pair`). Persisting the mailbox belongs to the durable-inbox
  work; single-use semantics are still enforced by the server, not the relay.

## Verification

```console
cargo test -p arreo-core pairing
cargo test -p arreo-relay pairing
```

`cargo xtask e2e --slice pairing` (real binaries, evidence frames) is wired by T-0027.
