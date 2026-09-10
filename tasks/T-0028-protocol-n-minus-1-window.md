---
id: T-0028
title: Protocol N−1 window — deterministic refuse-vs-downgrade rules for mixed versions
phase: 2
priority: 3
status: proposed
depends_on: [T-0013, T-0014]
scope:
  - crates/arreo-core/src/proto/**
  - crates/arreo-core/tests/compat.rs
  - crates/arreo-core/tests/fixtures/**
  - xtask/src/compat_slice.rs
  - xtask/src/main.rs
  - .github/workflows/*
  - specs/adr/**
---

## Goal

Updates must never strand an attached client (§3.13): a v1 client against a v2 server and a
v2 client against a v1 server keep working for the verbs they share. This task builds the
N−1 window on T-0013's `Hello.wants`/`Welcome.v` hook and writes down — then tests — the
deterministic rule for what is refused versus what is downgraded.

## Acceptance criteria

- [ ] `negotiate(server, wants)` becomes a window: accept the highest version common to
      `[server-1, server]`, echo it in `Welcome.v`, refuse anything outside with a typed
      `Error` naming the offered versions and our range — a downgrade is never silent.
- [ ] Rules written in an ADR (number assigned at write time, avoiding sibling collisions)
      and enforced in code: (a) unknown client→server *request* → typed `Error`, connection
      stays open so the client can report it, never a hang; (b) unknown server→client
      *event* → ignored and counted, never fatal; (c) new `#[serde(default)]` field → silent
      downgrade; (d) renamed/removed field or renumbered variant → refuse; (e) gap > 1 →
      refuse. T-0013's append-only rule becomes machine-checked rather than a comment.
- [ ] Classification without a full decode: a `{op, v}` probe reads the map head, so an
      unknown variant is classified request-or-event before the typed decoder sees it.
      Unknown variants never panic and never silently discard a state-mutating message.
- [ ] Compat matrix in `crates/arreo-core/tests/compat.rs` over committed corpora (v0 frozen
      from the T-0013 tests; v1 = the Phase 2 additions: device cert fields in `Hello`, the
      `revoked` error, metrics history). Both directions run the full verb sequence and
      assert: no panic, no hang on a refusal (the test fails on timeout), typed errors
      exactly where the rules say, and behavioral equality of the old client's output.
- [ ] Real-binary proof, new slice: `cargo xtask e2e --slice compat` in
      `xtask/src/compat_slice.rs`, registered in `xtask/src/main.rs` and added as a CI step —
      a real daemon driven by a v1-emulating client (`ARREO_PROTO_V1=1`, test-only) and a
      v1-speaking peer against the current client, asserting shared verbs plus one unshared.
- [ ] Refusals are recorded, never partial: a refused connection writes an audit row and
      leaves no session, and the 1 MB budget plus T-0013's fuzz-total decode still hold.
- [ ] Evidence under `.loop/evidence/T-0028/`: the rules table, both directions'
      transcripts, the unshared-verb refusal, and the bench line.

## Notes

- No new dependency: the probe reuses `rmp-serde`'s map head and the existing `Message`
  enum. Rejected: a hand-rolled schema registry (pays no rent for two versions).
- Shared files with T-0027: both touch `xtask/src/main.rs` and `.github/workflows/ci.yml`.
  T-0027 lands first and owns the `transport`/`pairing` arms; this adds one `compat` arm and
  one CI step, keeping the edits disjoint.
- Compatibility is a range check plus rules, not a translation layer: no dual readers,
  because T-0013's append-only + `serde(default)` discipline keeps N−1 cheap; a change that
  cannot honor it is refused and scheduled as a deferred update (§3.13). Honest gap: the
  window is proven for v0→v1 only — the first major break is out of scope, though the rules
  name it refused, and sibling protocol work (relay v0, mesh, handoff) must land its
  messages as v1 appends under these rules (prose dependency, not a `depends_on` id).

## Verification

```console
cargo test -p arreo-core compat
cargo xtask e2e --slice compat
cargo xtask e2e --slice api
```

The `compat` slice and its CI step are wired by this task.
