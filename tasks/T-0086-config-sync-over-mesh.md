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

## Notes

- Inputs: T-0083's local mechanism and presets, `docs/harness-centralization.md` (per-file
  SYNC/LOCAL/PROJECT classification and the worked example), T-0075's verified hazards.
- Rejected: a central server for sync (the mesh is peer-to-peer by design; §3.8 says no central
  authority), and Syncthing-style folder sync (the same section forbids blanket folders —
  surprise overwrites).
- If the protocol decision turns out to be large (a new verb, a blob framing, a resume path),
  that decision is the ADR this task owes: write it as `specs/adr/NNNN-*.md` with the
  alternatives it rejected.
