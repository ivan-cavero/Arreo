---
id: T-0045
title: Cross-server attach — attach to a pane on machine B from a client paired with A
phase: 2
priority: 3
status: in-progress
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

## Progress and findings (2026-09-11)

**Landed: the shared client (criterion 2, the "one code path" claim).** The daemon client that speaks
the protocol over either transport moved from `arreo-tui` to `arreo-core/src/mesh/session.rs`
(commit `4f79f4f`), because this task adds two callers — the CLI's `--machine` and a daemon attaching
to another machine — and `arreo-cli` may not depend on `arreo-tui`. The TUI re-exports it; its 18
tests pass untouched, which is what makes it a move rather than a rewrite.

**Found, and it is the blocker for criterion 1: a machine name is not dialable yet.** The directory
(keyed by `machine_id`, the machine's *root* key — T-0043) tells you *which machine* something is, not
how to reach it. Reaching it needs two other things:

1. **a device to route to** — the relay moves bytes between *device* ids, and a machine's daemon
   authenticates as a device (the one pairing created for it), a different key entirely;
2. **that device's public key** — the Noise handshake proves the peer holds the key we pinned
   (ADR 0011), so a client needs more than an id: it needs the key itself.

So a row has to carry the daemon's dial key, written by the relay from the certificate that
authenticated the session asserting the row (never self-reported). A prototype of exactly that was
built — `MachineRow.daemon_key`, a relay column written from `session.public_key`, and
`arreo attach --machine <name>` resolving through it to the existing client — and **reverted**, for two
reasons worth recording:

- The relay's own `the_schema_holds_directory_metadata_and_nothing_else` test pins the machine table's
  column list and forces the change to be a *decision*. Reading it showed the invariant is "no secrets,
  no agent state, no grants" — the account table already holds a public root key by the same argument —
  so a public dial key is admissible. Recorded here because the next attempt will meet that test too:
  it wants the reasoning, not a widened list.
- The end-to-end test then failed at the dial stage: the request reached the relay and the peer's id
  was right, but **the daemon authenticated as a different device key than the test had installed**, so
  nothing answered. That is a harness/plumbing question (which key does `arreo-server` load, versus
  which one `devices issue` writes into whose identity directory), not a design one — and it is not
  something to guess at, so the whole increment was reverted rather than committed unproven. An
  attach that "looks implemented" and cannot connect is precisely the failure this project keeps
  finding.

**Next attempt should start there:** make a two-machine harness whose daemon keys are unambiguous
(print what each process authenticated as, and assert it matches), then re-land the dial key, then the
`--machine` verb. The reverted prototype is described above in enough detail to rebuild in one sitting;
nothing was left in the tree.

**Criteria 7 and 8 belong to T-0047, which depends on this task.** Criterion 7 ("covered as a chaos
case driven by the mesh slice") and criterion 8 ("written by the T-0047 slice rather than asserted by
hand") both name that slice, and it is the referee for the whole mesh phase. Moving them is a
correction of the split, not a reduction: this task keeps resolution, semantics, observability and
trust (criteria 1–6), and the slice owns isolation-at-scale and the recorded budget row.

## Verification

```console
cargo test -p arreo-cli --test remote_machine
cargo xtask e2e --slice mesh
```
