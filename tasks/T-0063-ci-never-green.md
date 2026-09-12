---
id: T-0063
title: CI has never passed — make the matrix green and keep it green
phase: 2
priority: 1
status: proposed
depends_on: [T-0062]
scope:
  - .github/workflows/ci.yml
  - crates/arreo-relay/src/pairing.rs
  - crates/arreo-relay/src/main.rs
  - xtask/src/release_check.rs
  - .gitleaks.toml
  - REUSE.toml
  - LICENSES/**
  - .loop/evidence/T-0063/**
---

## Ownership boundary (2026-09-12, evening)

`crates/arreo-core/src/pty/adopt.rs` and `crates/arreo-cli/tests/update_server.rs`
are **owned by T-0038 (worker HandoffStage2, in flight)** — this task never edits them.
This task owns the CI environment (tool installs), REUSE, the gitleaks verdict, and the
final green run, which therefore waits for T-0038 stage 2 to land (see Sequencing).

## Goal

The repository is **public since 2026-09-10** and **CI has failed on all 90 runs since
then** (verified via the GitHub API: `total_count: 90`, zero successes). Every public
claim about a green three-OS CI is therefore false today, and the repo's first impression
is a red badge. This task makes the matrix green and adds the guard that keeps it green.

## What is established (2026-09-12, by API + local reproduction)

Three legs, three different failures — not one cause:

1. **Windows (`build`, ~2 min): FIXED by T-0062.** `crates/arreo-relay/src/pairing.rs`
   imported `std::os::unix::net` at module scope with no `#[cfg(unix)]`. The crate could
   not compile for `x86_64-pc-windows-msvc` at all. Fix committed (`3490dee`); the remaining
   Windows question is whether the C deps (ring, libsqlite3-sys) build under MSVC, which
   only the runner can answer.
2. **macOS (`clippy`, 13 s): FIXED this turn (`08ca7db`).** CI installs `@stable`; the repo
   pins 1.98.0. Same compiler version, but clippy's lint set moves underneath it: two lints
   the pinned clippy never fired (`items_after_test_module`, `vec_init_then_push`, both in
   the new `xtask/src/release_check.rs`) broke the leg. Fixed and verified with both
   `cargo clippy` and `cargo +stable clippy` at zero warnings.
3. **Ubuntu (`test`, ~2 min, exit 101): LIKELY LOAD, UNCONFIRMED.** fmt, build and clippy
   all pass on this leg; `cargo test --workspace` fails. Local data point (2026-09-12):
   the full suite failed once with `a_stalled_peer_does_not_block_the_next_device` (a 5 s
   connect timeout while a stalled peer holds a slot — under a 55-target parallel suite,
   5 s can expire), then passed 3/3 in isolation and 472/0 on the next full run. That is
   the signature of a load-sensitive timing assertion, the same class removed in T-0060's
   turn — but without the CI log it is a hypothesis, not a diagnosis. CI job logs require
   admin rights ("Must have admin rights to Repository"), and there is no `gh` auth on
   this box, so confirmation needs a human with repo rights.

## Batch 2 (2026-09-12, evening — pasted runner logs, no inference needed)

`public-readiness` (`release-check --public`): **4 passed, 6 failed.**

- [PASS] fmt, clippy, license (Apache/AGPL split holds), links (65 links / 13 docs).
- [FAIL] `test`: `arreo-cli --test update_server`, 2 failures —
  `a_running_daemon_is_handed_over_to_the_new_binary` (new daemon exits 1 before
  takeover) and `a_candidate_that_fails_the_handoff_changes_nothing` (daemon never
  comes up). Both are T-0038 stage-2 code, owned by HandoffStage2 (see boundary above).
- [FAIL] `vet` / `audit` / `deny`: `no such command` — the `public-readiness` job
  installs gitleaks + reuse but never installs these three (the matrix's supply-chain
  job does). CI-workflow bug, owned by this task.
- [FAIL] `reuse`: project non-compliant (missing license texts in `LICENSES/` and/or
  missing SPDX tags — likely the new T-0072…T-0076 task files and evidence). Owned by
  this task: `reuse download --all` + tags, then green.
- [FAIL] `secrets`: gitleaks 8.28.0, 133 commits, **1 leak**. Owned by this task to
  *identify* (`release-check --public --report-path <p>` keeps the deleted report):
  fixture/test → allowlist in `.gitleaks.toml` (value-scoped, negative-controlled);
  real secret → rotation outside the repo + recorded history-rewrite decision (the only
  sub-case that needs the human — see Notes).
- macOS leg: `arreo-core/src/pty/adopt.rs` does not compile — `SendFlags::NOSIGNAL`
  (Linux-only) and `ExitProbe::Pidfd` (variant cfg-gated, use-site not) from T-0038
  stage 2. Owned by HandoffStage2. Follow-up for this task's author: check why
  `check-targets` did not catch a macOS breakage, and record the answer.

## Acceptance criteria

- [ ] The ubuntu `test` failure is identified (from a runner with log access, or by
      reproducing the CI environment exactly) and fixed. The fix states what the failure
      was, not just that it is gone.
- [ ] All three legs pass on one run, on the current HEAD.
- [ ] The structural cause of the macOS failure is closed: CI installs the **pinned**
      toolchain (`rust-toolchain.toml`) instead of `@stable`, so "clippy is clean" means
      one thing everywhere. (One-line change: `dtolnay/rust-toolchain@stable` → the pinned
      channel. If `@stable` is kept deliberately, the reason is written here instead.)
- [ ] `cargo xtask release-check --public` passes on the runner (it now exists and CI runs
      it as `public-readiness`).
- [ ] The `xtask stubs` step (`cargo xtask e2e`, exit 0) is either kept green or removed —
      it failed the first ~19 runs because the `.cargo/config.toml` alias did not exist yet.
      A step that has never passed in 90 runs is either load-bearing or deleted.
- [ ] `public-readiness` installs its own supply-chain tools: `cargo-vet 0.10.0` +
      `cargo-deny 0.20.2` + `cargo-audit 0.22.0` (same pins as the matrix supply-chain
      job) before `release-check --public`, so vet/audit/deny run instead of failing
      with `no such command`.
- [ ] `reuse lint` (pinned 5.0.2) green on the runner: missing license texts downloaded,
      every new file tagged or covered by `REUSE.toml`.
- [ ] Gitleaks verdict recorded in evidence: the 1 leak identified by file+commit;
      allowlist entry with reason if fixture, rotation + history decision if real.
- [ ] Final green run on current HEAD: three matrix legs + `public-readiness` in one run.
      This criterion waits for T-0038 stage 2 (adopt.rs macOS + update_server); all
      criteria above land independently first.

## Sequencing

Land in this order, one push per line where possible: (1) tool installs in
`public-readiness`, (2) REUSE, (3) gitleaks verdict, (4) final green run after T-0038.
Never edit `adopt.rs` / `update_server.rs` from this task — that is HandoffStage2's
tree and a collision loses work.

## Notes

- **Why p1:** the repo is public and every CI badge is red. Nothing in T-0048's
  launch-readiness story is true while this is.
- The ubuntu failure may be environmental (runner disk, memory, timing) or a real test bug
  that only fires under CI parallelism. The first step is reading the log with admin
  access — a human with repo rights can do in one minute what took an hour of inference.
- Do not "fix" this by deleting tests or weakening assertions. A test that fails on CI and
  passes locally is evidence about the test's assumptions (timing, parallelism, paths),
  and the fix names the assumption.

## Verification

```console
# On the runner, not here: the whole point is the matrix.
gh run watch <run-id>  # or the Actions tab: three green legs + public-readiness
```
