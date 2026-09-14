---
id: T-0097
title: ACP adapter tier — the Agent Client Protocol as a native signal
phase: 4
priority: 4
status: proposed
depends_on: [T-0096]
scope:
  - crates/arreo-core/src/state/adapters.rs
  - crates/arreo-core/src/acp.rs
  - adapters/**
  - .loop/evidence/T-0097/**
verify:
  - cargo xtask adapters --check
---

## GATE — a live ACP-speaking agent is required, and none is installed here

`specs/harness-matrix.md` (T-0075) records which harnesses speak a protocol rather than printing
prose, and the survey's rows for ACP are `untried`: no ACP-speaking CLI is installed on this
box, and the adapter tier cannot be recorded without one. So the first step is a probe, not a
design:

```console
which pi opencode omp codex claude gemini          # the three installed (pi, opencode, omp)
pi --help 2>&1 | grep -i acp ; opencode --help 2>&1 | grep -i acp
```

If no installed harness offers ACP, **close this task with a one-command transcript** under
`.loop/evidence/T-0097/` and stop — the honest `untried` row stays `untried`. Do not write an
adapter against the protocol document alone.

## Goal

ROADMAP §6 Phase 4: "native events + ACP adapters". Today the native tier is per-harness
pattern matching; ACP is one protocol that several harnesses speak, which would make the tier
one implementation instead of N.

## Acceptance criteria

- [ ] A recorded ACP session (real CLI, real transcript) showing the event shapes: what an
      agent start, a tool call, a permission request and a turn end look like on the wire, with
      the CLI version named.
- [ ] `arreo_core::acp` parses those events into the **existing** `AgentState` transitions — no
      second state vocabulary, and the adapter registry's `acp` kind is data like any other.
- [ ] A permission request maps to `Question` with the request text as `asking`, and a reply
      maps to the protocol's own response message (not a keystroke).
- [ ] Fixtures recorded live, ≥4, replayed by `cargo xtask adapters --check` with the count
      stated.
- [ ] If the protocol turns out to be per-harness-different in practice, that is the finding:
      record it and file the shape that actually exists rather than the one the spec implies.
