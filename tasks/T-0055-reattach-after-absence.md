---
id: T-0055
title: §3.14 reattach — a machine that was away for weeks reaches its overview in seconds
phase: 2
priority: 3
status: done
depends_on: [T-0030, T-0032, T-0050]
scope:
  - crates/arreo-core/src/relay/session.rs
  - crates/arreo-core/src/relay/client.rs
  - crates/arreo-relay/src/inbox.rs
  - crates/arreo-relay/src/main.rs
  - crates/arreo-server/tests/relay_client.rs
  - crates/arreo-tui/tests/remote.rs
  - perf-budget.toml
  - .loop/evidence/T-0055/**
---

## Goal

ROADMAP §3.14's promise: a machine that was switched off for weeks comes back and is *usable in
seconds*, with the messages that accumulated while it was away delivered in order and applied once.
T-0030 built the durable inbox and its counted drops; T-0032 built the client that reconnects. This
task proves the two together across a simulated absence, and it is the last open criterion of T-0032
(split out because it needs a clock the relay does not yet let a test control).

## Acceptance criteria

- [x] The relay's clock is injectable for tests: `arreo-relay serve` accepts an offset (a hidden
      flag or an environment variable, the pattern `ARREO_TRANSPORT_TEST_LISTEN` already set for the
      loopback transport seam) so a test can make "15 days passed" a fact about the relay's view
      rather than a real sleep. Production reads the real clock; the seam changes nothing when unset.
- [x] The inbox TTL and bounds are settable per run (T-0030 made them operator-settable — this wires
      the *test* door, and the criteria below depend on it).
- [x] A simulated 15-day absence, end to end with real processes: a client attaches, disconnects, the
      clock advances past the retention window, queued messages are enqueued while it is away, it
      reattaches, and it reaches a full overview (sidebar + the focused pane's scrollback) in
      **< 3 s** — asserted against a budget row, not a constant in the test.
- [x] The drained batch is delivered in `seq` order, and re-running the drained batch re-executes
      nothing: the ack + cursor rule (T-0030) holds through the client, so a second drain of the same
      range yields nothing. Asserted by draining twice and comparing.
- [x] What the retention window dropped is *reported*, not silent: the drop counters (T-0030) appear
      where an operator can see them, and the test asserts the count matches what it enqueued past
      the bound.
- [x] A row in `perf-budget.toml` for the reattach target, enforced by the test that measures it
      (the pattern T-0032 set for `cross_machine_attach_s`).
- [x] Evidence `.loop/evidence/T-0055/`: the reattach transcript with timings, the drain order, the
      second drain finding nothing, and the drop counters.

## Landing notes (2026-09-11)

The clock seam is an environment variable read once (`ARREO_CLOCK_OFFSET_MS`), not a flag and not a
per-call hook, and the reason is retention's own arithmetic: expiry is a comparison between two
timestamps, and a clock that could move between the write and the sweep would make "expired" a
function of scheduling rather than of time. Naming it in `arreo_relay::directory` (next to
`now_ms`) keeps one spelling for the test and the reader, and the unit test pins that the seam
changes nothing when unset — a test-only offset that leaks into production would be a retention
correctness bug, not a convenience.

Two things the test taught, both now in the test rather than smoothed over:

- **A restart ends every session.** The first draft kept Bob connected across the clock-moving
  restart and failed with "connection lost" on the next send. That is not a defect — a kill is a
  kill — but a test that assumes otherwise is testing a socket, not retention. Both clients
  reconnect after the restart, which is also the shape a real absence has (nobody's connection
  survives three weeks).
- **The reattach budget is the file's number, not the test's.** The test reads
  `reattach_after_absence_s` out of `perf-budget.toml` the same way `bench` does (and with the same
  no-TOML-dependency parse), so the law is in one place. Measured: **63 ms** against 3 s for a
  five-day absence with a drain of three.

## Notes

- Why it is not part of T-0032: T-0032 is a *transport* task (one client, two transports, a drop and
  a resume), and it is done. This is a *durability* task — it exercises T-0030's inbox and its
  retention semantics through a client, and its blocker is the relay's clock, which is relay-side
  work with its own risk (a clock seam that leaks into production would be a correctness bug in
  retention).
- The alternative to clock injection — a real 15-day wait — is not a test. A short TTL with a real
  sleep is the tempting middle, and it is worse: it makes the suite slow *and* still does not exercise
  the window it claims to, because the messages must be enqueued while the client is away and then
  expire.
- Honest gap this task inherits rather than fixes: messages enqueued before the window are gone, and
  the counters are the only record. That is T-0030's stated contract (at-least-once inside the
  window, counted drops outside it), and the criterion here is that the count is visible, not that
  nothing is ever dropped.
