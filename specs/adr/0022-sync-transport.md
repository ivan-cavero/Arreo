# ADR 0022 — Syncing harness config between machines: an appended variant pair, and a counter keyed by the authenticated device

- **Status**: accepted (T-0086)
- **Supersedes**: nothing
- **Related**: ADR 0017 (the N−1 window and its append-only discipline), ADR 0019
  (per-machine trust), ROADMAP §3.8 (harness config sync)

## Context

§3.8's promise is *edit a provider once, and every machine has it*. T-0083 built the local
mechanism — presets, version vectors, history and revert, keep-both conflicts, a keychain
bridge, a reference-aware secret scan — and proved it on two isolated roots in one process.
What was missing was the network: two real machines, exchanging a file's delta over the
existing mesh.

Three questions had to be answered before a line of transport code, and each had a wrong
answer that looks reasonable:

1. **What travels?** A second wire format for a synced file would be a second implementation
   of the write path — and the local one carries eight security fixes from its review
   (digest verification, forged-vector refusal, unresolved-reference refusal, format
   validation, the LOCAL deny-list, the sibling gate, keep-both, history). A networked path
   that could drift from the tested one is exactly the defect the review found in the first
   place, one layer up.
2. **What is the counter keyed by?** A version vector's key is *the machine that is the
   authority on that counter*. Locally it was `payload.machine` — a self-declared string,
   which is safe when both halves are in one process and is not safe over a socket.
3. **Where does it sit in the protocol?** `proto/message.rs` and ADR 0017 constrain this
   tightly: variants are appended, never renumbered; new fields are optional; and a shape
   that carries more is a **new struct behind a new variant**, because `rmp-serde` encodes
   structs as positional arrays and an old reader fails the whole frame on a longer array
   (measured in T-0079).

## Decision

**One appended variant pair, carrying the local payload, with the counter keyed by the
authenticated device id.**

```rust
Message::Sync      { v: u32, exchange: SyncExchange }   // request
Message::SyncReply { v: u32, outcome: SyncOutcome }     // event
```

- `SyncExchange` carries the **same `SyncPayload` JSON** the local exchange produces, as a
  byte string, and the receiving daemon runs the **same `SyncEngine::receive`**. Every
  refusal the local path was hardened with is therefore the one code path, not a copy of it.
- **The counter key is `auth.device_id()`** — the identity the Noise handshake proved, not a
  string the peer chose. The daemon's arm **refuses** a payload whose `machine` disagrees with
  it, rather than silently rewriting the claim: a peer that lies about who it is is a finding
  for the operator, and correcting it would discard the only evidence.
- The local form keys by the same identity (`SyncEngine::new(…, identity)`, resolved from
  `identity/device.key`), so there is **one spelling** of "which machine counts this".
- The verb is `Verb::Sync` → `Capability::Control` → Owner only: it writes this machine's
  configuration. `role::required`'s exhaustive match forces that decision to be made
  explicitly, which is why it is a verb of its own rather than folded into `Admin` (a refusal
  reading "refusing Admin for dev_…" says nothing about what was attempted).
- **The wire carries no secret value and no path**: the payload holds `${ARREO_ENV:NAME}`
  references (resolved by the receiver's own keychain — T-0087 applies them at the PTY), and
  the destination is resolved from the receiver's own preset registry.

## Why this one

**Correctness.** Reusing the payload means the hardened path is the only path: a bug fixed in
`receive` is fixed for the wire, and a review of the local engine is a review of the networked
one. A second format would have needed its own eight fixes and its own review.

**Security.** The self-declared identity is the one field a hostile *paired* peer controls,
and it is exactly the field a version vector trusts. Keying by the authenticated device closes
the forgery class the engine already refuses *within* one machine (T-0083 review finding F2 —
"a payload may claim counters for any machine, including the receiving machine itself"),
reopened one layer up by the network. Refusing a disagreement rather than correcting it keeps
the attempt visible in the audit trail.

**Simplicity.** Two variants, two structs, one engine call, one audit action. The alternative
designs below each add a subsystem.

**Maintenance.** The device id is stable across renames, unique per machine, `dev_<hex>` (so
it is filesystem-safe in a conflict copy's name), and already the identity every other mesh
path authenticates. Nothing new has to be minted, rotated or stored.

## Alternatives rejected

| Alternative | Why not |
| --- | --- |
| **Extend an existing variant** (`Panes`, `Handoff`) with a `payload` field | ADR 0017's mechanism forbids it: `rmp-serde` encodes structs positionally, so an N−1 reader handed a longer array fails to decode the *whole frame*. T-0079 measured exactly this, which is why `PanesDetail` is a new variant rather than a bigger `PaneInfo`. |
| **Key the counter by the machine name** (what the local form did) | The name is a display label the operator can change (`arreo machines rename`, T-0043). A rename would fork one machine's counter into "old name" and "new name", making every peer see a concurrent edit and produce a conflict copy for a machine that did nothing. It is also self-declared, which is the forgery above. |
| **A separate file-transfer verb** (send bytes, let the peer apply) | A second write path, with none of the eight hardened refusals unless they are re-implemented — and then kept in step forever. The payload *is* the file-transfer shape; wrapping it in a second verb buys nothing. |
| **Fall back to the hostname when a machine has no device identity** | That is the two-spellings defect the decision exists to prevent, and it would silently make the counter renameable. A machine with no identity refuses by name and says how to fix it (`arreo pair`). |
| **Central server / Syncthing-style folder sync** | §3.8 rejects both explicitly: no central authority (the mesh is peer-to-peer), and no blanket folders (a surprise overwrite of a tree is not an opt-in per file). |

## Consequences

- The sync slice's stand-in machines now issue themselves a device identity
  (`xtask/src/sync_check.rs`), which is what a real machine has; conflict copies are named
  after the device rather than the display name, so an operator sees `dev_<hex>` in the file
  name and the display name in the sentence.
- `arreo sync` gained `--name` (this machine) alongside `--machine` (another machine, the
  spelling the rest of the CLI uses). The two flags mean different things and a caller that
  conflates them pushes to a peer when it meant to name itself — measured while integrating,
  and the reason the slice passes `--name`.
- A payload for a file the receiver has no preset for is refused rather than written: the
  registry is the receiver's, and two harnesses' configs are not interchangeable.
- The transport is one-way per invocation (`push --machine`). A pull or a full reconciliation
  is not built; a machine that has been offline catches up when its peer pushes, which is the
  §3.8 flow (the editing machine propagates).
