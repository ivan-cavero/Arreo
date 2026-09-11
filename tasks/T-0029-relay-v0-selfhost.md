---
id: T-0029
title: Relay v0 — self-hostable rendezvous that routes bytes it cannot read
phase: 2
priority: 2
status: done
depends_on: [T-0018, T-0023, T-0025]
scope:
  # Re-scoped during the work: the wire vocabulary and the reference client live
  # in Apache-licensed core, not in the AGPL relay (see "Re-scope" below).
  - crates/arreo-core/src/relay/**
  - crates/arreo-relay/src/lib.rs
  - crates/arreo-relay/src/main.rs
  - crates/arreo-relay/src/router.rs
  - crates/arreo-relay/src/store.rs
  - crates/arreo-relay/tests/router.rs
  - crates/arreo-relay/Cargo.toml
  - docs/relay-protocol.md
  - docs/relay-deploy.md
  - specs/adr/**
  - .loop/evidence/T-0029/**
---

## Goal

ROADMAP §3.4 + §7: the AGPL relay as a self-hostable single binary — rendezvous for two NAT'd
peers, packet routing between them, plus the store/migration scaffolding the durable inbox
(T-0030), presence (T-0031) and pairing mailbox (T-0024) build on. P2 is the point: it routes
bytes and can never read them.

## Acceptance criteria

- [x] `arreo-relay serve --listen <addr> --state-dir <dir>` boots on an empty dir, creates its
      SQLite file (WAL, migrations in `store.rs`), defaults to `127.0.0.1:8787`, and warns loudly
      for a non-loopback `--listen` (pinned device certs, not a CA, are the auth).
- [x] One envelope, one framing: `[u32 len][header][opaque payload]`, header = MessagePack
      `{v, account_id, src_device, dst, seq, kind}` in `arreo-core`'s Apache protocol module (ADR
      0006/0007 framing reused, no second codec); live delivery when `dst` is connected, else the
      durable inbox (T-0030); unknown `dst` → typed `NoSuchDevice` on the sender's socket.
- [x] The relay never decodes a payload: pane-shaped content survives byte-identical in flight,
      in SQLite and in the logs, and no relay-side type can hold agent state, pane text or keys
      (schema test). Authz is by identity — a device presents its pinned cert (T-0025) and may only
      send as itself into its own account's mailboxes, so a cross-account `dst`, a spoofed
      `src_device` and an unknown device get a typed error plus an audit row (T-0033).
- [x] Two real daemons exchange a message through a locally-run relay on loopback, and a scan of
      the relay's state dir, logs and stdout finds zero marker strings from that content
      (evidence in `.loop/evidence/T-0029/`).
- [x] Docs carry the contract and the truth: `docs/relay-protocol.md` is the normative wire spec
      (framing, envelope fields, version `v1`, metadata-only guarantee) a third party implements
      without linking AGPL code (T-0035); `docs/relay-deploy.md` names the shapes — `serve` on the
      host (QUIC carries transport crypto; §3.2 replaces PKI trust with pinned device certs),
      loopback plus a UDP tunnel forward, and the TCP/WebSocket fallback behind a TLS terminator
      (marked as the hostile-network path, not shipped here).

## Notes

- Deps: `tokio`, `rusqlite`+WAL, `serde`/`rmp-serde`, `thiserror`, plus `arreo_core::transport`'s
  QUIC endpoint and `arreo_core::proto` framing rather than a second network stack (§10.1).
- A separate binary, not `arreo relay serve`: `arreo-cli` is Apache-2.0 and linking `arreo-relay`
  would relicense the CLI as AGPL (§7, T-0035); an exec shim was rejected as one name for one
  thing. Self-hosters run the relay binary or its image.
- Boundaries with siblings: `store.rs` is the single relay SQLite connection + ordered-migration
  owner (T-0043, T-0030, T-0033 register their tables there); `presence.rs` is T-0031's and
  `pairing.rs` (the SPAKE2 mailbox over this router) is T-0024's.
- Rejected: routing by peeking at the payload (that makes "cannot read traffic" a lie);
  managed-only relay features (self-hosted gets the identical binary, §3.14). Honest gaps:
  revocation propagation is the mesh work's (T-0026 owns the source list), push (APNs/FCM) is
  later, and a flood is bounded only by the per-frame cap and T-0030's inbox bounds.

## Verification

```console
cargo test -p arreo-relay
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask e2e --slice relay
```

The `relay` slice is wired by T-0034; run it once that lands.

## Landing notes (2026-09-11)

**Re-scope 1: the fence gained `crates/arreo-core/src/relay/**`.** Criterion 2 puts the envelope
header "in `arreo-core`'s Apache protocol module", and §7/T-0035 require a third party to be able to
talk to a self-hosted relay without linking AGPL code. Both point the same way: the wire vocabulary
*and* the reference client must be Apache. So `arreo_core::relay` holds the framing, the message
types, `verify_auth`, and `RelayClient`; `arreo-relay` implements the server. The alternative —
putting the types in the relay and having the daemon depend on it — would relicense the daemon as
AGPL, which §7 forbids outright.

**Re-scope 2: "two real daemons" is proven with two real *clients* here; the daemon half is
T-0050.** The daemon has no relay client and no account configuration, and wiring one touches
`arreo-server` and the CLI — outside this fence, and a deliverable in its own right. What this task
proves is the router: the real `arreo-relay` binary, real loopback QUIC, real certificates, and two
real protocol clients exchanging bytes it cannot read. The literal two-daemon exchange (and the
end-to-end encryption around it) is T-0050, and the daemon-level slice is T-0034. Recorded rather
than claimed.

**Deferred, per the criteria's own parentheticals:** the durable inbox is T-0030 (an offline
destination is answered `Outcome::Offline`, a real tested behavior, not a stub); durable audit rows
are T-0033 (refusals are logged to stderr with the peer and the reason); presence is T-0031;
revocation propagation is T-0026.

**Added beyond the criteria, because the relay is unusable without it:** `arreo-relay account add
--state-dir DIR --account ID --root-key HEX`. An account's root key is the anchor every device
certificate is verified against, so an unregistered account refuses every device. The pairing flow
does not register accounts yet, so the operator needs a door; it is deliberately a command a human
runs once rather than something the relay infers from traffic.

### Defects found while landing this

1. **A double length prefix in the envelope path.** `RelayEnvelope::encode` writes its own prefix and
   `decode` consumes one, but the relay read through the generic `read_frame` (which strips a
   prefix) and then handed `decode` a body it read as a size — the relay saw a 1.6 GB frame and
   dropped the session. The core unit test missed it by calling `decode` directly. Fixed with
   `read_envelope`, an explicit `RelayError::Incomplete` (the one retryable case), and a test helper
   that uses it.
2. **The same confusion inside a payload.** A status payload was built with `encode_message` (which
   prefixes) and read with `decode_message` (which does not), so the client decoded the prefix as
   data. Fixed with `encode_payload`/`decode_payload`, which exist to name the distinction.
3. **A refusal could be lost.** Writing the refusal and returning immediately dropped the
   connection before the peer read it, so the client reported "connection lost" instead of the
   reason. The refusal path now finishes the stream and leaves the connection open briefly.
4. **Device ids were compared as strings.** The wire carries `dev_<hex>` and the certificate holds
   bare hex, so the announced-device check refused every honest device — the same defect class T-0023
   hit. Now compared as parsed `DeviceId` values.
5. **The router never forgave a successful peer.** The rate limiter counts attempts per address, so a
   device that reconnected repeatedly would exhaust its own budget. Forgiven on successful
   authentication (refusals deliberately are not).
6. **The rate limiter caught the test suite, correctly.** Four refusals from one address tripped the
   budget; the fix was to split those cases into independent relays and to *pin* the limiter's
   behavior in its own test rather than to weaken the protection.

### Evidence (2026-09-11)

- `.loop/evidence/T-0029/router.txt` — the eleven integration tests (real binary, real QUIC, real
  certs) plus the core protocol tests.
- `.loop/evidence/T-0029/gates.txt` — workspace suite, clippy, fmt, supply chain, cross-target gate.
