---
id: T-0003
title: VT state per pane — alacritty_terminal grid + scrollback paging
phase: 0
priority: 2
status: todo
depends_on: [T-0002]
scope:
  - crates/arreo-core/src/vt/**
  - crates/arreo-core/tests/vt/**
---

## Goal

Feed PTY bytes through `alacritty_terminal` (+ `vte`) per pane; keep a compact grid and
stream older scrollback to a disk-backed file (mmap reattach on demand).

## Acceptance criteria

- [ ] Grid API: cell-range reads, cursor position, dirty ranges after each feed.
- [ ] Scrollback: hot 512 lines in RAM, older lines spill to disk, paged back losslessly
      (test: replay 100k lines, page from index 0 matches byte-for-byte).
- [ ] Feed is O(output), never O(grid scan).
- [ ] No unbounded growth under a 1M-line replay (test with memory assertion).

## Verification

```console
cargo test -p arreo-core vt
```
