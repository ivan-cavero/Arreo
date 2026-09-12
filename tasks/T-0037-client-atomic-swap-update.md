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

- [~] **→ T-0070** (the invariant, and the machinery that must satisfy it).
- [~] Split: the **stage → swap → re-exec → reattach** half is **T-0070**; the **read the
      channel index → verify (T-0036, fail closed)** half stays here.
- [~] **→ T-0070** (the slice, wired and proven there).
- [~] **→ T-0070.**
- [~] Split: the *staging* half (a failed write leaves the binary byte-identical) is **T-0070**;
      the *verification* half (a failed signature) stays here.
- [~] **→ T-0070.**
- [~] **→ T-0070.**
- [~] Split: `--check` (needs the channel) stays here; the package-manager/read-only path is
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
