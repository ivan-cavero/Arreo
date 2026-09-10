---
id: T-0013
title: Protocol framing v0 — MessagePack schema, codec, versioned messages
phase: 1
priority: 3
status: todo
depends_on: [T-0003]
scope:
  - crates/arreo-core/src/proto/**
  - crates/arreo-core/tests/proto/**
---

## Goal

The one protocol every surface speaks (client, phone, web, another server). MessagePack
frames over the socket; schema versioned from the first commit (N−1 compat is a later
promise — make it possible now).

## Acceptance criteria

- [ ] Message types: hello/version, snapshot, delta, resume, error, state-event, metrics.
- [ ] Codec round-trip property tests (proptest): any encoded message decodes identically.
- [ ] Version field + negotiation path (reject-only-for-incompatible in v0).
- [ ] Fuzz target on the decoder: garbage in = error out, never panic (_corpus committed).
- [ ] Codec is zero-copy where cheap; 1MB delta encode/decode < 5 ms (bench probe).

## Verification

```console
cargo test -p arreo-core proto
cargo xtask bench --probe proto
```
