---
id: T-0040
title: Metrics history — durable per-agent series with a bounded store
phase: 2
priority: 3
status: proposed
depends_on: [T-0006, T-0018]
scope:
  - crates/arreo-core/src/metrics/**
  - crates/arreo-core/src/store.rs
  - crates/arreo-core/tests/metrics_history.rs
  - crates/arreo-core/src/proto/**
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-tui/src/ui.rs
  - xtask/src/tui_slice.rs
  - perf-budget.toml
  - .loop/evidence/T-0040/**
---

## Goal

The other half of pillar P3: the T-0006 sampler is live-only, so "what did this agent do at
3 a.m." is unanswerable. Persist it as a real time series — 10 s raw, 1 m and 1 h rollups,
pruned hard, queryable from CLI, socket API and TUI (§3.1 metrics sampler, §5 storage
discipline, Phase 2 "cgroups limits + metrics history").

## Acceptance criteria

- [ ] Three tiers with explicit numbers: 1 s in-RAM samples (live only, last 5 min), 10 s rows
      kept **24 h**, 1 m rollups kept **30 d**, 1 h rollups kept **365 d**. Every tier stores
      average **and** peak RSS (peak is the number people act on) plus cpu and pids.
- [ ] Schema v3 migration in `store.rs` along T-0018's versioned path (data never dropped); rows
      are keyed `(pane, ts_ms, step_ms)`, so re-running a rollup is idempotent, not duplicating.
- [ ] Retention enforced by an hourly prune tick that never deletes the newest row of a live pane
      (a graph never goes empty). An integration test seeds 400 days of synthetic samples, advances
      the clock, prunes, then asserts per-tier row counts and **≤ 2 MB per pane** on disk.
- [ ] Query surface: `arreo metrics history <pane> --since 6h --step 1m [--json]` and the
      socket request `Metrics { pane, since_ms, until_ms, step_ms }` — one indexed range scan
      on `(pane, ts_ms)`. Asking finer than available downshifts to the nearest real step and
      says so (`--step 1s` over 6 h reports 10 s) instead of returning empty; an unknown pane
      gives an empty series plus a clear message, not an error.
- [ ] N−1 safe: `Metrics` is an optional-field addition to T-0013/T-0014's protocol — a client
      that omits it still receives the live payload, and a server without history reports
      "history unavailable" rather than zeros, which would be a lie.
- [ ] TUI: the focused pane shows a RAM sparkline from the same series, labelled with the peak
      and, when T-0019 enforcement is active, the budget line. Evidence is a scripted frame from
      `cargo xtask e2e --slice tui --case metrics-graph` (real pty, real key events).
- [ ] Overhead unchanged: `cargo xtask bench --probe metrics` still shows the sampler ≤ 1% CPU at
      30 panes with rollups enabled, and the 10 s writer does not let the tick cadence drift
      (asserted from timestamps in the slice).

## Notes

- A graph, not a dashboard: no cross-pane aggregation, no alerting (T-0041 owns that), no
  pricing-gated retention tiers (Phase-5 relay policy) — local truth stays unlimited for
  self-hosters, which is the point of the self-host tier.
- Rollups come from the tier below, never from re-sampling `/proc` — a pane restored late must
  not leave a hole where the underlying 10 s row exists.
- Storage math is an acceptance bar, not a comment: 1 h rows dominate the tail at roughly
  350 KB per pane-year, so 30 panes × 1 year ≈ 11 MB fits inside ≤ 2 MB/pane once the 10 s tier
  is pruned at 24 h.
- Honest gap: cgroup-level pressure (`memory.current` including children, OOM events) is
  *not* this series — T-0041 adds it over the same store so both lines render on one graph.
- Scope note: the TUI change stays small (a sparkline in the existing focused-pane view, no new
  layout) and extends T-0015's slice with one case rather than adding a slice; evidence lands in
  `.loop/evidence/T-0040/` beside the CLI transcript.

## Verification

```console
cargo test -p arreo-core --test metrics_history
cargo xtask e2e --slice tui --case metrics-graph
cargo xtask bench --probe metrics
```
