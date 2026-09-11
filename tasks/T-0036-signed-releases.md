---
id: T-0036
title: Signed releases — minisign-signed artifacts with fail-closed verification
phase: 2
priority: 1
status: proposed
depends_on: [T-0020]
scope:
  - crates/arreo-core/src/update/**
  - crates/arreo-core/tests/update_verify.rs
  - crates/arreo-core/Cargo.toml
  - crates/arreo-cli/src/main.rs
  - xtask/src/package.rs
  - supply-chain/arreo.pub
  - .github/workflows/release.yml
  - .github/workflows/ci.yml
  - docs/release.md
---

## Goal

T-0020 shipped the cargo-dist skeleton and wrote "artifacts are UNSIGNED — never present
them as trusted". This closes that hole: every release artifact carries a signature from a key
pinned in this repo, and everything downstream verifies or refuses (ROADMAP §3.13
channels-and-trust, §4 supply chain) — no byte of an update is trusted without it.

## Acceptance criteria

- [ ] Trust decision recorded in `docs/release.md`: **minisign** (one offline ed25519 key,
      in-process verification, zero egress) over keyless Sigstore — Fulcio/Rekor need
      network + OIDC at update time, and the self-hosted AGPL tier verifies on hosts behind
      zero inbound ports, sometimes offline. Rejected alternative and reason written down.
- [ ] Public key committed at `supply-chain/arreo.pub` and embedded in the binary
      (`include_str!`); the secret key exists only as the `MINISIGN_SECRET_KEY` CI secret.
      No signing code in the workspace — verification only.
- [ ] Tag-triggered `.github/workflows/release.yml` builds the three targets, writes
      `SHA256SUMS`, signs every artifact **and** the manifest, and fails hard when any
      artifact lacks its `.minisig` or any signature fails verification against the
      committed key.
- [ ] `arreo_core::update::verify` returns typed errors per failure mode (`MissingSignature`,
      `UnknownKeyId`, `BadSignature`, `DigestMismatch`, `Io`) — no boolean, no partial
      success, no warn-and-continue branch.
- [ ] Tamper refusal proven by artifact: `cargo test -p arreo-core --test update_verify`
      flips one byte of a signed fixture and asserts `BadSignature` naming the file; the
      release job repeats it on the just-built artifacts (tampered copy →
      `arreo update verify` non-zero → job fails).
- [ ] `arreo update verify <path> [--sig <path>]` is the user door: artifact name, key id
      and digest on success; on failure it names what failed and offers no bypass flag.
- [ ] Rotation documented as a two-key trust set (current + next): a leaked key becomes a
      procedure, not an emergency release.
- [ ] The added verifier crate is vetted — `cargo vet check`, `cargo deny check` and
      `cargo audit` stay green (T-0020's merge gate), exemption/audit in the same change.

## Notes

- Where signatures live: release assets beside each binary plus the signed `SHA256SUMS`, which
  is the only trusted digest source (never the web page, never a filename); install scripts
  verify the manifest first and then verify what they install.
- Crates: verification lives in `arreo-core` so daemon, CLI and TUI share one door — a
  second verification path is a bug, not a feature. Dependency `minisign-verify` (pure Rust,
  no build script, no network). Rejected: `minisign` (signing/key handling the workspace
  deliberately keeps out), `sigstore`/`cosign` (network at update time), `ring`/`ed25519-dalek`
  (re-implementing minisign's format and losing `cargo dist sign` compatibility).
- Honest gap: minisign proves *our* authorship, not Apple notarization or Windows
  Authenticode; those are a separate paid-identity line item recorded in `docs/release.md`.
- The end-to-end slice that drives sign → verify → refuse from a local channel is T-0042;
  this task lands the verifier, the pinned key and the release job.

## Gate (recorded 2026-09-11, while selecting the next task)

**This task needs a human before it can start, and it is not a code problem.**

Three of its criteria are about a key that must exist in two places only one of which I can reach:

- `supply-chain/arreo.pub` must be a *real* public key — a placeholder would be worse than nothing,
  because the release job would verify against a key nobody holds and fail closed on every artifact
  (a green-looking gate that can never pass).
- The matching secret must be created by the key's owner as the `MINISIGN_SECRET_KEY` CI secret.
  Setting a repository secret needs GitHub credentials and, more importantly, key *custody*: the
  point of an offline signing key is that it is generated and held by the person who owns it, not by
  an agent that has been reading the repo.
- `docs/release.md`'s rotation procedure (current + next key) is a statement about who holds what,
  which is an operator's decision.

What is *not* gated, and what a later pass can do without the human: the verifier itself
(`arreo_core::update::verify` with its typed errors), the `arreo update verify` door, the
tamper-refusal test against a locally generated throwaway keypair, and the release workflow's
structure. The task is left whole rather than half-landed because its central claim — "every
artifact carries a signature from a key pinned in this repo" — is only true once the key is real,
and a verifier nobody can use is not a deliverable.

**Action for the operator:** generate the keypair offline (`minisign -G`), commit the public half to
`supply-chain/arreo.pub`, and add the secret half as `MINISIGN_SECRET_KEY`. The task is then
unblocked end to end.

## Verification

```console
cargo test -p arreo-core --test update_verify
cargo xtask package --dry-run
cargo vet check && cargo deny check && cargo audit
```
