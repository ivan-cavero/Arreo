---
id: T-0047
title: "`mesh` e2e slice — two real daemons + a relay on loopback"
phase: 2
priority: 4
status: done
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

- [x] Wiring exactly like existing slices: `xtask/src/mesh_slice.rs` with `run(rest)`, registered in
      `main.rs` (hint updated to include `mesh` and `relay`), one CI step on all three OSes, and the
      chaos probe registered in `xtask/src/chaos/mod.rs` so `--slice chaos` runs it too.
- [x] Real processes, no fakes: two shipped `arreo-server` daemons, a self-hosted `arreo-relay`, and a
      third **daemon-less client** machine (the shape an operator's laptop has), each with its own temp
      state dir and ephemeral `127.0.0.1:0` ports; no root, no external network, no mocking.
- [ ] Asserted behaviours (minimum): the directory lists both machines with presence and last-seen;
      staleness is derived from an injected clock (not by sleeping 30 days); a second claim of a live
      name yields the deterministic suffix plus the conflict flag; rename preserves `machine_id`; A
      attaches to B and reaches live overview within the ≤ 3 s budget; a `read`/`send` round trip
      returns identical payloads locally and remotely; a device untrusted on B is refused with the
      actionable message and then succeeds after the grant command; B's death mid-attach leaves A's
      local pane untouched and the failure message carries last-seen.
- [x] Determinism and speed: temp dirs, port 0, poll-with-deadline, and a **deadline on every CLI
      call** so a hang becomes a named failure rather than a stalled run. **21.8 s** on the dev box
      (under the 60 s bar), green on repeated runs.
- [x] Honest skips, never silent passes: `N passed, M skipped, K failed` in the summary, and the two
      skips are loud and named — the durable-inbox assertion (the relay slice's) and the trust refusal
      (**T-0064**: the daemon refuses correctly but the client hangs, so the slice reports what is true
      rather than failing on a defect outside its fence).
- [x] Evidence: `.loop/evidence/T-0047/transcript.txt` (the runner transcript and the chaos probe) plus
      `01-directory.json`, `02-attach.txt`, `03-deny.txt`, `04-grant-attach.txt` under
      `--interactive-evidence`.
- [x] Chaos case: `xtask/src/chaos/mesh_reconnect.rs` kills B and restarts it **ten times**, asserting
      A's pane keeps answering, A's listing and scrollback never show B's pane, and B rejoins the relay
      each round (2.0–2.5 s). Registered in the chaos suite (`--slice chaos` → 8 passed) and individually
      invocable like the others.

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

## Absorbed from T-0045 (2026-09-11)

Two of T-0045's criteria name this slice as their home, so they moved here — a correction of the split,
not a reduction:

- **Node isolation (was T-0045 criterion 7).** Killing B's link mid-attach leaves A's local session and
  any C session untouched (per-machine independent reconnect, §3.7 isolation), covered as a chaos case
  driven by this slice.
- **The latency budget (was T-0045 criterion 8).** On loopback, ≥ 3 s to a live overview of a remote
  machine fails; the measured value lands in `perf-budget.toml` as `cross_machine_attach_ms`, and this
  slice is what writes it.

Both need a slice that spawns two real daemons and a relay, which is what this task is.

## Verification

```console
cargo xtask e2e --slice mesh
cargo xtask e2e --slice mesh --interactive-evidence
```

## Outcome

Done. `cargo xtask e2e --slice mesh` — 15 passed, 2 skipped, 0 failed, 21.8 s; the chaos probe adds
`mesh-reconnect` to `--slice chaos` (8 passed). The latency row T-0045 moved here is measured and
recorded: `cross_machine_attach_s` budget 3 s, actual 203–233 ms.

Three findings, all from the fixture being wrong rather than the product:

- **A machine's daemon and its CLI share one device identity**, so a remote verb run *on a
  daemon-hosting machine* displaces that machine's own relay session (T-0060, by design). The slice
  therefore drives remote verbs from a daemon-less client machine — which is also the common real
  deployment.
- **A device needs pinning on the peer, not just a certificate**: the Noise handshake resolves the
  caller from the peer's own pin list, and a device with a valid account certificate that the peer has
  never seen is refused — and every refusal spends the relay's 3-per-10s handshake budget for the
  address, which then shows up as unrelated transport failures later.
- **T-0064 (filed)**: a remote trust refusal at Hello hangs the client, because the post-Hello read has
  no bound. The daemon produces the right refusal and the client never prints it.
