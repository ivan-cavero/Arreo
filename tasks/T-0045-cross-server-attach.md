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

- [x] Resolution and connection: `arreo attach [--machine <name>] [<pane>]` resolves the name through
      T-0043's directory (never an IP/port/SSH target), then opens the same Noise-KK session a local
      client uses. Unknown machine exits 3, and nothing is ever dialed by address from argv.
      `--link auto|relay` are accepted; **`lan-direct` is refused with "not implemented"** rather than
      silently dialing the relay — LAN discovery is its own piece of work, and the directory carries
      no addresses. (`auto` therefore means the relay today; the flag exists so the default does not
      have to change later.)
- [x] One code path, both roles: the client is one implementation in `arreo-core::mesh::session`,
      used by the TUI, the CLI's `--machine` and a daemon — and the daemon-as-client claim is a test
      (`a_daemon_reaches_another_machines_pane_through_the_same_client`: A's daemon opens a session to B
      through the relay, B serves it as a peer, and A logs the answer).
      The "identical script through both paths and compare transcripts" form was **not** the test that
      landed: the CLI and the daemon run the same `Client`, so a transcript comparison would assert
      that one function equals itself. What the test asserts instead is the *observable* each path
      produces — the pane's line arrives over the CLI path, and the daemon path sees B's pane count.
- [ ] Identical semantics remotely: snapshot on attach, then deltas; `read`, `send`, `wait` and
      `metrics` behave as locally — a conformance test replays one scripted sequence against a local
      pane and a remote pane and shows equal payloads modulo pane ids and timestamps.
- [ ] Observability: remote panes appear with the same fields as local ones (state, RAM, machine name,
      link path), and a remote `question` state surfaces in the local sidebar with its payload — no
      degraded second-class display for remote panes.
- [x] Trust is the target's call: B authorizes the device itself (T-0046), and a refusal arrives as
      exit 5 carrying the peer's own message — which names the machine and the exact granting command,
      passed through unchanged rather than reworded. A never brokers trust; there is no auto-extend
      (ADR 0019 rejects it as the convenience that would void the model).
- [x] Unreachable is fast and honest: exit 4, within the §5 10 s row (the client's budget is now
      3 attempts × 3 s = 9 s), with the message carrying the directory's presence and the relay's own
      `last seen` timestamp plus the link tried. A machine the directory calls offline is refused
      **before any dial**. Per-machine jittered backoff and "a B failure must not disturb A's local
      panes" remain **T-0047's** (they need the two-daemon slice to observe; the backoff itself is
      T-0050's `backoff_delay`, already jittered per attempt).
- [ ] Node isolation: killing B's link mid-attach leaves A's local session and any C session untouched
      (per-machine independent reconnect, §3.7 failure isolation) — covered as a chaos case driven by
      the mesh slice (T-0047) and recorded in `.loop/evidence/T-0045/`.
- [ ] **Moved to T-0047**: the latency budget (`cross_machine_attach_ms`) is written by that slice,
      as its own criterion already says.

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

## The dial key landed (2026-09-11, second attempt)

The design recorded above is now built and verified. What changed:

- **`MachineRow.daemon_key`** — the key a peer dials, in the row. `export_rows` /
  `parse_export` carry it as an eighth column, and an export from before it existed still
  parses (seven columns) with the row reading as "not routable", which is what it is.
- **The relay writes it from the authenticated session** (`Session.public_key`, kept past
  the handshake for exactly this). A machine cannot advertise a route it does not hold;
  proved by mutation — substituting the request-carried key turns the assertion red.
- **`arreo attach --machine <name> [<pane>]`**, resolving through the directory: unknown
  name is exit 3 (listing what the account has), a row with no dial key is exit 4, a machine
  the directory calls offline is exit 4 **before any dial**, and a refusal by the target's
  trust ledger is exit 5 carrying the peer's own message (which names the granting command).
- **The handshake budget is now 3 × 3 s**, so a machine that does not answer fails inside
  §5's 10 s row instead of the 12 s the old 3 × 4 s would have taken.
- **The failure message carries the directory's view**: presence and the relay's own
  `last seen` timestamp. The *timestamp*, not a computed age, because `last_seen_ms` is on
  the relay's clock and mixing two clocks is exactly what the interesting case looks like
  (a clock seam made this visible: presence said `stale` while a client-computed age said
  `0s ago`, which would have been a lie in an operator's message).

Evidence: `.loop/evidence/T-0045/transcript.txt` (a hand-driven three-process run: the row,
the key it holds, the attach printing the remote pane's line) and four acceptance tests, each
with a real relay, a real daemon and a real CLI process.

## A real footgun found while testing: a CLI session displaces the daemon's

The tests initially failed for a reason that had nothing to do with the code under test and
everything to do with how one is written:

> **A CLI verb that talks to the relay uses the machine's own device key, and the relay keeps
> one live session per device — so polling `machines list` *on a machine whose daemon is
> connected* displaces that daemon from the routing table.** The daemon's session is still
> open and it never learns; the machine simply stops being reachable by name until it
> reconnects.

That is a real operator footgun, not a test artifact: running `arreo machines list` on a
daemon-hosting box (or `arreo attach` from it, or any future relay-touching verb) makes that
machine unreachable until its daemon reconnects on its own schedule. It is filed as **T-0060**
with the repro, because fixing it is relay session bookkeeping and belongs in its own task
rather than bolted onto this one. The tests here avoid it by observing a machine's
registration *from its own log* (`await_registration`) instead of dialing as it — which is why
that helper exists and is documented rather than being a mystery in the harness.

## What is still open

- **Criterion 3, conformance**: `read`, `send`, `wait` and `metrics` do not take `--machine`
  yet, so "equal payloads locally and remotely" can only be asserted end to end once they do.
  The shape is the same resolution this verb uses (`remote::resolve` → `Target`), which is why
  it was worth building the resolution as a reusable function rather than inside the verb.
- **Criterion 4, observability**: remote panes with the same fields as local ones, and a
  remote `question` surfacing in the sidebar — a TUI change on top of the shared client.
- **Criteria 7–8** moved to T-0047 as recorded above.

## Verification

```console
cargo test -p arreo-cli --test remote_machine
cargo xtask e2e --slice mesh
```
