---
id: T-0048
title: OSS public-readiness — licenses, stranger path, contributor surface, history scan
phase: 2
priority: 3
status: proposed
depends_on: [T-0020, T-0022]
scope:
  - README.md
  - CONTRIBUTING.md
  - SECURITY.md
  - docs/**
  - REUSE.toml
  - LICENSE*
  - .github/**
  - xtask/src/release_check.rs
  - xtask/src/main.rs
  - supply-chain/**
  - .loop/evidence/T-0048/**
---

## Goal

Phase 2 ends with a public OSS launch, and Phase 1's exit line already asked for "OSS repo
public-ready" (§6): a stranger must be able to clone Arreo, run the battery and find a way to
contribute, with the license story (§7) and the honesty rule ("claims point to artifacts", §10.2)
airtight. This task prepares and proves that state; it does not decide to go public.

## Acceptance criteria

- [ ] License story verified mechanically: `LICENSE` (Apache-2.0), `LICENSE-RELAY` (AGPL-3.0) and
      `REUSE.toml` cover every tracked path (`reuse lint`, pinned, in CI); every crate's `Cargo.toml`
      license matches §7; no Apache-2.0 crate depends on the relay; `NOTICE.md` is accurate.
- [ ] Stranger path, executed not described: from a clean clone and an empty state dir, the README
      quickstart's exact commands reach a first pane and the battery runs (`cargo xtask e2e --slice api`)
      — transcript at `.loop/evidence/T-0048/stranger-tour.txt` with wall time and every prerequisite
      installed (`rust-toolchain.toml` pin resolved, nothing undocumented).
- [ ] Newcomer docs and contributor surface: a short tour doc explains what Arreo is and how
      daemon/client/socket fit without ROADMAP first; relative links in README/CONTRIBUTING/docs
      resolve; CONTRIBUTING's entry points each point to a real path with a real example, and no doc
      names a verb that does not exist.
- [ ] Public repo furniture: `.github/ISSUE_TEMPLATE/{bug,feature,adapter-request}.yml` and
      `.github/PULL_REQUEST_TEMPLATE.md` exist; `SECURITY.md` carries CONTRIBUTING §8's disclosure process.
- [ ] Secrets scan covers the whole history, not HEAD: pinned `gitleaks` with full-log options over all
      commits plus fixture/evidence directories, zero findings — or every finding rotated with a
      recorded history-rewrite decision; raw report at `.loop/evidence/T-0048/secrets-scan.txt`.
- [ ] Public-claims audit: every public claim maps to a shipped artifact — the "externally audited"
      badge, `v1.0.0`, `arreo.dev` install URLs and platform-support wording are true today or marked
      pre-launch; the edited-claims list is in evidence.
- [ ] Automated gate: `cargo xtask release-check --public` (registered in `xtask/src/main.rs`) runs
      fmt/clippy/test, `cargo vet`/`audit`/`deny`, the REUSE lint, the secret scan and the doc link
      check, printing PASS/FAIL per item, exiting non-zero on any FAIL, and wired as a CI step so
      readiness cannot drift after this task closes.
- [ ] **HUMAN GATE — not automatable, a human decision:** the handoff is the closing criterion:
      `.loop/evidence/T-0048/readiness.md` collects every criterion above with its artifact, the exact
      `gh repo edit --visibility public` command and the announcement checklist, then this task is set to
      `status: needs-human`; the loop never flips visibility, creates the org, or announces.

## Notes

Docs/CI/xtask scope on purpose — launch prep, not a feature. `REUSE.toml` and `LICENSE-RELAY` already
exist and `supply-chain/` holds T-0020's vet state, so this is verification plus disclosure surface, not
license re-creation; it extends T-0020's gates and T-0022's contributor story rather than duplicating
them. `release-check` is a new verb in the existing `xtask` (no new crate; the pinned secret scanner is
the only added tool, since the scan must be reproducible by a stranger on any OS). Rejected: gating the
launch on an external security audit (§4 defers that to pre-revenue) and "flip visibility now, fix docs
later". Honest gaps: cargo-dist artifacts stay skeleton-level (T-0020) and the managed relay is out of
scope, so its deployment docs stay a stub.

## Verification

```console
cargo xtask release-check --public
cargo test --workspace
```
