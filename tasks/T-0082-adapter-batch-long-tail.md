---
id: T-0082
title: Adapter suite v2 batch C — the extension-family long tail, one TOML per recorded harness
phase: 4
priority: 4
status: proposed
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

## Acceptance criteria

- [ ] N adapters (N ≥ 0) each with **≥4 live-recorded fixtures** and the CLI version named in the
      TOML header. The count is stated in the evidence; a batch that records nothing is a valid
      outcome **if** it says so.
- [ ] For every harness attempted but not recorded: one evidence line naming why (binary absent,
      no credentials, plugin host unavailable).
- [ ] `cargo xtask adapters --check` green with the total fixture count stated.
- [ ] No adapter is written from a registry entry alone — the registry says where the hook goes,
      which is why it is the seed list and not the deliverable.

## Notes

- Inputs: `specs/harness-matrix.md` (T-0075) and `.loop/evidence/T-0075/herdr-integrations.txt`.
- If a batch of these turns out to share one mechanism, one task per mechanism is better than
  one per harness — say so in the evidence and file the merge as a note rather than doing it
  silently.
