---
id: T-0042
title: Release/update e2e slice — one command, three OSes, real artifacts
phase: 2
priority: 3
status: proposed
depends_on: [T-0036, T-0037, T-0038, T-0039, T-0040, T-0041]
scope:
  - xtask/src/release_slice.rs
  - xtask/src/main.rs
  - xtask/src/update_slice.rs
  - .github/workflows/release.yml
  - .github/workflows/ci.yml
  - .github/workflows/nightly-bench.yml
  - docs/release.md
  - .loop/evidence/T-0042/**
---

## Goal

The T-0015/T-0016 precedent applied to releases: the update story gets its own executable
referee. `--slice release` proves sign → verify → refuse on real artifacts, the chained case
drives one whole update through a local channel, and the CI matrix runs it on Linux, macOS and
Windows — making §3.11's "a release is not a release if any OS is red" mechanical.

## Acceptance criteria

- [ ] `cargo xtask e2e --slice release`: generates a throwaway minisign keypair in a temp dir
      (never committed, never reused), signs both a fixture and a real
      `cargo build --release` daemon binary, verifies both against the generated public key,
      then flips one byte → asserts a refusal naming the artifact and a non-zero exit.
- [ ] `--slice release --chain` runs the whole story in one command against a `file://` channel
      in a temp dir: index → verify → client atomic swap → server handoff (0 panes, then 8 panes
      with output in flight) → deferred path (forced on Unix by flag to exercise T-0039's rule)
      → metrics history query → alert ordering. One transcript, one pass/fail line per stage.
- [ ] The channel is transport-agnostic by construction, and the slice proves it: index parsing,
      artifact naming and verification share the single code path an `https` base URL uses (only
      the fetcher differs), so a green slice exercises the real path and not a lookalike.
- [ ] CI matrix: `--slice release` + `--slice update` on ubuntu/macos/windows,
      `--slice handoff` on ubuntu/macos, `--case windows-deferred` on windows, with the
      tag-triggered release workflow running the same battery *before* publishing artifacts —
      a red slice blocks the release.
- [ ] PR vs nightly split recorded in the workflow comments and `docs/release.md`: PRs run the
      fast cases (verify/refuse, client swap, deferred refusal), nightly runs `--chain` plus
      `bench`. Handoff under 8 panes is seconds, so the full battery stays inside the < 5 min
      e2e budget.
- [ ] Evidence under `.loop/evidence/T-0042/`: one transcript per OS, the tampered-artifact
      refusal output, the chain transcript with the handoff marker-continuity proof, and an
      index naming which artifact backs each claim.
- [ ] Nothing in the slice needs network, a published GitHub release or a real signing secret
      — hermetic, deterministic, re-runnable on the dev box.
- [ ] Partial landing is honest: if T-0039 has not landed, the windows-deferred case reports a
      loud skip with the owning task named (T-0019's no-delegation precedent) and the Unix
      chain still runs; a skip is never counted as a pass in the CI summary.

## Notes

- The other tasks in this slice each wire their own slice (`update`, `handoff`, the
  `enforcement` extension, T-0015's `tui` case); this task owns the `release` slice, the CI
  matrix and the chain — composition is where update bugs actually live (a verified binary
  nobody can swap, a swap that drops a resume token, a handoff that forgets the audit row).
- The `file://` channel is a deliberate seam, not a test double: self-hosted users mirroring
  releases behind a firewall is a real Phase-5 flow, and it keeps CI offline-clean.
- Rejected: pointing the slice at the real release CDN — flaky, network-dependent, and it would
  require publishing a release to test publishing a release. Publishing stays cargo-dist on
  tags, gated by these slices.
- Honest gap: no Apple notarization or Authenticode proof (no paid identity yet, per T-0036);
  the evidence index says so rather than letting a green chain imply a signed-installer claim.

## Verification

```console
cargo xtask e2e --slice release
cargo xtask e2e --slice release --chain
```
