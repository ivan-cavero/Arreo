---
id: T-0125
title: A real-daemon FFI slice — the check the battery cannot fake
phase: 3
priority: 2
status: proposed
depends_on: [T-0114]
scope:
  - xtask/src/ffi_slice.rs
  - xtask/src/main.rs
  - crates/arreo-core-ffi/tests/**
  - docs/mobile.md
  - .loop/evidence/T-0125/**
verify:
  - cargo xtask e2e --slice ffi
  - cargo test --workspace
---

## Why this exists

T-0114 shipped a **green, wrong** tree: 936 tests, 14/14 slices, bench 6/6, vet/deny/audit all
passing — and the feature was one-shot against a real daemon. A security review caught it; the
battery could not, because every check the battery runs is either a unit test or a fixture, and
**the fixture was not a daemon**. It closed its session after each answer, which forced the
reconnect path and hid a failure that any real machine produces on the second read.

The proof that found it lives in a scratch crate under `target/` (gitignored): a probe that
spawns a real `arreo-relay` and a real `arreo-server`, pairs a viewer, spawns a real pane and
reads its metrics **twice through the exported FFI surface**, then asserts a wrong-but-valid key
is refused. Its source and one run are captured in `.loop/evidence/T-0114/`, but a captured
artifact is not a gate: nothing runs it, and the next person to touch this path gets no warning.

This task makes it durable. It is the same judgment as T-0039's deferred case and the
handoff-abort slice: a property that only real processes exhibit belongs in an xtask slice, not
in a fixture.

## Why it is worth a slice of its own

- The mobile core's *only* remote read lives on this path today, and T-0115 (the act door) will
  take the same conversation. A regression here is silent until a phone is offline-in-hand.
- The failure mode is not exotic: it is "the daemon keeps its session open after a verb", which is
  the daemon's ordinary behaviour. Any future FFI remote verb inherits the trap.
- The fixture cannot reteach the lesson. `serve_metrics_daemon` now models a real daemon because a
  human rewrote it after being told; nothing stops it drifting back, and only a real process
  notices when it does.

## Acceptance criteria

- [ ] `cargo xtask e2e --slice ffi` exists, registered in `xtask/src/main.rs`, and drives the
      **real binaries**: spawn `arreo-relay serve` and `arreo-server`, pair a device as a viewer
      through the real CLI, spawn a pane, dial through the exported FFI surface, and read metrics
      **at least twice on one session**. The probe in `.loop/evidence/T-0114/real-daemon-probe.rs`
      is the reference implementation — reuse its scaffolding rather than re-deriving it (the
      relay's account registration wants the *public* root key hex, and the certificate file is
      MessagePack bytes, not text).
- [ ] It **fails on the pre-fix shape**: reverting the conversation cache to a per-call handshake
      must redden it (T-0114's M1' was exactly this, on real processes —
      `read #2 FAIL — the SECOND read failed: handshake took longer than 10s`). Paste that red in
      the evidence.
- [ ] A **wrong-but-valid** pinned key is refused, and the check is load-bearing: ignoring
      `server_key` must redden it. (T-0114's M2 showed the first version of that assertion was
      weak — a served read also fails later on a dead channel — so assert on the *positive*
      symptom: the pane's data must not come back.)
- [ ] An **empty window is an answer**: a pane with no history yet returns an empty series with
      `downshifted`, not an error, asserted against the real daemon.
- [ ] The slice SKIPs rather than fails when the binaries or the relay cannot run, naming what is
      missing — the `check-targets` pattern. It must never report PASS without having read real
      metrics (assert the rows are non-empty and the step is the tier the machine served).
- [ ] `docs/mobile.md` points at the slice, and `AGENTS.md`'s command list gains it.
- [ ] No `arreo-server`/`arreo-relay` code is linked into the harness (AGPL — spawn the binaries,
      as the CLI's own real-process tests do).

## Notes

- Consider whether `--slice ffi` should also cover T-0115's act door when it lands; if the slice is
  already spawning a machine + pane + viewer, an act is one more verb on the same conversation and
  the marginal cost is a few lines.
- The probe's rough edges are recorded in `.loop/evidence/T-0114/` so the slice does not rediscover
  them.
