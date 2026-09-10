---
id: T-0035
title: AGPL boundary — the relay is linked by nothing, spoken to by anyone
phase: 2
priority: 3
status: proposed
depends_on: [T-0001, T-0029]
scope:
  - crates/arreo-relay/Cargo.toml
  - xtask/tests/workspace_deps.rs
  - REUSE.toml
  - CONTRIBUTING.md
  - docs/relay-protocol.md
  - docs/relay-deploy.md
  - .loop/evidence/T-0035/**
---

## Goal

ROADMAP §7 + §9: Apache-2.0 core, AGPL-3.0 relay — the boundary must be architecture a machine
checks, not a sentence everyone forgets. A differently licensed component must be able to talk to
the relay without contaminating either side, and no Apache crate may link the AGPL one.

## Acceptance criteria

- [ ] The rule lives where contributors read it (`CONTRIBUTING.md` + the `docs/relay-protocol.md`
      header): `arreo-relay` may depend on Apache first-party crates (one-way, today only
      `arreo-core`); **no Apache first-party crate may depend on `arreo-relay`**; other-licensed
      code interoperates over the documented protocol or by running the unmodified binary.
- [ ] Manifest truth first (a live bug this task fixes): `crates/arreo-relay/Cargo.toml` inherits
      `license.workspace = true` — the workspace default **Apache-2.0** — while `REUSE.toml` and
      `LICENSE-RELAY` say AGPL. The relay declares `license = "AGPL-3.0-or-later"` (the workspace
      default stays Apache) and the gate asserts each crate's declared license.
- [ ] Gate extending the T-0001 direction test in `xtask/tests/workspace_deps.rs` (no new tool;
      runs in `cargo test --workspace`): no crate but `xtask` (dev tooling, never shipped) depends
      on `arreo-relay`; every shipped crate is Apache and the relay is AGPL; `REUSE.toml` maps
      `crates/arreo-relay/**` to AGPL and names no path that does not exist — the stale
      `crates/arreo-relay-proto/**` entry is retired, since the protocol types are Apache and live
      in `arreo-core` (T-0029), which is what makes the protocol implementable from other code.
- [ ] Mutation control: adding `arreo-relay = { path = "../arreo-relay" }` to an Apache manifest
      fails the gate naming the crate and the line; the probe is reverted and the failing-then-
      passing transcript lands in `.loop/evidence/T-0035/`.
- [ ] The portable interface is real: `docs/relay-protocol.md` is versioned (`v1`) with framing,
      envelope fields, error codes and the metadata-only guarantee, and its implementer note says
      no AGPL code is needed — the test asserts the envelope types are reachable from the Apache
      `arreo-core` public API (the frames T-0023 carries).
- [ ] No convenience shortcut: `arreo relay serve` stays unimplemented and the gate keeps it that
      way (linking the relay into `arreo-cli` would relicense the CLI; an exec shim is a second
      name for one thing) — stated in `CONTRIBUTING.md` with the reason.
- [ ] `docs/relay-deploy.md` states the AGPL obligations honestly: self-hosting an unmodified relay
      triggers no source duty; modifying it and offering it over a network requires publishing the
      modified source (§13) — the §7 protection, written down.
- [ ] `cargo deny check` (T-0020) stays green with its GPL-family *dependency* ban untouched: the
      first-party relay is not a dependency license, so this task does not weaken the allow-list.
- [ ] Evidence `.loop/evidence/T-0035/`: gate output before/after the mutation, license fields read
      back from the manifests, the `REUSE.toml` diff, and the deny run.

## Notes

- Crates/deps: none added — a gate, a manifest field, a doc header, a REUSE correction; `xtask`
  reads manifests rather than adding a license scanner (`cargo-deny` stays the authority).
- Why the rule points one way: Apache code may enter an AGPL work, AGPL code may not enter an
  Apache work (the binary would be AGPL) — so the relay sits atop the first-party graph and the
  protocol is the only seam other licenses cross. Rejected: dual-licensing the relay (§7's
  protection dies) and moving the protocol into an AGPL crate (implementers inherit AGPL).
- Honest gaps: the gate reads manifests/REUSE textually (T-0001's mechanism) — it catches edges,
  not copy-pasted code; the Enterprise on-prem license (§7) is commercial, not code.

## Verification

```console
cargo test -p xtask --test workspace_deps
cargo test --workspace
cargo deny check
```
