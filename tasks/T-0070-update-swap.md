---
id: T-0070
title: The update swap — stage, atomic rename, crash-safety, rollback, re-exec (no release channel needed)
phase: 2
priority: 2
status: done
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

- [x] The invariant is stated in code (`arreo_core::update`'s module docs) and in
      `docs/release.md`, and enforced by construction: the module touches a binary path, its
      `.prev` sibling and the resume token, and contains no `kill`, no `waitpid` and no
      service-manager call. **Proved, not asserted**, by the slice — see the criteria below.
- [x] `arreo update --from <path>` end-to-end, with one design change that makes the
      guarantee stronger than asked for: **the swap is a hard link plus one atomic `rename(2)`**,
      not two renames. `hard_link(current, .prev)` adds a second name for the old inode, then
      `rename(staged, current)` replaces the entry in one syscall — so there is no window in
      which the path is missing, and a crash after any step leaves a runnable binary there.
      (This task asked for crash-injection *between the two renames*; the link removes the
      window instead, and the unit tests assert the path is runnable after every step. ADR
      0020 records the reasoning and the rejected sequence.)
- [x] **Invariant proof in `cargo xtask e2e --slice update`**: 17 passed, 0 skipped, 0 failed in
      ~12 s. Eight panes run a monotonic counter with a marker; after a real update the slice
      asserts the daemon's `Child` is still un-reaped and its pid unchanged, every pane is alive
      with its marker readable, and every counter **continued past where it was** rather than
      resetting — which is how "the pane process was not restarted" is proven, since `PaneInfo`
      carries no pid (a discovery this task records: the wire has `id`, `alive`, `alert` and
      nothing else). The reattach is measured at **0.35–0.55 s** against the 2 s budget.
      **Adversarial pass**: the worker mutated the pane script to re-print its marker and jump
      its counter (emulating a restart) and check 1c FAILED, exit 1 — so the evidence is
      load-bearing rather than a trend that would pass either way.
- [x] Crash-safe, by construction rather than by window: the unit tests walk the sequence step by
      step and assert a runnable binary at the path after each one; the slice asserts `--version`
      runs at all six points it checks; and `--rollback` restores the original bytes (asserted on
      bytes, not on a version string).
- [x] A refused candidate leaves the running binary byte-identical and leaves nothing staged — the
      slice asserts both, for a file with no execute bit (the `NotExecutable` refusal), and the CLI
      test does the same through the verb.
- [x] Concurrency, and better than asked: the lock is an **OS file lock** (`File::try_lock`), not
      a lock file with a pid in it. The second updater exits **3** with "another update is already
      in progress (holding …)". The stale-lock case this criterion worried about **cannot arise** —
      the kernel holds the lock in the open file description and releases it when the process ends,
      cleanly or by `SIGKILL` — so there is no liveness probe and no timeout to guess with. Proved
      two ways: the unit test locks, fails a second acquire, drops, and re-acquires; the slice
      holds the lock from its own process and watches the verb refuse.
- [x] `arreo update --rollback` restores `.prev`; the restored binary is then run (`--version`) to
      prove it is genuinely the old one, and a second rollback refuses with the path it looked for.
      (Rollback does not re-exec: it is the recovery path, and re-running an old binary over a new
      install is the one thing an operator may want to inspect before handing over.)
- [x] A path this user cannot write is recognised and named: `package_manager_advice` maps
      `/Cellar/`, `/opt/homebrew/`, `/.cargo/bin/` and `/usr/{local/,}bin/` to `brew upgrade arreo`,
      `cargo install --force arreo` or the distribution's package manager, exits 4, and makes no
      partial writes (the failure happens at the `.prev` link, before anything is replaced).
      Unknown locations get generic advice rather than a wrong package manager — asserted.

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

## Outcome

Done. `arreo update --from <path>`, `--rollback` and `--check`, with the swap in
`arreo_core::update` and the resume token in `arreo_core::update::resume`.

**The invariant is proven, which is the point of the task**: the slice holds a real daemon and
eight live panes across a real update and shows the daemon un-reaped, every pane alive, and every
pane's counter continuing — 17 checks, ~12 s, hermetic, in CI.

Three findings worth keeping:

- **"No pane restarted" cannot be proven by pid**, because `PaneInfo` carries only `id`, `alive`
  and `alert`. The proof has to be behavioural — a monotonic counter that would reset — and the
  worker checked that the evidence *bites* by mutating the pane script to emulate a restart and
  watching the check fail.
- **`exec` discards unflushed stdout, and it replaced the process image before the report was
  printed.** The first version printed the install lines *after* handing over, so
  `arreo update … | cat` showed only the new binary's output. Reporting now happens before the
  hand-over, and the hand-over carries the path it already resolved — because after the swap
  `/proc/self/exe` names the unlinked old dentry (`… (deleted)`) and re-deriving it fails.
- **A test that copies a 128 MB binary three times and only cleans up on success filled a 12 GB
  tmpfs in one failing run.** The CLI tests now hold their scratch directory in a `Drop` guard and
  hard-link the installed copy instead of copying it.

Criterion-by-criterion: 8 of 8. ADR 0020 records the hard-link-over-two-renames decision, the
OS-lock-over-pid-file decision, and why the hand-over carries its path.
