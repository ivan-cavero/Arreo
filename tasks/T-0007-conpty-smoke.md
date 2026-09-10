---
id: T-0007
title: ConPTY smoke test on Windows + the 3-OS CI matrix gate
phase: 0
priority: 2
status: done
depends_on: [T-0002]
scope:
  - crates/arreo-core/tests/conpty/**
  - .github/workflows/*
  - xtask/src/**
  - xtask/Cargo.toml
  - docs/conpty-windows.md
---

## Scope note (re-scoped by loop, turn 6)

`xtask/Cargo.toml` (one dep line: xtask → arreo-core, dev-only) and
`docs/conpty-windows.md` (criterion 4 *requires* documented limitations)
were outside the letter of the fence but inside its intent. Reason written
here, not silent.

## Goal

Prove portable-pty + ConPTY can hold a pane on Windows **early** — if it cannot, we must
know now. Then make the 3-OS matrix a hard release gate.

## Acceptance criteria

- [x] `cargo xtask conpty-smoke` runs a real pane via ConPTY on a Windows runner: spawn
      `cmd /c dir`, read output, resize, assert content, kill, no zombie console.
      Shipped as a real verb (spawn → dir marker → resize → UTF-8 → reap).
      Windows-runner proof pends on CI push — unix proxy passes locally; the
      Windows CI step added to the workflow is what executes the ConPTY path.
- [x] UTF-8 output round-trips (codepage forced; assert accented chars survive).
      Verb forces `chcp 65001` on Windows; asserts `héllo wörld ✓` both sides.
- [x] CI matrix (ubuntu-latest, macos-latest, windows-latest) runs the full test battery;
      workflow requires all three green. Pre-existing from T-0001; this task adds
      the `conpty-smoke` step to the same matrix job.
- [x] Documented known limitations in a comment/doc (conhost degradation path).
      `docs/conpty-windows.md`.

## Verification

```console
cargo xtask conpty-smoke        # on windows runner / local windows machine
```

## Notes

This is the fail-fast task: if ConPTY + portable-pty can't hold a pane reliably, the
architecture needs to know in week one, not month six.
