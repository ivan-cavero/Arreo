---
id: T-0086
title: Harness config sync over the mesh — deltas between real machines, conflict copies, merge hazards
phase: 4
priority: 3
status: proposed
depends_on: [T-0083, T-0047]
scope:
  - crates/arreo-core/src/sync/**
  - crates/arreo-core/src/mesh/**
  - crates/arreo-core/src/proto/**
  - crates/arreo-server/src/**
  - crates/arreo-cli/src/**
  - xtask/src/mesh_slice.rs
  - docs/harness-centralization.md
  - .loop/evidence/T-0086/**
verify:
  - cargo xtask e2e --slice mesh
  - cargo test --workspace
---

## Goal

The transport half of ROADMAP §3.8, split from T-0083 (whose local half — presets, version
vectors, history, revert, the keychain bridge — ships the mechanism and proves it on two
isolated roots). This is the part that needs the relay fabric: a *real* delta exchange between
two machines, and the merge hazards that only appear when two machines edit the same file.

## Acceptance criteria

- [ ] **The delta travels.** A version vector per synced file (machine → counter), exchanged
      with a pinned peer over the existing mesh session — no new relay verb if one of the
      existing ones carries a blob; if a verb is added, it keeps the N−1 window (the compat
      slice is the referee) and is documented in `docs/relay-protocol.md`.
- [ ] **Keep-both, never last-writer-wins.** Concurrent edits to one file leave the loser as
      `<name>.conflict-<machine>-<ts>.<ext>` on both machines, with the notification naming
      which file and which machine — §3.8's rule, proven by editing the same file on two live
      machines and asserting both files exist on both sides.
- [ ] **The three merge hazards T-0075 measured, handled rather than discovered**: opencode
      reads `opencode.json` **and** `opencode.jsonc` (writing one while the sibling exists must
      refuse or reconcile — never silently diverge); JSONC comments must survive a merge or the
      merge must refuse; array keys (`plugin`) merge as a union rather than being replaced.
- [ ] **A delta is verified before it is written**: the payload is scanned with the same
      `scan_secrets`/`is_env_reference` the local path uses, and a payload that would put a
      literal secret on this machine is refused with the finding, not written.
- [ ] **`--slice mesh` extended** (not a second slice): two daemons on the existing relay
      fabric, the §3.8 owner case end to end across them (edit once on A → both machines
      converge → B resolves the reference from its own store, and a machine missing the secret
      says so by name), plus the conflict case. Evidence under `.loop/evidence/T-0086/`.
- [ ] `cargo xtask e2e --slice compat` still green (the window is the referee for any protocol
      change), and the mesh slice's existing checks unmodified.

## Design (planner, 2026-09-14 — the decision this task owes, made before implementation)

The transport is one **new protocol variant pair**, which is the mechanism ADR 0017 and
`proto/message.rs` prescribe: variants are appended, new fields are optional, and **a shape
that carries more is a new struct behind a new variant** (never an extended one — `rmp-serde`
encodes structs as positional arrays, so an old reader fails the whole frame on a longer
array; T-0079 measured it). So:

```rust
// appended at the end of `Message`, `SyncExchange` a new struct
Message::Sync { v: u32, exchange: SyncExchange },   // request: one file's payload
Message::SyncReply { v: u32, outcome: SyncOutcome } // reply: applied / conflict / refused
```

`SyncExchange` carries the **same `SyncPayload` JSON the local exchange already produces**
(serialized to bytes), and the receiver runs the **same `Engine::receive`** — which is the
point: all eight review fixes from T-0083 (digest, forged vector, unresolved reference,
format validation, LOCAL deny-list, sibling, conflict, history) are the code path, and a
networked form that could diverge from the tested local one is the defect this avoids.

### The identity is the one the session authenticates — and that is a change to T-0083

A version vector's key is *the machine that is the authority on that counter*. Today the key
is `payload.machine`, a **self-declared string**, and `receive` absorbs that machine's entry
(that was F2's fix: only the sender's own counter moves). Over a socket that is not enough:
any paired peer could send `machine: "beta"` and pin beta's counter, which is exactly the
attack F2's second half closed *within* one machine.

So: **the counter key becomes the authenticated device id.** The daemon's `Message::Sync`
arm takes `auth.device` (bound to the Noise handshake, not to anything the peer says) and
uses it as `payload.machine` before `receive` runs; a payload whose own claim disagrees is
refused rather than silently corrected. Rationale for the *device id* rather than the machine
name: the name is a display label the operator can change (`arreo machines rename`, T-0043),
and a rename would fork the counter into "old name" and "new name", making every peer see a
concurrent edit and produce a conflict copy for a machine that did nothing. A device id is
stable, unique, authenticated, and `dev_<hex>` — filesystem-safe, which matters because it
lands in the conflict copy's name (`merge::sanitize_machine` already guards that).

The local form keys by the same identity, so there is **one spelling** of "which machine
counts this": `Engine` takes the identity explicitly (its `MachineEnv` still supplies the
display name and the paths). A machine with no device identity refuses the verb by name
rather than falling back to a self-declared string — the fallback would be the two-spellings
bug this decision exists to prevent.

### Authorization and trust

The verb is gated as `Verb::Admin` (→ `Capability::Control` → Owner only): it *writes this
machine's configuration*, which is an administrative change to the machine, not pane
control. Adding the verb forces a decision in `role::required`'s exhaustive match — that is
the door, and the TUI's viewer-disabled rendering follows from the same table. The receiver
also re-verifies the payload (scan, digest, references) before writing: trust in the peer is
not trust in its bytes, and the LOCAL deny-list is enforced on arrival exactly as it is for a
local apply.

### What the wire does *not* carry

No secrets — the payload carries references, never values (T-0083's property, and the
keychain bridge is what resolves them at spawn, T-0087). No path: the receiver resolves the
destination from its own registry, never from the payload (the review's claim 1, which the
slice's hostile-payload probes now cover).

## Notes

- Inputs: T-0083's local mechanism and presets, `docs/harness-centralization.md` (per-file
  SYNC/LOCAL/PROJECT classification and the worked example), T-0075's verified hazards.
- Rejected: a central server for sync (the mesh is peer-to-peer by design; §3.8 says no central
  authority), and Syncthing-style folder sync (the same section forbids blanket folders —
  surprise overwrites).
- If the protocol decision turns out to be large (a new verb, a blob framing, a resume path),
  that decision is the ADR this task owes: write it as `specs/adr/NNNN-*.md` with the
  alternatives it rejected.
