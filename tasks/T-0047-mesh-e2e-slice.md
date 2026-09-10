---
id: T-0047
title: "`mesh` e2e slice — two real daemons + a relay on loopback"
phase: 2
priority: 4
status: proposed
depends_on: [T-0012, T-0018, T-0043, T-0044, T-0045, T-0046]
scope:
  - xtask/src/mesh_slice.rs
  - xtask/src/main.rs
  - xtask/src/chaos/mesh_reconnect.rs
  - xtask/src/chaos/mod.rs
  - .github/workflows/ci.yml
  - .loop/evidence/T-0047/**
---

## Goal

The referee for T-0043…T-0046: the mesh claims are only real if two actual `arreo-server` daemons plus
a relay, all on loopback, reproduce them hermetically and quickly. Mirrors how T-0015/T-0016 added
slices (`xtask` module + `--slice` registration + a CI step) so the mesh cannot regress silently, per
§10.1's "e2e suite is the definition of works".

## Acceptance criteria

- [ ] Wiring exactly like existing slices: `xtask/src/mesh_slice.rs` with a `run(rest)` entry,
      `mod mesh_slice;` plus `Some("mesh") => mesh_slice::run(rest)` in `main.rs`, the unknown-slice
      hint updated to include `mesh`, and one CI step (`cargo run -p xtask -- e2e --slice mesh`) in
      `.github/workflows/ci.yml` on all three OSes.
- [ ] Real processes, no fakes: the slice spawns two `arreo-server` daemons (A and B) and a self-hosted
      `arreo-relay`, each with its own temp state dir and ephemeral `127.0.0.1:0` ports; no root, no
      external network, no protocol mocking, and the daemons are the shipped binaries under test.
- [ ] Asserted behaviours (minimum): the directory lists both machines with presence and last-seen;
      staleness is derived from an injected clock (not by sleeping 30 days); a second claim of a live
      name yields the deterministic suffix plus the conflict flag; rename preserves `machine_id`; A
      attaches to B and reaches live overview within the ≤ 3 s budget; a `read`/`send` round trip
      returns identical payloads locally and remotely; a device untrusted on B is refused with the
      actionable message and then succeeds after the grant command; B's death mid-attach leaves A's
      local pane untouched and the failure message carries last-seen.
- [ ] Determinism and speed: fixed injected clock, temp dirs, port 0, poll-with-deadline instead of
      sleeps > 250 ms; green on repeated runs and under 60 s on the dev box, cheap enough for every
      commit.
- [ ] Honest skips, never silent passes: if a required soft-dependency API (relay durable inbox or
      presence push) is absent, the dependent assertion prints a loud SKIP naming the missing API, and
      the slice exits 0 only when every non-skipped assertion passed; the summary line prints
      `N passed, M skipped, K failed`.
- [ ] Evidence: `--interactive-evidence` (the T-0015 flag name) writes to `.loop/evidence/T-0047/` the
      runner transcript, each side's `arreo machines list --json`, the attach transcript, and the
      deny→grant→attach sequence.
- [ ] Chaos case: `xtask/src/chaos/mesh_reconnect.rs` kills B's daemon and its relay link mid-attach in
      a loop and asserts no panic in A, bounded jittered reconnect attempts, and zero cross-machine
      contamination; it runs inside the mesh slice and stays individually invocable like the other
      chaos cases.

## Notes

`xtask` only — no protocol or daemon code changes; the slice exists to fail loudly when the mesh
regresses and to produce the number T-0045's `cross_machine_attach_ms` budget row needs (written into
`perf-budget.toml` by this slice). Reuses existing conventions (T-0008's harness/assert style, T-0015's
evidence flag, T-0019's chaos wiring) rather than inventing a third test harness. Rejected: a
Docker-compose two-machine setup (heavy, non-hermetic, unavailable on CI macOS/Windows) and asserting
the mesh in unit tests over in-process fakes (would skip the shipped socket/relay paths — the exact gap
this slice closes). Soft dependencies: the relay durable inbox and presence push (separate Phase 2
tasks) supply the queued-message assertion; until they land it is a named skip. Honest gaps: loopback
proves protocol and state semantics, not WAN/NAT behaviour; real Pi5 hardware and mobile clients are out
of scope (Phase 3), and < 60 s is a dev-box number, not a CI SLA.

## Verification

```console
cargo xtask e2e --slice mesh
cargo xtask e2e --slice mesh --interactive-evidence
```
