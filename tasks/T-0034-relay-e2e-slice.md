---
id: T-0034
title: Relay e2e slice — routing, inbox, presence and remote attach as PASS/FAIL
phase: 2
priority: 4
status: proposed
depends_on: [T-0029, T-0030, T-0031, T-0032, T-0033]
scope:
  - xtask/src/relay_slice.rs
  - xtask/src/main.rs
  - .github/workflows/ci.yml
  - .loop/evidence/T-0034/**
---

## Goal

One command proves the Phase 2 remote stack the way `--slice tui`/`--slice theme` proved the
local TUI (T-0015/T-0016) and `--slice transport` proves crypto (T-0027): real `arreo-relay`,
two real daemons, a real remote TUI on a pty, a real SQLite state dir, each behavior reported as
an explicit PASS/FAIL line. This task wires the `relay` slice, so the slice name referenced by
T-0029…T-0033 becomes runnable here.

## Acceptance criteria

- [ ] `cargo xtask e2e --slice relay` implemented in `xtask/src/relay_slice.rs` and registered in
      `xtask/src/main.rs` (module, match arm, the `unknown slice` have-list and the usage line) —
      the have-list names every slice the repo actually has.
- [ ] Routing + ciphertext-only: two daemons exchange an envelope through a locally-run relay on
      loopback; a full scan of the relay's state dir, logs and stdout for marker strings from the
      exchanged content finds none, and the wire capture shows no decrypted byte.
- [ ] Inbox: device offline → messages published → reconnect → drained in `seq` order exactly
      once (a duplicate-frame injection produces no second delivery) with the relay restarted in
      between; then the eviction path — push past small configured bounds and assert oldest-first
      drops with a `dropped` count reaching the client.
- [ ] Presence: connect → `online`; injected clock past 90 s → `offline` with an age; past 30 d →
      `stale`; `kill -9` + restart → nothing reads online until it reconnects. No real sleeps —
      the slice injects the same clock the unit tests use.
- [ ] Remote attach, interactively: the slice starts the real `arreo-tui --remote` on a
      portable-pty master, drives real key events and captures frames — sidebar parity with a
      local attach, focus/attach/send round-trip, then the relay connection is killed mid-attach
      and the post-reconnect screen is asserted byte-identical to the control run (no duplicated
      lines, no gap).
- [ ] Budgets as gates, measured end-to-end and not merely claimed: `cross_machine_attach_s = 3`
      (§5), reattach after the simulated 15-day gap < 3 s, `queued_loss = 0` within the retention
      window; the numbers land in `.loop/evidence/T-0034/`.
- [ ] Mutation control: `--weakened` (test-only `ARREO_TEST_WEAKEN_CRYPTO`, mirroring T-0027)
      runs the slice with the tamper/ciphertext checks disabled and every affected check must
      report FAIL, proving the negative assertions bite; the weakened path compiles only under
      that cfg, never in a shipping binary.
- [ ] CI: `relay` added to `.github/workflows/ci.yml` beside the tui/theme/transport steps;
      hermetic (loopback only, no external network), the slice alone < 60 s so the battery stays
      inside the §10.1 five-minute budget; fails loudly if the relay cannot start or a check was
      skipped.
- [ ] Evidence on `--interactive-evidence`: per-check PASS/FAIL transcript, TUI frames, the
      drop/reconnect capture, the ciphertext scan report and the timing numbers under
      `.loop/evidence/T-0034/`, with no key material in any of them.

## Notes

- Crates: `xtask` only (dev tooling, never shipped, so it may drive the AGPL binary without touching
  the license boundary T-0035 asserts).
- Shared-file note: this task edits `xtask/src/main.rs` and `.github/workflows/ci.yml`, which
  T-0027 and T-0028 also touch — one match arm and one CI step each; land after T-0027.
- Honest gaps: loopback only (no NAT path, no push wakeup, no LAN-direct fallback — §3.4 calls it a
  bonus); `--weakened` is deliberately a failing invocation, so not a CI step.

## Verification

```console
cargo xtask e2e --slice relay
cargo xtask e2e --slice relay --interactive-evidence
cargo xtask e2e --slice relay --weakened
```

The `--weakened` run is expected to report the tamper/ciphertext checks as FAIL.
