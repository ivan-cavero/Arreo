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
  - .loop/evidence/T-0063/**
---

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
