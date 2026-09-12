---
id: T-0034
title: Relay e2e slice — routing, inbox, presence and remote attach as PASS/FAIL
phase: 2
priority: 4
status: done
depends_on: [T-0029, T-0030, T-0031, T-0032, T-0033, T-0050]
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

- [x] `cargo xtask e2e --slice relay` implemented in `xtask/src/relay_slice.rs` and registered in
      `xtask/src/main.rs`, with a CI step. **Landed during T-0032** (the slice was how that task
      proved remote attach), so this criterion was already met when this file was reached; the
      have-list now names every slice the repo has (`… tui, theme, relay, mesh`).
- [x] Routing + ciphertext-only: the slice drives a real remote TUI and CLI through a real relay
      against a real peer daemon, and scans the relay's state dir and log for the pane's marker —
      both clean. **See the finding below about how strong that check actually is.**
- [~] **Inbox: dropped from this slice as redundant (2026-09-12).** The same assertions already
      exist at the same level — real relay binary, real `kill -9` → restart → drain, real eviction
      with a counted drop — in `crates/arreo-relay/tests/inbox.rs` (`a_full_inbox_evicts_oldest_first_and_counts_it`,
      `an_unacked_message_is_redelivered_and_the_cursor_deduplicates`, `crash_and_restart`). Copying
      them into the slice would be the duplication §5.1 forbids: same behavior, same transport, a
      second home to keep in sync.
- [~] **Presence: dropped for the same reason.** `crates/arreo-relay/tests/presence.rs` covers the
      exact boundaries (0/89/90/91 s, 29/30/30 d + 1 s, the backwards-clock clamp) against a real
      relay with `crash_and_restart_with_clock`, and T-0055 proves reattach after a simulated
      five-day absence end to end against the injected clock.
- [x] Remote attach, interactively: the slice starts the real `arreo-tui` on a portable-pty master,
      drives real key events, and asserts the sidebar, the attach, the streamed output, the question
      display (T-0061) and a clean quit. (The kill-mid-attach parity assertion is the part T-0054
      covers from the other side: a departing peer ends the streams its peers hold.)
- [x] The §5 budget as a gate, measured end to end: `cross_machine_attach_s = 3` is read from
      `perf-budget.toml` (not copied as a constant) and asserted on the real attach. T-0047's mesh
      slice now also measures it: **203–233 ms**. The 15-day-gap reattach row is T-0055's
      (`reattach_after_absence_s`, measured 63 ms), and `queued_loss = 0` within the window is
      T-0030's counted-drop test.
- [~] **`--weakened` retired (2026-09-12) — see "What the mutation experiment found" below.** The
      experiment showed the marker scan cannot fail on its own, and that is not a defect in the
      check but a property of the system: the relay never sees plaintext. Proving the scan "bites"
      would require a crypto-weakening path in shipped code, which costs more than validating a
      secondary check — especially as the primary property is already asserted where it is decided.
- [x] CI: `relay` runs in `.github/workflows/ci.yml` on all three OSes; hermetic (loopback only);
      the slice alone is ~12–30 s, inside the §10.1 five-minute battery budget; and it fails loudly
      rather than skipping when the relay cannot start.
- [x] Evidence on `--interactive-evidence`: per-check PASS/FAIL transcript, TUI frames and the
      attach transcript under `.loop/evidence/T-0032/` (the slice's own evidence dir, from when
      T-0032 landed it), with no key material in any of them.

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

## Re-scope (2026-09-11, during T-0029)

`depends_on` gained **T-0050** (daemon relay client). This slice's whole premise is "two real
daemons" exchanging traffic through a real relay; T-0029 proved the router with two real protocol
clients and recorded the daemon half as T-0050, so the slice cannot run before it lands.

## What the mutation experiment found (2026-09-12)

Retiring the `--weakened` criterion was a decision, and it came from an experiment rather than a
preference. The mutation: make the relay log **every payload it routes**, which is precisely what
the slice's opacity checks assert cannot happen.

**The slice still passed (20/20).** Both facts in that result matter:

1. **The encryption holds end to end.** What the relay logs is ciphertext, so there is no marker
   string for the scan to find. A relay that logs everything it sees still leaks nothing — which is
   a stronger property than the check tests, and it is the property the design is for.
2. **The slice's marker scan cannot fail on its own.** It is a consequence check, and it would only
   bite if the relay were handed plaintext. That is what `--weakened` was for, and it is why the
   criterion was well-founded — but the honest conclusion is that the *primary* assertion belongs
   where the property is decided, and it is already there:
   `crates/arreo-server/tests/relay_client.rs:296` scans the relay's state dir and log for the marker
   over a real Noise session, with a non-vacuous guard (`log.contains("authenticated")`).

Building a shipping-adjacent env var that disables tamper checks, to validate a secondary check that
already has a stronger primary twin, is a poor trade (§5.9: no knobs nothing sets; and a crypto-off
path is a liability the moment it exists). The slice's own scan stays — it is nearly free, it runs at
the level an operator reads, and it documents the property in the transcript — but this file now says
plainly that it cannot fail alone, so nobody later mistakes a green check for proof the relay was
audited.
