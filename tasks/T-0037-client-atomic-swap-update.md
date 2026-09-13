---
id: T-0037
title: Client atomic swap — update in place, restart, reattach by resume token
phase: 2
priority: 2
status: proposed
depends_on: [T-0012, T-0013, T-0036]
scope:
  - crates/arreo-core/src/update/**
  - crates/arreo-cli/src/update.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-cli/tests/update.rs
  - crates/arreo-tui/src/main.rs
  - xtask/src/update_slice.rs
  - xtask/src/main.rs
  - perf-budget.toml
  - docs/release.md
  - .loop/evidence/T-0037/**
---

## Re-scope (2026-09-12): the swap moved to T-0070

Five of this task's eight criteria are about the **swap** (the invariant, the atomic rename,
crash-safety, concurrency, `--rollback`) and need nothing from the signing key. Three are about
the **channel** (read the index, verify, `--check`) and cannot start while T-0036 is human-gated
on key custody.

So the swap became **T-0070**, which can proceed now with an explicit local artifact source
(`arreo update --from <path>` — a real feature, and where a channel source will feed in). This
task keeps the channel half: the anonymous `arreo update` that requires a signature, `--check`,
and the release index. Until both land, `arreo update` with no `--from` refuses.

This is a correction of the split, not a reduction: nothing is dropped, and the criteria below
are annotated with which task owns them.

## Goal

The client half of §3.13: download → verify → atomic swap → restart → reattach through the
resume token. The daemon owns the agents, so this path must be *provably* incapable of touching
a PTY — T-0037 makes `arreo` and `arreo-tui` self-updating (server half T-0038, fallback T-0039).

## Acceptance criteria

**The channel half, re-written (2026-09-13).** The original eight criteria were split to
T-0070 (the swap) and T-0036 (the verifier); what remained — "read the channel index → verify
(fail closed) → fetch" — was never re-written as criteria. This is that re-write, and it is
startable now: the verifier exists (T-0036), the swap exists (T-0070), and the only missing
piece is the **fetch**, which is thin (a URL + the verifier). The channel is the repository's
GitHub Releases: `https://github.com/<owner>/<repo>/releases/latest/download/` — the same
artifacts the T-0036 release job publishes, so `--check` fetches what a real release would
produce, and an empty channel is reported honestly rather than as an error.

- [ ] **A channel URL is a configuration value**, not a constant in code: read from
      `ARREO_CHANNEL_URL` (or a `--channel` flag), defaulting to the repo's GitHub Releases
      `latest` URL. The fetch is transport-agnostic by construction — a URL is a URL, so
      `file://` and `https://` share every line of code except the fetcher; prove it by
      running `--check` against both.
- [ ] **`arreo update --check`** fetches the channel index, **verifies its signature against
      the pinned key (T-0036, fail closed — an unverifiable index is a refusal, never a
      warning)**, and reports the newest version + the artifact name. An **empty** channel is
      reported as "no releases yet" with exit 0 — the honest answer, not an error.
- [ ] **`arreo update` (anonymous, no `--from`)** fetches the newest artifact, verifies it,
      stages it, and **stops before the swap** — the swap is T-0070's `--from` machinery, and
      this task wires the fetch into it, so a verified artifact becomes a `--from`-equivalent
      without duplicating the install logic. A signature failure at any point refuses with the
      typed error naming the file.
- [ ] **The refusal is the same sentence the verifier uses** — `BadSignature`, `UnknownKeyId`,
      `DigestMismatch` — so an operator who sees a refusal can act on it without translating.
- [ ] **No network in tests**: the slice and tests use `file://` channels in a temp dir (the
      transport-agnostic proof is the point; a test that dials the real internet is a test that
      fails on a plane).
- [ ] `--check` and the anonymous update are exercised end to end in `xtask/src/update_slice.rs`
      (extend it — a second slice for one story is a defect), with frames/transcripts under
      `.loop/evidence/T-0037/`.

      channel index → verify (T-0036, fail closed)** half stays here.
      the *verification* half (a failed signature) stays here.
      **T-0070**.

## Notes

- The resume token is why a client restart is cheap: it lives in the XDG state dir
  (`.../arreo/resume.json`), is written *before* the swap and used after re-exec, and it is the
  same token a phone uses after a network hop (T-0013) — one mechanism, two clients.
- Swap mechanics differ per OS but the control flow is single: Unix `rename(2)`; Windows
  rename-away-then-rename-in, because a running image can be renamed but not deleted (`.prev`
  is dropped on the next successful start). Windows *server* updates are T-0039's story.
- Rejected: overwrite-in-place (a crash mid-write bricks the install) and "restart via the
  service manager" (that touches the daemon — the invariant above forbids it).
- Honest gap: Homebrew and `cargo install` own their binary path, so the updater prints the
  correct package-manager command rather than fighting it; and no unattended background install
  ships here (that needs Phase-5 channel policy) — `--check` is all this task ships.
- The `update` slice is wired in **T-0070** now (T-0042 runs it on the CI matrix and chains it
  into the release story).

## Verification

```console
cargo test -p arreo-cli --test update
cargo xtask e2e --slice update
```
