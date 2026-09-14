---
id: T-0039
title: Windows deferred update — the core: keep the artifact, and the one window it may be promoted in
phase: 2
priority: 4
status: done
depends_on: [T-0018, T-0037, T-0038]
scope:
  - crates/arreo-core/src/update/**
  - crates/arreo-cli/src/update.rs
  - xtask/src/update_slice.rs
  - docs/release.md
  - .loop/evidence/T-0039/**
---

## Re-scope (planner, 2026-09-14) — why this task is now two

The task as filed put the deferred update, its Windows application point, and a
Windows CI proof in one fence. **The Windows half cannot be compiled on this
box, let alone run**: `cargo check --target x86_64-pc-windows-msvc -p arreo-server`
fails at a C dependency (`cc-rs: failed to find tool "lib.exe"`), which is exactly
what `cargo xtask check-targets` reports as `SKIP (C deps need SDK — CI covers)`.
No `clang`, no `wine`, no Windows SDK here.

So the split is by **what this machine can prove**, with the reason written here
rather than the work silently dropped:

- **T-0039 (this task, done)** — the deferred update itself: the one window rule,
  the staged artifact, the marker, the refusals, the operator surfaces, and the
  end-to-end case in the update slice. Every line of it is exercised by
  `cargo test` and `cargo xtask e2e --slice update --case deferred` on this box.
- **T-0090** — the application point that makes the platform difference real:
  Windows service control codes, promotion before the socket is bound, `.prev`
  cleanup on a successful start, and the `windows-deferred` slice case that the
  Windows runner runs. Its proof is the Windows runner, the same class as T-0085.

The fence below is what this task actually touched. `crates/arreo-server/src/handoff/windows.rs`,
`crates/arreo-server/src/lifecycle.rs`, `xtask/src/main.rs` and
`.github/workflows/ci.yml` moved to T-0090's fence.

## Goal

§3.13's fallback, made honest: an update that cannot be cut over live is **kept**
rather than discarded, reported until it is promoted, and promoted only in the
one window that costs nobody their work.

## What was built

- **`arreo_core::update::deferred`** — the deferred state. `window_is_open` is
  the *one* rule (`live_panes == 0`), `panes_block` is the refusal that names
  each pane, `stage_next` writes the verified artifact as `<current>.next` plus a
  JSON marker in the state directory, `promote` is the one door (refuse for
  panes → re-prove the stage → swap), `clear_after_confirm` clears the marker
  only when a binary reporting the staged version has taken over. Every function
  that touches the marker has an `_in(state_dir)` form, so the tests never touch
  the machine's real state directory (the same reason `resume::dir_from` exists).
- **The CLI**: `arreo update --status` (the pending line, and the version
  confirmation that clears it), `arreo update --apply-now` (the window: exit 3
  and each pane named when one is live), and the deferral itself — a failed cut
  keeps the artifact instead of `remove_file`-ing it. The refusal to *combine*
  `--status`/`--apply-now` with a flag that describes a new install is part of
  the same story: a flag obeyed silently is a lie about what happened.
- **The slice**: `cargo xtask e2e --slice update --case deferred` — 7 checks,
  end to end on the real binaries. `--case` is new; the default (no `--case`) is
  the whole slice, so every existing invocation is unchanged.

## Acceptance criteria

- [x] `docs/release.md` records the deferral evidence: ConPTY pseudoconsole
      handles *are* inheritable (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`), but the
      read loop, conduit pipes and child bookkeeping are process-local, and a
      running image can only be renamed, never replaced — so a cut needs a
      restart regardless. Deferred on evidence, with the revisit condition.
- [x] The safe-window rule is one function: `deferred::window_is_open`, whose
      only input is the pane list, so the caller's obligation (read it under the
      daemon's lock, immediately before the swap) is visible in the signature.
      `arreo update --apply-now` exits **3** while any pane lives and names them;
      the promotion at zero panes is the same call. The "plain restart" half of
      this criterion is Windows' — T-0090, because there is no handoff there to
      make a restart unnecessary.
- [x] Deferred path: the verified artifact is staged as `<current>.next`,
      reported by `arreo update --status` as
      `update pending v0.2.0 → v0.3.0 (applies at next restart)`, and promoted by
      `arreo update --apply-now` when the window opens. A marker that cannot be
      parsed is a loud error naming the file, never "nothing pending". The
      **automatic** promotion at the next start is T-0090.
- [x] A verified-but-unpromotable stage (no longer runnable, or reporting a
      version other than the one recorded) refuses promotion, keeps the current
      binary serving, reports why, **and is discarded with its marker** — so the
      same failure cannot repeat on every boot. That is a property of the code,
      not a promise: every refusal path calls `discard_in`.
- [x] `.prev` survives the promotion and holds what was replaced (proved in the
      slice case: `arreo-server.prev` reports 0.2.0 after the swap). Its deletion
      on the next successful start needs the start hook — T-0090.
- [ ] Windows Service integration — **T-0090** (cannot be compiled here: no MSVC
      SDK; `check-targets` SKIPs it and CI covers it).
- [ ] Windows CI proof — **T-0090** (`--case windows-deferred` on the Windows
      runner). The Unix case is here and green: 7 passed, 0 failed.

## Verification

```console
cargo test -p arreo-core update          # 7 deferred tests + the update module's own
cargo xtask e2e --slice update --case deferred
```

Evidence: `.loop/evidence/T-0039/` — `deferred-e2e.txt` (the same sequence
through the CLI by hand, including the marker on disk), `deferred-e2e.sh` (the
script that produced it), `slice-case.txt` (the slice's run).

## Mutations (run by the planner; each reddens what it should)

- `window_is_open` always true → 3 slice checks red (`a live pane refuses…`,
  `at zero panes the promotion happens…`, `the marker clears…`), and 2 unit
  tests red.
- `clear_after_confirm` ignoring the version → the promote/confirm unit test red.
- `promote` not discarding an unpromotable stage → the no-retry-loop unit test
  red.

## Notes

- The slice case is Unix-only and says so (`[SKIP]` on Windows): the proof needs
  a *stand-in* server that answers `--version` and nothing else, which is a shell
  script. The Windows case is T-0090's.
- The other half of "does not retry in a loop" is the busy case: when the
  one-handoff lock is held, another updater is cutting **right now**, so the
  artifact is still discarded rather than deferred — a deferred artifact of ours
  would be promoted over theirs at the next start. That distinction is in the
  code and in the CLI's comment.
