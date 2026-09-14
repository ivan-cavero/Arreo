---
id: T-0082
title: Adapter suite v2 batch C — the extension-family long tail, one TOML per recorded harness
phase: 4
priority: 4
status: done
depends_on: [T-0075, T-0080]
scope:
  - adapters/**
  - fixtures/**
  - .loop/evidence/T-0082/**
verify:
  - cargo xtask adapters --check
---

## Goal

Herdr's integration registry names the extension point of a dozen more harnesses — shell hooks
for Grok, Qwen, Qoder, Droid, Cursor, Copilot, Devin, Kimi, Antigravity and MastraCode, a JS
plugin for Kilo, a Python package for Hermes. That is a map of **where** each harness accepts an
integration, not proof of what it emits, and none of those binaries exists on this box.

This batch converts exactly the harnesses it can record live, in the order the survey ranked
them, and nothing else: `default.toml` already covers the rest honestly. Two shapes are the
interesting ones and should be first if a CLI is available — Qwen's `-session`-suffixed hook and
the Kilo/Hermes non-shell extension shapes both imply the harness reports session identity
itself, which is the pi/opencode resume problem again with a different mechanism.

## Outcome — N = 0, and why for each harness

Nothing was recorded: **no credential for any of the twelve exists on this box**, and a
question/working/idle/stress fixture needs a live turn. Four of the twelve are published on
npm as the real product and were installed into scratch and run — Copilot 1.0.83, Qwen Code
0.23.3, Kimi Code 0.42.0, Kilo 7.6.2 — and each refuses a turn for want of a credential. The
rest have no package under the probed names (`grok-cli` and `cursor-agent` resolve to
*different* tools), and Hermes is a Python package, so npm is the wrong registry to probe for
it. Per-harness lines and the verbatim transcript:
`.loop/evidence/T-0082/scope-note.md` + `transcript.txt`.

## Acceptance criteria

- [x] N adapters (N ≥ 0) each with ≥4 live-recorded fixtures and the CLI version named in the
      TOML header — **N = 0**, stated in the evidence as criterion 1 allows. No TOML was
      written, so there is no header to name a version in.
- [x] For every harness attempted but not recorded, one evidence line naming why — twelve
      lines in `.loop/evidence/T-0082/scope-note.md`, each naming the cause (`binary absent` /
      `no credentials` / `plugin host unavailable`) and, where a package resolved, the exact
      refusal text the CLI printed.
- [x] `cargo xtask adapters --check` green with the total fixture count stated: **24 passed,
      0 failed** — unchanged, which is the honest evidence that nothing was fabricated.
- [x] No adapter written from a registry entry alone: `adapters/` holds only the recorded
      TOMLs (pi, opencode, omp, default) and the check proves they still parse and replay.

## Follow-up

T-0089 — the retry, gated on a credential appearing on the box. It carries the four verified
versions from this task and the codex/[CC] resume argv from T-0081's note.

## Notes

- Inputs: `specs/harness-matrix.md` (T-0075) and `.loop/evidence/T-0075/herdr-integrations.txt`.
- The merge question the task asked ("if a batch of these shares one mechanism, one task per
  mechanism") is **not answerable yet**: it needs recordings. Filed as an input to T-0089
  rather than answered from the registry — Qwen's `-session` hook and Kilo/Hermes's non-shell
  plugins do look like one mechanism (the harness reports its own session identity), but
  "looks like" is not a recording.
- The only agent CLIs installed on this box are `pi`, `opencode` and `omp`, all already
  adapter-covered, so there was no local harness to record instead.
