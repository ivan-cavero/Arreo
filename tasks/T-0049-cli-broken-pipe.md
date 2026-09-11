---
id: T-0049
title: CLI robustness — never panic on a closed stdout/stderr pipe
phase: 2
priority: 4
status: proposed
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

- [ ] `arreo <verb> | head -1` exits without a panic message for at least
      `audit`, `devices list`, `panes`, and `pair` (the last one mid-wait): exit
      status is 0..=141 with no `panicked at` text on stderr. Proven by a test
      that pipes the CLI's stdout into a reader which closes after one line.
- [ ] The fix is one decision applied once, not a per-call-site `let _ =`:
      either restore the default `SIGPIPE` disposition so the process dies the
      way every other Unix tool does, or route all CLI output through a writer
      that returns `Result` and maps a broken pipe to a clean exit. State which
      and why in the ledger (an ADR only if it constrains other crates).
- [ ] No CLI verb changes its output bytes: the pipeline test compares the first
      line against the same run redirected to a file.
- [ ] `cargo test --workspace`, clippy `-D warnings`, and fmt stay green; the
      existing pairing/devices/audit tests are unaffected.

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
