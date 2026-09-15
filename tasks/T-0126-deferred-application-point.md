---
id: T-0126
title: The deferred update on Windows — service control codes, promotion before serving, and the windows-deferred case
phase: 2
priority: 4
status: proposed
depends_on: [T-0039, T-0105]
scope:
  - crates/arreo-server/src/handoff/windows.rs
  - crates/arreo-server/src/handoff.rs
  - crates/arreo-server/src/lifecycle.rs
  - crates/arreo-server/src/main.rs
  - xtask/src/update_slice.rs
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
why it is its own task and why its proof is the Windows runner — the same class of
gate as T-0085.

**Where a line can be type-checked on Linux, it does not live here.** T-0039 used
that trick for `report_deferred` (compiled on every platform, called from a `#[cfg]`
site), and T-0105 took the whole platform-neutral half of this task for the same
reason: the automatic promotion, the re-exec and `.prev` cleanup are proven on
Linux and are not in this fence. What is left here is only what genuinely needs
Windows.

## Goal

T-0039 built the deferred update and proved it on Unix; T-0105 made the promotion
automatic and proved the start path on Unix. What remains is the part that is
*actually different on Windows*: how a Windows daemon is stopped and started, and
the slice case that runs there.

## Acceptance criteria

- [ ] **The Windows procedure exists as a module beside the Unix one**:
      `arreo-server/src/handoff/windows.rs`, so the "this platform cannot cut, and
      here is what it does instead" answer sits next to the Unix path it replaces —
      one place to read the platform matrix. `docs/release.md`'s deferral section
      points at it.
- [ ] **Windows Service integration**: stop/start through `windows-service` control
      codes rather than signals (a new, Windows-only dependency — ledger note with
      the rationale, and `cargo vet`/`cargo deny` exemptions regenerated), and a
      service-started daemon runs T-0105's start path — promote, then re-exec —
      **before it binds the socket or spawns any PTY**.
- [ ] **`arreo update --apply-now` performs the plain restart at zero panes** where
      no live cut exists (the half of T-0039's criterion 2 that belongs to this
      platform): nothing to hand off, so the daemon is stopped and started through
      the service manager, and the report says which happened. The promotion itself
      is T-0105's code; this is the Windows restart around it.
- [ ] **`cargo xtask e2e --slice update --case windows-deferred`**: 3 live panes →
      pids unchanged across stage and swap, status reports pending, `--apply-now`
      refuses, and after the panes exit a restart serves the new version. Run on the
      Windows runner in `.github/workflows/ci.yml` (add the case to the existing
      update step; do not restructure the matrix — that file is the user's, and
      T-0063/T-0085 are open on it).
- [ ] No second update implementation: verification, staging, the channel index,
      `.prev`, the marker and rollback stay in `arreo-core::update`. Only the
      *application point* differs — a Windows-only update path is the bug this task
      exists to prevent.

## Notes

- Inputs: T-0039's evidence (`.loop/evidence/T-0039/`), T-0105's start-path work,
  `docs/release.md`'s "When the cut cannot happen", and `specs/adr/0021*` for the
  Unix cut this replaces.
- Wine is a fast local smoke only and never a shipping claim (`docs/cross-os.md`);
  GitHub's Windows runner is the authority.
- If `.github/workflows/ci.yml` is mid-edit by the user (T-0063), the CI step is the
  last thing to land: the daemon side can be finished and reviewed without it, and
  the criterion stays unticked until the runner runs it.
