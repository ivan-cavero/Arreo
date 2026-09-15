---
id: T-0114
title: FFI: the metrics reads a RAM meter needs
phase: 3
priority: 2
status: proposed
depends_on: [T-0104, T-0040]
scope:
  - crates/arreo-core-ffi/**
  - docs/mobile.md
  - .loop/evidence/T-0114/**
verify:
  - cargo test -p arreo-core-ffi
  - cargo xtask ffi --check
---

## Goal

Phase 3's "RAM meters": a phone shows each agent's memory over time. The data exists —
T-0040's `metrics_series` table and the `MetricsHistory`/`MetricsSeries` socket verbs — and
T-0104 gave the client a typed boundary. What is missing is the boundary's half: the session
handle can dial, drain, ack, heartbeat and read the directory, but it cannot ask for metrics.

## Acceptance criteria

- [ ] `RelaySessionHandle` gains the two reads the meter needs: a history request (pane, step,
      window) and a series request — **the same two verbs the CLI uses**, no new protocol.
- [ ] The reply crosses as a typed record (`WireMetricsPoint`-shaped: ts, avg/peak RSS, cpu,
      pids), and the downshift note T-0040's verbs carry crosses too — a UI that silently shows
      a coarser step than it asked for is showing the wrong graph.
- [ ] An empty series is an empty list, never an error: a pane that just started has no
      history, and that is a state a meter renders (T-0040's own rule).
- [ ] The contract test drives the exported reads through a real session against a test relay,
      as `the_session_dials_drains_acks_and_reads_the_directory` does for the directory.
- [ ] `docs/mobile.md` gains the two reads and what a meter must do about the step.

## Notes

- Deliberately no charting, no smoothing, no formatting: that is the UI's, and this crate
  carries data, not presentation.
