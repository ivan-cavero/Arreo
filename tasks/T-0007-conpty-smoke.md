---
id: T-0007
title: ConPTY smoke test on Windows + the 3-OS CI matrix gate
phase: 0
priority: 2
status: todo
depends_on: [T-0002]
scope:
  - crates/arreo-core/tests/conpty/**
  - .github/workflows/*
  - xtask/src/**
---

## Goal

Prove portable-pty + ConPTY can hold a pane on Windows **early** — if it cannot, we must
know now. Then make the 3-OS matrix a hard release gate.

## Acceptance criteria

- [ ] `cargo xtask conpty-smoke` runs a real pane via ConPTY on a Windows runner: spawn
      `cmd /c dir`, read output, resize, assert content, kill, no zombie console.
- [ ] UTF-8 output round-trips (codepage forced; assert accented chars survive).
- [ ] CI matrix (ubuntu-latest, macos-latest, windows-latest) runs the full test battery;
      workflow requires all three green.
- [ ] Documented known limitations in a comment/doc (conhost degradation path).

## Verification

```console
cargo xtask conpty-smoke        # on windows runner / local windows machine
```

## Notes

This is the fail-fast task: if ConPTY + portable-pty can't hold a pane reliably, the
architecture needs to know in week one, not month six.
