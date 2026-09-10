---
id: T-0029
title: Relay v0 — self-hostable rendezvous that routes bytes it cannot read
phase: 2
priority: 2
status: proposed
depends_on: [T-0018, T-0023, T-0025]
scope:
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

- [ ] `arreo-relay serve --listen <addr> --state-dir <dir>` boots on an empty dir, creates its
      SQLite file (WAL, migrations in `store.rs`), defaults to `127.0.0.1:8787`, and warns loudly
      for a non-loopback `--listen` (pinned device certs, not a CA, are the auth).
- [ ] One envelope, one framing: `[u32 len][header][opaque payload]`, header = MessagePack
      `{v, account_id, src_device, dst, seq, kind}` in `arreo-core`'s Apache protocol module (ADR
      0006/0007 framing reused, no second codec); live delivery when `dst` is connected, else the
      durable inbox (T-0030); unknown `dst` → typed `NoSuchDevice` on the sender's socket.
- [ ] The relay never decodes a payload: pane-shaped content survives byte-identical in flight,
      in SQLite and in the logs, and no relay-side type can hold agent state, pane text or keys
      (schema test). Authz is by identity — a device presents its pinned cert (T-0025) and may only
      send as itself into its own account's mailboxes, so a cross-account `dst`, a spoofed
      `src_device` and an unknown device get a typed error plus an audit row (T-0033).
- [ ] Two real daemons exchange a message through a locally-run relay on loopback, and a scan of
      the relay's state dir, logs and stdout finds zero marker strings from that content
      (evidence in `.loop/evidence/T-0029/`).
- [ ] Docs carry the contract and the truth: `docs/relay-protocol.md` is the normative wire spec
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
