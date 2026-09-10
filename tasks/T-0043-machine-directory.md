---
id: T-0043
title: Machine directory — relay-side registry of the account's machines
phase: 2
priority: 1
status: proposed
depends_on: [T-0018]
scope:
  - crates/arreo-core/src/mesh/directory.rs
  - crates/arreo-core/src/mesh/mod.rs
  - crates/arreo-relay/src/directory.rs
  - crates/arreo-relay/src/store.rs
  - crates/arreo-relay/tests/directory.rs
  - specs/adr/0009-machine-directory.md
  - .loop/evidence/T-0043/**
---

## Goal

Give the account a durable answer to "which machines exist and are they up" (§3.7): an account owns
machines with name, identity, presence and last-seen; the directory is metadata-only and lives at the
relay while names resolve everywhere else — the substrate `arreo machines` (T-0044), cross-server attach
(T-0045) and per-machine trust (T-0046) stand on, and two machines claiming one name must never lie.

## Acceptance criteria

- [ ] Schema + ownership: relay SQLite tables `account` / `machine` with `machine_id` (ed25519
      fingerprint), `name`, `name_key` (NFC + casefolded), `presence`, `last_seen`, `proto_version`,
      `tombstone_until`; `UNIQUE(account_id, name_key)`; no agent state, no device grants, no private
      keys — a schema test fails if such a column appears; `cargo test -p arreo-relay --test directory`.
- [ ] Joining is explicit, never ambient discovery: a machine enters only by completing the account join
      (single-use code, 5-min expiry, §4); LAN mDNS may resolve an address for a known `machine_id` but
      must never create or rename a row. `specs/adr/0009-machine-directory.md` records that decision and
      the rejected discovery-as-registry variant.
- [ ] Name rules deterministic: `[a-z0-9][a-z0-9-]{0,31}`, NFC + casefolded uniqueness; a second claim
      of a live name is admitted under a deterministic suffix (`workbox-2`) flagged `name_conflict` —
      never silently renamed, never rejected. Claims serialize in one transaction: N concurrent
      identical claims yield exactly one plain name and N−1 suffixed names, stable across runs.
- [ ] Rename is atomic and identity-preserving: never touches `machine_id`; a colliding rename is
      refused with the conflicting name and leaves the old name intact (no partial state). A removed
      name is a 30-day tombstone for the same `machine_id` (a returning machine keeps its name);
      another machine may take it only after expiry.
- [ ] Stale is detected, not guessed: presence comes from the relay's single presence rule (not
      re-derived here) — `online` when the 30 s heartbeat is ≤ 90 s, `offline` otherwise, `stale` when
      `last_seen` > 30 days; every listed row carries the age, and `arreo machines remove --stale`
      prunes exactly the stale set and is idempotent (a second run removes nothing).
- [ ] Consistency: add → rename → remove → re-add of the same `machine_id` leaves a directory whose
      sorted export round-trips byte-identically, with no tombstones orphaned past expiry.
      Evidence: `.loop/evidence/T-0043/directory-consistency.txt`.
- [ ] Each server keeps a read-only cache (name → `machine_id`/presence/last_seen, `as_of`) that can
      never add, rename or remove; divergence resolves relay-first on the next contact (rows stay
      labeled).

## Notes

Crates: `arreo-core` (directory types, no I/O), `arreo-relay` (durable store), `arreo-server` (read-only
cache) — §3.7 puts the directory at the relay while names must resolve with the relay unreachable, and
the self-hosted AGPL relay gets the identical feature (no managed-only registry); T-0018's rusqlite/WAL
patterns are reused rather than a second store style. Boundary agreed in flight:
`crates/arreo-relay/src/store.rs` is the single relay SQLite connection + migration owner (the relay
inbox task's device registry registers its migration there), and presence/staleness lives in the relay
presence component — this task consumes that rule and only fixes the thresholds above as its contract.
Rejected: mDNS as source of truth (a hostile home LAN must not inject or rename machines),
directory-in-server (one authority every machine would have to agree on), device grants here (T-0046
keeps them per-machine; the relay stays metadata-only). Soft dependency: account/root-key creation and
the pairing code come from the Phase 2 transport+pairing task (separate file) — this consumes
`account_id` + machine identity, not SPAKE2. Honest gaps: presence is relay-observed only (no NAT
probing in v1); a machine that never returns can only age out via the tombstone/stale policy.

## Verification

```console
cargo test -p arreo-relay --test directory
cargo clippy --workspace --all-targets -- -D warnings
```
