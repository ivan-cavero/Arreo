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

## Goal

The client half of §3.13: download → verify → atomic swap → restart → reattach through the
resume token. The daemon owns the agents, so this path must be *provably* incapable of touching
a PTY — T-0037 makes `arreo` and `arreo-tui` self-updating (server half T-0038, fallback T-0039).

## Acceptance criteria

- [ ] Invariant stated in code and in `docs/release.md`, enforced by construction: **the
      client update path never signals, reaps, restarts or stops a PTY-bearing process, and
      never stops the daemon** — it may only touch the CLI/TUI binary path and the resume store.
- [ ] `arreo update` end-to-end: read the channel index → verify (T-0036, fail closed) → stage
      beside the running binary on the same filesystem → atomic `rename(2)` swap keeping the
      previous binary as `.prev` → re-exec → reattach. No instant where the binary path is
      missing or non-executable.
- [ ] Invariant proof in `cargo xtask e2e --slice update` (this task wires the slice): 8 panes
      with a live marker stream; record daemon pid, all 8 pane pids and a scrollback marker → run
      `arreo update` → assert daemon pid and every pane pid unchanged, zero `exit` state events,
      markers still readable, reattach via the stored resume token in < 2 s, session id unchanged.
- [ ] Crash-safe swap: a crash injected between the two renames still leaves an executable
      binary at the path (old or new); the slice asserts `--version` runs either way and that
      the `.prev` recovery path restores the old one.
- [ ] A failed download or a failed verification leaves the running binary and the resume store
      byte-identical (hash before == hash after, asserted in the slice).
- [ ] Concurrency: two `arreo update` processes → one swaps, the other exits non-zero with
      "update already in progress" (a lock file, not a race); a stale lock from a killed updater
      is reclaimed by pid liveness, not by a timeout guess.
- [ ] `arreo update --rollback` restores `.prev`, re-execs and reattaches on the same
      resume token.
- [ ] `arreo update --check` reports current/available versions and changes nothing; on a
      read-only or package-manager-owned install path the updater makes zero partial writes
      and prints the package manager's own command instead.

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
- The `update` slice is wired here (T-0042 runs it on the CI matrix and chains it into the
  release story).

## Verification

```console
cargo test -p arreo-cli --test update
cargo xtask e2e --slice update
```
