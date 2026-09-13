---
id: T-0080
title: Escape-aware state signals — an OSC BEL is not a bell, plus omp's adapter and TUI-mode fixtures
phase: 4
priority: 2
status: done
depends_on: [T-0017, T-0075]
scope:
  - adapters/**
  - crates/arreo-core/src/state/**
  - fixtures/**
  - crates/arreo-server/tests/**
  - xtask/src/adapters_check.rs
  - .loop/evidence/T-0080/**
verify:
  - cargo xtask adapters --check
  - cargo xtask e2e --slice state
---

## Goal

Two harnesses are supported deeply, and one of the signals that support rests on is wrong.
The state engine flags a bell by scanning the **raw bytes** for `0x07`
(`crates/arreo-core/src/state/engine.rs`: `if bytes.contains(&0x07)` → `bell_pending` → an
`Event { state: Blocked, rule: "bell" }` for any adapter with `bell_means_attention = true`,
which is all three shipped adapters). But `0x07` is also the standard **terminator of an OSC
string** — a window title, a hyperlink. Measured on the two live harnesses
(`.loop/evidence/T-0075/tui-escape-analysis.txt`, raw captures kept): **every** BEL pi 0.84.4
and opencode 1.18.30 emitted in their TUIs terminated an OSC string — 60/60 and 5/5 in ten
seconds. So a pane that prints a clickable link or sets a title reports `Blocked`.

Reproduced end to end through the product, not argued from the code (planner, 2026-09-13):

```console
$ arreo spawn hyperlink /bin/sh -c 'printf "\033]8;;http://example.com\007click\033]8;;\007"; sleep 30'
spawned hyperlink
$ arreo wait hyperlink --state blocked --timeout 2s
state=Blocked confidence=inferred:bell pattern=None
```

A pane whose entire output is one hyperlink is a pane the fleet's attention ordering puts at
the top. This is the "question detection on the builders themselves" scenario §6 asks for, and
it fails.

## Acceptance criteria

- [x] **A BEL that terminates an OSC string is not a bell.** Detection parses the escape
      stream rather than scanning for the byte; the reproduction above must report a
      non-attention state, with a regression test whose fixture is built from the recorded TUI
      captures (replaying them must not produce `Blocked`).
- [x] A **bare** BEL — the real bell, not an OSC terminator — still means attention exactly as
      today, proven by a test that asserts both halves. The change must not trade a false
      positive for a false negative.
- [x] `adapters/omp.toml`: omp 18.1.16 as a first-class adapter (`harness`, program match,
      resume strategy, patterns) instead of falling through to `default.toml`, with its resume
      argv verified live (`-r {session}` by id prefix and `-c` both re-open the same file — the
      survey's transcript) and ≥4 recorded fixtures (question/working/idle/stress).
- [x] **TUI-mode fixtures for pi and opencode** — today's fixtures are `--print`/text mode and
      contain no escape sequences, which is why this bug survived T-0017. The new fixtures
      carry OSC strings (opencode in alt-screen, pi not — the survey measured both) and the
      adapter check runs the existing latency assertion (≤ 200 ms) against them.
- [x] No new dependency; no `#[allow]`; `cargo xtask adapters --check` green with the new
      fixture count stated, and `--slice state` green.

## Why this is one task

The three parts are one story: the escape-aware parse is what the TUI fixtures test, and the
TUI fixtures are what make omp's adapter honest (its events were read live in `--mode json`,
but its TUI has never been captured). Splitting them would ship the parser without the fixture
that makes it meaningful, or a new adapter whose signals rest on the same wrong rule.

## Notes

- Input: `specs/harness-matrix.md` (T-0075) rows for pi/opencode/omp and
  `.loop/evidence/T-0075/tui-escape-analysis.txt` with the two raw captures.
- Rejected alternative: setting `bell_means_attention = false` in the three TOMLs. It removes
  the false positive by removing the signal — a real bell would stop meaning anything — and it
  would leave the same byte-scan in place for the next escape sequence that contains `0x07`.
- The engine's own tests already cover BEL-means-attention; they feed synthetic bytes, which is
  exactly why they pass today. The new tests must feed **recorded** output.
