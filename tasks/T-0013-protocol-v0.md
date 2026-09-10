---
id: T-0013
title: Protocol framing v0 — MessagePack schema, codec, versioned messages
phase: 1
priority: 3
status: done
depends_on: [T-0003]
scope:
  - crates/arreo-core/src/proto/**
  - crates/arreo-core/tests/proto/**
  - crates/arreo-core/Cargo.toml
  - xtask/src/bench.rs
---

## Scope note (re-scoped by loop, turn 13)

`Cargo.toml` (rmp-serde + proptest deps) and `xtask/src/bench.rs`
(`--probe proto`, required by the criterion's own verification line) are
outside the letter but required by it. Daemon cutover explicitly NOT in
this task (T-0014 owns it) — JSONL still serving, proven live.

## Goal

The one protocol every surface speaks (client, phone, web, another server). MessagePack
frames over the socket; schema versioned from the first commit (N−1 compat is a later
promise — make it possible now).

## Acceptance criteria

- [x] Message types: hello/version, snapshot, delta, resume, error, state-event, metrics.
      (+ Welcome + 1:1 control verbs so the T-0014 cutover is mechanical.)
- [x] Codec round-trip property tests (proptest): any encoded message decodes identically.
      `any_message_round_trips` (id/line/count/code strategies) + per-type unit trips.
- [x] Version field + negotiation path (reject-only-for-incompatible in v0).
      Every variant carries `v`; `negotiate` exact-matches in v0 (N−1 with v1).
- [x] Fuzz target on the decoder: garbage in = error out, never panic (_corpus committed).
      `decoder_total_on_arbitrary_bytes` (proptest, 0–256 random bytes) +
      `proto_corpus_decode` (8 pinned cases). Deterministic-corpus reading of
      the criterion (no cargo-fuzz dep — same policy as T-0009).
- [x] Codec is zero-copy where cheap; 1MB delta encode/decode < 5 ms (bench probe).
      `decode_borrowed` pins the zero-copy API; `bench --probe proto`
      best-of-5 PASS (0 ms/0 ms). Unit timing gate is release-only (debug
      noise 6–30 ms documented in-test, not wished away).

## Verification

```console
cargo test -p arreo-core proto
cargo xtask bench --probe proto
```
