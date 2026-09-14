---
id: T-0090
title: The deferred update's application point — Windows service control, promotion before serving, and the windows-deferred case
phase: 2
priority: 4
status: proposed
depends_on: [T-0039]
scope:
  - crates/arreo-server/src/handoff/windows.rs
  - crates/arreo-server/src/handoff.rs
  - crates/arreo-server/src/lifecycle.rs
  - crates/arreo-server/src/main.rs
  - crates/arreo-core/src/update/**
  - xtask/src/update_slice.rs
  - xtask/src/main.rs
  - .github/workflows/ci.yml
  - docs/release.md
  - .loop/evidence/T-0090/**
verify:
  - cargo xtask e2e --slice update --case windows-deferred   # Windows runner
  - cargo test -p arreo-core update
---

## GATE — this task cannot be proven on a Linux box, and that is measured

```console
$ cargo check --target x86_64-pc-windows-msvc -p arreo-server
error occurred in cc-rs: failed to find tool "lib.exe": No such file or directory
$ which clang clang-cl lld llvm-lib wine wine64
(nothing)
$ cargo xtask check-targets
check-targets: x86_64-pc-windows-msvc SKIP (C deps need SDK (lib.exe) — CI covers)
```

So the code this task adds **cannot be type-checked here**, let alone run. That is
the honest reason it is a task of its own rather than part of T-0039, and the
reason its proof is the Windows runner — the same class as T-0085.

**Where a line can be type-checked on Linux, put it where it can be.** T-0039
already used that trick for `report_deferred` (compiled on every platform, called
from a `#[cfg]` site). Prefer it again.

## Goal

T-0039 built the deferred update and proved it on Unix: the window rule, the
staged artifact, the marker, the refusals, and the operator surfaces. What is
left is the part that is *actually different on Windows* — the application point —
plus the two things whose caller lives in a start hook.

## Acceptance criteria

- [ ] **The Windows procedure exists as a module beside the Unix one**:
      `arreo-server/src/handoff/windows.rs`, so the "this platform cannot cut, and
      here is what it does instead" answer sits next to the Unix path it replaces
      — one place to read the platform matrix. `docs/release.md`'s deferral
      section points at it.
- [ ] **Windows Service integration**: stop/start through `windows-service`
      control codes rather than signals (a new, Windows-only dependency — ledger
      note with the rationale, and `cargo vet`/`deny` exemptions regenerated), and
      a service-started daemon **promotes the staged binary before it binds the
      socket or spawns any PTY** — using `arreo_core::update::deferred::promote`,
      under the daemon's own lock, with the pane list read at that instant.
- [ ] **The automatic promotion**: on every platform, a start that finds a pending
      marker promotes it in the window (no panes at start), so the operator does
      not have to run `--apply-now` at all. The window rule is not re-implemented:
      it is `deferred::window_is_open`.
- [ ] **`.prev` is deleted on the next successful start** (the function and its
      caller — T-0039 deliberately did not ship an uncalled one), and the pending
      marker clears only after a binary reporting the new version confirms it.
- [ ] **`arreo update --apply-now` performs the plain restart at zero panes**
      where no live cut exists (the half of T-0039's criterion 2 that belongs to
      this platform): nothing to hand off, so the daemon is stopped and started
      through the service manager, and the report says which happened.
- [ ] **`cargo xtask e2e --slice update --case windows-deferred`**: 3 live panes →
      pids unchanged across stage and swap, status reports pending, `--apply-now`
      refuses, and after the panes exit a restart serves the new version. Run on
      the Windows runner in `.github/workflows/ci.yml` (add the case to the
      existing update step; do not restructure the matrix — that file is the
      user's, and T-0063/T-0085 are open on it).
- [ ] No second update implementation: verification, staging, the channel index,
      `.prev` and rollback stay in `arreo-core::update`. Only the *application
      point* differs — a Windows-only update path is the bug this task exists to
      prevent.

## Notes

- Inputs: T-0039's evidence (`.loop/evidence/T-0039/`), `docs/release.md`'s
  "When the cut cannot happen", and `specs/adr/0021*` for the Unix cut this
  replaces.
- Wine is a fast local smoke only and never a shipping claim (`docs/cross-os.md`);
  GitHub's Windows runner is the authority.
- If `.github/workflows/ci.yml` is mid-edit by the user (T-0063), the CI step is
  the last thing to land: the slice case and the daemon side can be finished and
  reviewed without it, and the criterion stays unticked until the runner runs it.
