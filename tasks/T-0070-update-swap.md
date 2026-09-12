---
id: T-0070
title: The update swap — stage, atomic rename, crash-safety, rollback, re-exec (no release channel needed)
phase: 2
priority: 2
status: proposed
depends_on: [T-0012, T-0013]
scope:
  - crates/arreo-core/src/update/**
  - crates/arreo-cli/src/update.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-cli/tests/update.rs
  - crates/arreo-tui/src/main.rs
  - xtask/src/update_slice.rs
  - xtask/src/main.rs
  - .github/workflows/ci.yml
  - docs/release.md
  - .loop/evidence/T-0070/**
---

## Goal

The half of §3.13's client update that does not need a release channel: **the swap
itself** — stage a binary beside the running one, `rename(2)` it into place keeping the
previous as `.prev`, survive a crash between the two renames, refuse to run twice at once,
roll back, re-exec, and reattach. Plus the invariant that makes updates safe for the agents:
**the client update path never signals, reaps, restarts or stops a PTY-bearing process, and
never stops the daemon.**

## Why this is split out of T-0037 (2026-09-12)

T-0037 covers the whole client update: *read the channel index → verify → stage → swap →
re-exec → reattach*. Five of its eight criteria are about the **swap**: the invariant, the
atomic swap, crash-safety, concurrency, and `--rollback`. Three are about the **channel**:
reading the index, verifying the artifact (T-0036), and `--check`.

T-0036 is human-gated — it needs a real minisign keypair whose secret must be created by its
owner as a CI secret (the point of an offline signing key is that an agent does not hold it).
So the channel half cannot start, and the swap half has no reason to wait: the swap is the
part that can *destroy* an agent, and it is the part most worth building and proving early.

The artifact source here is therefore an explicit local path:

```console
arreo update --from /path/to/a/newer/arreo
```

That is a real feature rather than a test seam — installing a binary you already have is
useful on its own, and it is where a channel source will feed in. **Verification is out of
scope and must fail closed**: absent a signed channel, this refuses to install an artifact it
cannot verify *unless* the operator named the path explicitly, which is a deliberate act and
the whole of what `--from` means. T-0037 adds the anonymous path (`arreo update`, no `--from`)
that requires a signature.

## Acceptance criteria

- [ ] The invariant is stated in code and in `docs/release.md`, and enforced by construction:
      the update path touches only the CLI/TUI binary path, the `.prev` slot and the resume
      store. It never signals, reaps, restarts or stops a PTY-bearing process, and never stops
      the daemon.
- [ ] `arreo update --from <path>` end-to-end: verify the artifact is a runnable binary →
      stage beside the running binary **on the same filesystem** → atomic `rename(2)` keeping
      the previous as `.prev` → re-exec → reattach. At no instant is the binary path missing or
      non-executable.
- [ ] **Invariant proof in `cargo xtask e2e --slice update`** (this task wires the slice): 8
      panes with a live marker stream; record the daemon pid, all 8 pane pids and a scrollback
      marker → run the update → assert the daemon pid and every pane pid are unchanged, zero
      `exit` state events, the markers are still readable, and the reattach happens in < 2 s
      with an unchanged session id.
- [ ] Crash-safe: a crash injected between the two renames still leaves an executable binary at
      the path (old or new). The slice asserts `--version` runs either way, and that the `.prev`
      recovery path restores the old one.
- [ ] A failed staging or a failed verification leaves the running binary byte-identical (hash
      before == hash after, asserted in the slice).
- [ ] Concurrency: two `arreo update` processes → one swaps, the other exits non-zero with
      "update already in progress" (a lock file, not a race); a stale lock from a killed updater
      is reclaimed by pid liveness, not by a timeout guess.
- [ ] `arreo update --rollback` restores `.prev`, re-execs and reattaches.
- [ ] On a read-only or package-manager-owned install path the updater makes **zero** partial
      writes and prints the package manager's own command instead.

## Notes

- **Swap mechanics differ per OS, the control flow does not.** Unix is `rename(2)`; Windows
  renames the running image away first and renames the new one in (a running image can be
  renamed but not deleted — `.prev` is dropped on the next successful start). Windows *server*
  updates are T-0039's story.
- **The resume token is part of this task**, because re-exec without it is just a restart. It
  lives in the XDG state dir, is written *before* the swap and used after re-exec, and it is the
  same token a phone uses after a network hop — one mechanism, two clients. It does not exist
  yet (`grep resume` finds nothing), so it is built here rather than assumed.
- Rejected: overwrite-in-place (a crash mid-write bricks the install) and "restart via the
  service manager" (that touches the daemon — the invariant forbids it).
- Honest gap: Homebrew and `cargo install` own their binary path, so the updater prints the
  correct package-manager command rather than fighting it. No unattended background install
  ships here.

## Verification

```console
cargo test -p arreo-cli --test update
cargo xtask e2e --slice update
```
