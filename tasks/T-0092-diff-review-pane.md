---
id: T-0092
title: Diff review — read a worktree's diff without leaving the wall
phase: 4
priority: 2
status: done
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

- [x] `arreo diff <pane> [--json]` prints the pane's worktree diff (staged + unstaged + new
      files) as a stable, documented structure; the JSON schema is versioned like the other
      CLI schemas and has a contract test that fails on a renamed key.
- [x] A **parser, not a pager**: `arreo_core::diff` turns unified-diff bytes into a typed
      structure (files, hunks, line kinds, old/new line numbers). Hand-written and tested
      against the real output of `git diff` on this box — including a rename, a binary file, a
      file with no trailing newline, and a CRLF file, each asserted rather than assumed.
- [x] The TUI renders it: a diff view for the focused pane's worktree with per-file navigation,
      additions/deletions styled from the **theme tokens** (T-0016 — `diff*` tokens already
      exist), and horizontal scroll for long lines. Keyboard-complete; no mouse-only action.
- [x] A pane with no worktree, or a clean one, says so in one line rather than showing an empty
      view: "no changes" and "no worktree" are different facts and are not collapsed.
- [x] Evidence: scripted-pty frames of the diff view on a real repository with a real change,
      under `.loop/evidence/T-0092/` — a unit test on a parsed string does not close this task.
- [x] `cargo xtask e2e --slice tui` extended with the diff assertions (it drives the real
      binary on a real pty, which is the only proof that the view is readable).

## Outcome, and the defects the slice caught

`arreo_core::diff` (parser + `worktree_diff`), the CLI verb `arreo diff <pane> [--json]`, the
TUI view (`d`), the JSON contract test, and six new assertions in the TUI slice (**87 passed**,
was 80). The frame the criterion asked for is
`.loop/evidence/T-0092/02-diff-view.txt` — line-number gutters, `+`/`-` prefixes, per-file
headers, the summary and `file 1/N`, coloured from T-0016's twelve `diff*` tokens.

**Three defects, all found by testing rather than by reading** (the parser's doc comment
records each):

1. `str::lines()` **strips `\r`**, so the CR of a CRLF source line vanished — the parser
   silently disagreeing with the file it describes. It now splits on `\n` only; the mutation
   reddens the CRLF assertion.
2. `run_git`'s "exit 1 means differences" rule **swallowed `rev-parse --verify`'s "no HEAD"**,
   so the unborn-repository check never fired and the failure surfaced later as a confusing
   git error.
3. **Both consumers resolved the repository from their own working directory**, ignoring
   `[worktree] repo` — which the *daemon* honours when it creates the worktree. A consumer
   running anywhere but the repository therefore reported "no worktree" about a pane that
   plainly had one: a wrong answer that reads like a fact about the pane. The TUI slice caught
   it (its TUI runs from the workspace root) by printing
   `/home/dev is not inside a git repository`. One resolution chain now serves `arreo diff`,
   `arreo worktrees list|remove` and the TUI: `--repo` → `[worktree] repo` → the current
   directory, with a regression test that runs `arreo diff` from `/`.

Mutations verified by the planner: renaming `old_line` to `oldLine` in the JSON reddens the
schema contract test; swapping `split_lines` for `lines()` reddens the CRLF assertion.

## Notes

- No diff **editing** and no comment-back in this task: the mobile half is Phase 3's surface and
  the review loop (comment → send to the agent) is a follow-up, filed when this lands.
- Do not pull in a diff crate: the parsing we need is a page of code, and the failure mode of a
  pager library here is a screen the operator cannot read.
