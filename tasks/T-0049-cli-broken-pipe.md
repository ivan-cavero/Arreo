---
id: T-0049
title: CLI robustness — never panic on a closed stdout/stderr pipe
phase: 2
priority: 4
status: done
depends_on: [T-0014]
scope:
  - crates/arreo-cli/src/main.rs
  - crates/arreo-cli/tests/**
---

## Goal

Found while writing T-0024's tests: `arreo pair | head -1` aborts the CLI. Rust's
`println!` panics when the write fails, and a reader that exits (a pager, `head`, a
harness that stops reading) closes the pipe, so *every* printing verb
(`audit`, `devices list`, `panes`, `metrics`, `pair`, …) can die with
`failed printing to stdout: Broken pipe (os error 32)` and exit 101 where it
should exit quietly. A Unix tool that panics because its consumer went away is a
tool that cannot be composed in a pipeline — and Arreo's CLI is exactly the
artifact scripts and the agent skill drive.

## Acceptance criteria

- [x] `arreo <verb> | head -1` exits without a panic message for at least
      `audit`, `devices list`, `panes`, and `pair` (the last one mid-wait): exit
      status is 0..=141 with no `panicked at` text on stderr. Proven by a test
      that pipes the CLI's stdout into a reader which closes after one line.
- [x] The fix is one decision applied once, not a per-call-site `let _ =`:
      either restore the default `SIGPIPE` disposition so the process dies the
      way every other Unix tool does, or route all CLI output through a writer
      that returns `Result` and maps a broken pipe to a clean exit. State which
      and why in the ledger (an ADR only if it constrains other crates).
- [x] No CLI verb changes its output bytes: the pipeline test compares the first
      line against the same run redirected to a file.
- [x] `cargo test --workspace`, clippy `-D warnings`, and fmt stay green; the
      existing pairing/devices/audit tests are unaffected.

## Landing notes (2026-09-11)

Decision, as the criterion demands: **default SIGPIPE disposition**, not a
Result-routing writer. One `unsafe` block at the single entry point versus 245
touched print sites plus an audit that no future `println!` reintroduces the
panic; the disposition is process-wide, which is exactly the scope of the
problem (every verb prints). No `libc` dependency for one call — the raw
`signal(2)` binding is three lines, `unsafe` confined to installing `SIG_DFL`
(which runs no code and cannot violate memory safety), no-op on non-Unix. No
ADR: the decision constrains no other crate (CLI entry point only), and the
rationale lives in the code comment plus this note.

Two lessons from proving it, both in the tests rather than smoothed over:

- **A test that cannot fail proves nothing.** The first draft read one line then
  closed: 50 rows (~7 KB) fit the 64 KB pipe buffer, so the child usually
  finished before the close landed — and passed 3/3 *without* the fix. The
  deterministic shape closes immediately (spawn+close is microseconds,
  exec+query+print is milliseconds); that the verb would have written is proven
  by the file run, asserted non-empty. Same reason the `--json` single-line
  variants were dropped: `head -1` reads all of one line.
- **`pair` mid-wait cannot be pipe-tested without the full rig** (live relay +
  phone; pairing.rs keeps stdout open deliberately). The fourth test asserts the
  property that covers it instead: the disposition is installed once at the top
  of `main`, so no verb can opt out — two definitions (unix + non-unix), one
  call, positioned before any verb runs.

## Notes

- Deliberately *not* folded into T-0024: SIGPIPE disposition is process-wide and
  affects every verb, so it is a CLI decision rather than a pairing one. T-0024
  worked around it in its test harness (which keeps the child's stdout open and
  says why), and recorded the finding here.
- If the chosen approach is `SIGPIPE`-to-default, that needs `unsafe` (or a libc
  dependency); if it is error mapping, it touches every `println!` in the CLI and
  needs a small `out!`/`print_line` helper. Prefer whichever keeps the diff
  small *and* auditable — the rationale is what matters, not the byte count.
- Harnesses under `xtask/**` spawn these verbs with piped stdout, so they inherit
  the same hazard today (an xtask that stops reading can abort the child it is
  measuring). Fixing the CLI fixes those too.

## Verification

```console
cargo test -p arreo-cli
cargo xtask e2e --slice api
```

Last run: not yet implemented.
