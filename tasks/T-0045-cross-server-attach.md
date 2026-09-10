---
id: T-0045
title: Cross-server attach — attach to a pane on machine B from a client paired with A
phase: 2
priority: 3
status: proposed
depends_on: [T-0014, T-0043, T-0044]
scope:
  - crates/arreo-cli/src/remote.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-core/src/mesh/session.rs
  - crates/arreo-server/src/mesh/remote_client.rs
  - crates/arreo-cli/tests/remote_machine.rs
  - .loop/evidence/T-0045/**
---

## Goal

The §3.7 core scenario, proven end to end: on the VPS (`arreo attach --machine pi …`) you reach the
Pi's agents, and from the Pi you reach the VPS — same protocol, same snapshot/delta semantics, no SSH,
no IPs, no ports. It also pins down what "B is unreachable" means honestly, because that happens on
real networks.

## Acceptance criteria

- [ ] Resolution and connection: `arreo attach [--machine <name>] [<pane>]` resolves the name through
      T-0043's directory (never an IP/port/SSH target), then opens the same Noise-KK session a local
      client uses; `--link relay|lan-direct|auto` defaults to `auto` (LAN first). Unknown machine exits
      3, and nothing is ever dialed by address from argv.
- [ ] One code path, both roles: the same client function serves the CLI and the daemon's embedded
      client, so machine A's daemon can attach to B (server-as-client, §3.7) — asserted by a test that
      runs an identical read/send script through the CLI path and the daemon path and compares the two
      transcripts with ids normalized.
- [ ] Identical semantics remotely: snapshot on attach, then deltas; `read`, `send`, `wait` and
      `metrics` behave as locally — a conformance test replays one scripted sequence against a local
      pane and a remote pane and shows equal payloads modulo pane ids and timestamps.
- [ ] Observability: remote panes appear with the same fields as local ones (state, RAM, machine name,
      link path), and a remote `question` state surfaces in the local sidebar with its payload — no
      degraded second-class display for remote panes.
- [ ] Trust is the target's call: B authorizes the device itself (T-0046). A device trusted only on A
      is refused by B with exit 5 and a message naming B and the exact granting command; A never
      brokers trust, and there is no auto-extend on the first cross-machine attempt.
- [ ] B unreachable is fast and honest: attach or first-frame failure within ≤ 10 s (no indefinite
      hang), exit 4, message carrying directory presence and last-seen age plus the link(s) tried;
      retries use per-machine jittered backoff, and a B failure must not disturb A's local panes or
      another machine's session.
- [ ] Node isolation: killing B's link mid-attach leaves A's local session and any C session untouched
      (per-machine independent reconnect, §3.7 failure isolation) — covered as a chaos case driven by
      the mesh slice (T-0047) and recorded in `.loop/evidence/T-0045/`.
- [ ] Latency budget: on loopback, ≥ 3 s to a live overview of the remote machine fails (§5 row
      "Cross-machine attach"); the measured value lands in `perf-budget.toml` as
      `cross_machine_attach_ms`, written by the T-0047 slice rather than asserted by hand.

## Notes

Lives in `arreo-cli` (client verbs), `arreo-core/src/mesh/session.rs` (remote session over the
existing client protocol, reusing T-0014's message verbs) and a thin `arreo-server` embedded client so
the daemon-as-client claim is code, not prose. The transport belongs to the Phase 2 transport task
(soft dependency, separate file — this task consumes its client session API and dials only through
it). Rejected: a separate remote-only protocol (two things to secure and evolve), routing sessions
through A as a proxy (breaks per-machine E2E and makes A a trust broker), and "SSH under the hood"
convenience (the bookmark model §3.7 rejects). Honest gaps: interactive attach over a real WAN/NAT
path is out of scope (loopback and LAN-direct only) and the phone-side UX is Phase 3; if the relay
inbox lacks a drain API, `wait` on a reconnecting session degrades to attach-on-return with a clear
message instead of fake queued results.

## Verification

```console
cargo test -p arreo-cli --test remote_machine
cargo xtask e2e --slice mesh
```
