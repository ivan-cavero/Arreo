---
id: T-0008
title: 10-agent load test vs the performance budget (nightly benchmark)
phase: 0
priority: 4
status: todo
depends_on: [T-0002, T-0003, T-0004, T-0006]
scope:
  - xtask/src/**
  - perf-budget.toml
  - crates/arreo-server/**
---

## Goal

Turn ROADMAP §5 into executable law: `perf-budget.toml` + `cargo xtask bench` replaying
30-pane-shaped traffic, asserting RSS and latency budgets; nightly CI job.

## Acceptance criteria

- [ ] `perf-budget.toml` encodes §5 budgets (RSS ≤ 120 MB @ 30 panes, ≤ 3 MB/pane, ≤ 200 ms
      detection).
- [ ] Load harness: spawn 10 real panes, replay [CC]-shaped output (recorded fixtures),
      sample RSS/latency, compare against budgets, exit non-zero on regression.
- [ ] Nightly workflow runs it; results posted as CI artifacts (chart optional).
- [ ] Phase 0 exit demo wired: `xtask bench` output is the evidence artifact.

## Verification

```console
cargo xtask bench --panes 10
```

## Notes

Phase 0 exit demo (ROADMAP §6): 10 agents, live states, < 100 MB RSS, TUI-less CLI —
proven on Linux, macOS, Windows. This task produces that proof automatically.
