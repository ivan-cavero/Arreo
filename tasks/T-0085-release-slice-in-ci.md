---
id: T-0085
title: The release slice in CI — matrix, PR/nightly split, and the pre-publish gate
phase: 2
priority: 3
status: proposed
depends_on: [T-0042, T-0063]
scope:
  - .github/workflows/ci.yml
  - .github/workflows/nightly-bench.yml
  - .github/workflows/release.yml
  - docs/release.md
verify:
  - cargo xtask e2e --slice release
  - cargo xtask e2e --slice release --chain
---

## Goal

T-0042's deferred half: run the release slice on the three OSes and wire the
tag-triggered release job to it, so "a release is not a release if any OS is red"
is mechanical rather than a promise. The slice itself landed (T-0042, hermetic
half); this is the CI wiring.

**Why it is its own task, and why it is blocked on T-0063.** The files it edits —
`.github/workflows/ci.yml` and `nightly-bench.yml` — are the user's in-flight work
during T-0063 (CI never-green); the loop held them rather than editing under the
user's hands. This task is the moment to fold the release rows in, once that
settles. `release.yml` is in scope too: T-0036 built the tag job and T-0037 gave
it the channel index; the battery must run **before** publishing.

## Acceptance criteria

- [ ] `ci.yml` gains the release rows on the existing runners: `--slice release`
      and `--slice update` on ubuntu/macos/windows, `--slice handoff` on
      ubuntu/macos, `--case windows-deferred` on windows — each a named step like
      the slices already there, and each with the `-s` flag so the OS is legible
      in the log.
- [ ] The **Windows leg runs the real `Handoff::Deferred` branch** (`--slice
      release --chain --case windows-deferred`) — the one thing the Linux dev box
      cannot execute, and therefore the leg that proves T-0039's rule rather than
      the Unix-observable instance of it.
- [ ] `nightly-bench.yml` gains `--slice release --chain` beside `bench`: PRs run
      the fast cases (verify/refuse, client swap, deferred refusal — seconds
      each); nightly runs the chain. The split is recorded in the workflow
      comments **and** `docs/release.md`, with the reason (the chain's two
      handoffs are seconds each, so the full battery stays inside the < 5 min e2e
      budget).
- [ ] `release.yml`'s tag job runs the same battery on the three targets
      **before** it publishes artifacts: a red slice blocks the release, and the
      job fails rather than publishing with a warning.
- [ ] The alert-ordering stage's SKIP on a non-delegated runner is visible in the
      CI summary as a skip (never counted as a pass) — the ubuntu leg is where it
      becomes a real assertion, since GitHub's ubuntu runners permit cgroup
      delegation (the enforcement slice already relies on this).
- [ ] Evidence: the first green CI run's log URLs per OS under
      `.loop/evidence/T-0085/`, plus the local `--slice release --chain`
      transcript for the baseline.

## Notes

- The exact YAML the worker prepared is in the T-0042 worker's report (the
  `ci_text` field); it is a starting point, not a contract — the CI file has moved
  since (T-0063) and the rows must fit whatever shape it has then.
- Rejected: putting `--chain` on every PR. It builds the release daemon and runs
  two handoffs — a minute of CI time per push for coverage the nightly already
  gives, on a change surface (the update path) that most PRs do not touch.
