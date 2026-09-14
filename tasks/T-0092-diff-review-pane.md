---
id: T-0092
title: Diff review — read a worktree's diff without leaving the wall
phase: 4
priority: 2
status: proposed
depends_on: [T-0091, T-0015]
scope:
  - crates/arreo-core/src/diff.rs
  - crates/arreo-tui/src/**
  - crates/arreo-cli/src/main.rs
  - crates/arreo-server/tests/diff.rs
  - xtask/src/tui_slice.rs
  - docs/tour.md
  - .loop/evidence/T-0092/**
verify:
  - cargo test --workspace
  - cargo xtask e2e --slice tui
---

## Goal

An agent in a worktree (T-0091) produces changes nobody reviews, because reviewing means
leaving the wall for a terminal and `git diff`. ROADMAP §6 Phase 4: "diff review (desktop
full, mobile read + comment-back)".

One sentence: the TUI renders the diff of a pane's worktree — per file, with the hunks a
reviewer reads — and the same bytes are available to a CLI consumer.

## Acceptance criteria

- [ ] `arreo diff <pane> [--json]` prints the pane's worktree diff (staged + unstaged + new
      files) as a stable, documented structure; the JSON schema is versioned like the other
      CLI schemas and has a contract test that fails on a renamed key.
- [ ] A **parser, not a pager**: `arreo_core::diff` turns unified-diff bytes into a typed
      structure (files, hunks, line kinds, old/new line numbers). Hand-written and tested
      against the real output of `git diff` on this box — including a rename, a binary file, a
      file with no trailing newline, and a CRLF file, each asserted rather than assumed.
- [ ] The TUI renders it: a diff view for the focused pane's worktree with per-file navigation,
      additions/deletions styled from the **theme tokens** (T-0016 — `diff*` tokens already
      exist), and horizontal scroll for long lines. Keyboard-complete; no mouse-only action.
- [ ] A pane with no worktree, or a clean one, says so in one line rather than showing an empty
      view: "no changes" and "no worktree" are different facts and are not collapsed.
- [ ] Evidence: scripted-pty frames of the diff view on a real repository with a real change,
      under `.loop/evidence/T-0092/` — a unit test on a parsed string does not close this task.
- [ ] `cargo xtask e2e --slice tui` extended with the diff assertions (it drives the real
      binary on a real pty, which is the only proof that the view is readable).

## Notes

- No diff **editing** and no comment-back in this task: the mobile half is Phase 3's surface and
  the review loop (comment → send to the agent) is a follow-up, filed when this lands.
- Do not pull in a diff crate: the parsing we need is a page of code, and the failure mode of a
  pager library here is a screen the operator cannot read.
