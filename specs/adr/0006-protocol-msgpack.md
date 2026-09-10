# ADR 0006: protocol framing — MessagePack (`rmp-serde`) + `u32 LE` frames

- Status: accepted (2026-09-10, T-0013)
- Context: ROADMAP §3.2 needs one uniform binary protocol for CLI, phone,
  web, and server-as-client; T-0005's JSONL was honestly temporary (ADR
  0005). N−1 compat must be possible from the first commit.
- Decision: `rmp-serde` 1 over serde types in `arreo-core::proto`; one
  `Message` enum (hello/welcome/snapshot/delta/resume/error/state-event/
  metrics + 1:1 control verbs); `u32 LE length + bytes` stream framing;
  `negotiate` is reject-if-incompatible in v0; JSONL kept working untouched
  (daemon/CLI still speak it — cutover is T-0014's job, not this task's).
- Why this one:
  - `rmp-serde` (not hand-rolled msgpack, not protobuf/bincode): serde
    derive reuse — the SAME structs serve JSON (debug) and msgpack (wire);
    no schema compiler, no build step. 1 MB delta best-of-5: 0 ms/0 ms vs
    the 5 ms budget — zero-copy headroom to spare.
  - Single `Message` enum with `#[serde(tag = "op")]` (not separate
    per-verb structs): one codec, one fuzz target, exhaustive matching at
    every consumer. New variants append-only + `#[serde(default)]` fields —
    that IS the N−1 mechanism, enforced by rule not hope.
  - `u32 LE` prefix (not newlines, not QUIC streams yet): exact boundaries
    over any stream socket today; QUIC datagrams reuse the same frames later.
- Alternatives rejected:
  - Hand-rolled msgpack: re-implements an audited codec for no gain
    (rejected: security + maintenance).
  - protobuf: schema compiler + build dep + worse Rust ergonomics for a
    15-variant enum (rejected: simplicity).
  - Cutting the daemon over in this task: mixes codec delivery with
    migration risk; JSONL works and is tested — cutover belongs to T-0014
    with its own tests (rejected: work-unit discipline).
- Consequences: `PaneInfo` unified across framings (identical shape);
  `proptest` round-trips + committed corpus pin the decoder; timing gate is
  release-only in unit tests (debug noise documented) with `bench --probe
  proto` as the executable budget.
