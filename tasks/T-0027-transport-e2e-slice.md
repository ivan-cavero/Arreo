---
id: T-0027
title: Transport e2e slice — real sockets, real pairing, hostile cases as PASS/FAIL
phase: 2
priority: 4
status: obsolete
depends_on: [T-0023, T-0024, T-0025, T-0026]
scope:
  - xtask/src/transport_slice.rs
  - xtask/src/main.rs
  - .github/workflows/*
  - .loop/evidence/T-0027/**
---

## Goal

One command proves the whole Phase 2 security slice the way `--slice tui`/`--slice theme`
proved the TUI (T-0015/T-0016): real `arreo-server` and `arreo-cli` processes, real loopback
QUIC sockets, real frames and pairing transcripts, and the hostile cases these features
exist for — each an explicit PASS/FAIL line, evidence in `.loop/evidence/T-0027/`.

## Acceptance criteria

- [ ] `--slice transport` and `--slice pairing` implemented in `xtask/src/transport_slice.rs`
      and registered in `xtask/src/main.rs` (match arm, the `unknown slice` have-list, usage
      line) — this task wires the two slices, so the slice names in T-0023/T-0024/T-0026
      become runnable here.
- [ ] `transport` slice: real daemon, real QUIC, real Noise-KK handshake, asserting
      attach → snapshot → delta → resume parity with a Unix-socket run in the same process;
      then the negative cases — one tampered ciphertext byte → typed decrypt error, zero
      plaintext, registry unchanged; a replayed handshake flight → refused, no session. The
      local Unix socket is exercised in the same run and stays green (T-0023's other half).
- [ ] `pairing` slice: real `arreo pair` on the server vs a second real client binary with
      its own identity dir. Asserts: happy path issues a cert and opens a remote session;
      wrong code leaves the identity dir byte-identical (tree hash); replayed transcript
      refused; TTL expiry (injected as 1 s, never a real 5-minute sleep) kills the window;
      revoke → next connection refused; daemon restart → still refused.
- [ ] Mutation control: `--weakened` (test-only cfg, `ARREO_TEST_WEAKEN_CRYPTO`) runs the
      same slice with the tamper and replay checks disabled, and every affected check must
      report FAIL — captured in evidence. This proves the negative assertions bite instead
      of passing vacuously. The weakened path is compiled only under that cfg, never in a
      shipping binary.
- [ ] CI: `transport` and `pairing` added as steps in `.github/workflows/ci.yml` next to
      the tui/theme steps, hermetic (loopback only, no external network), whole set < 60 s
      so the full e2e suite stays inside the §10.1 five-minute budget.
- [ ] Evidence on `--interactive-evidence`: per-check PASS/FAIL transcript, typed client
      errors, identity-dir hashes, audit rows (no key material) in `.loop/evidence/T-0027/`.
- [ ] The slice fails loudly (nonzero exit) if the daemon cannot start, a socket is not
      real, or any negative case is not exercised — no silent skips.

## Notes

- Mirrors T-0015/T-0016: real binaries, real transports, assertions on the wire and the
  on-disk state, committed evidence — the referee is executable, not prose (§10.1).
- Shared-file note: this task edits `xtask/src/main.rs` and `.github/workflows/ci.yml` for
  `transport`/`pairing`; T-0028 adds a `compat` slice arm to the same two files. Land this
  one first (T-0028 is later in the phase) and keep each edit to its own match arm / step so
  the two changes do not conflict.
- Sibling work in the same phase (relay v0 routing/presence, mesh, auto-update) gets its own
  slices; this task does not assert relay routing, only the machine-local crypto and pairing
  behavior — honest fence, not an omission.
- Honest gaps: the LAN-direct path is exercised on loopback only (no mDNS discovery here,
  §3.4 calls it a bonus); the relay-routed path is asserted once the relay v0 binary lands
  (prose dependency, no dangling task id).

## Verification

```console
cargo xtask e2e --slice transport
cargo xtask e2e --slice pairing
cargo xtask e2e --slice transport --weakened
```

The `--weakened` run is expected to report the tamper/replay checks as FAIL.

## Retired (2026-09-12) — superseded, not deferred

`status: obsolete`. The task's goal was "one command proves the whole Phase 2 security slice", and
every behavior it names is already asserted — at the same level (real binaries, real loopback QUIC
sockets, real frames, real identity dirs) and already running in CI. Building the slice would be a
second home for the same assertions, which §5.1 forbids: a test that duplicates another is deleted,
not written.

Where each criterion lives today:

| T-0027 criterion | Proven by |
| --- | --- |
| attach → snapshot → delta → resume parity, local socket green | `crates/arreo-server/tests/api.rs`, `crates/arreo-tui/tests/remote.rs`, and `--slice api` / `--slice tui` |
| a tampered handshake flight is refused | `crates/arreo-core/src/transport/noise.rs:934` (`a_flipped_byte_in_the_handshake_prevents_a_session`) |
| a tampered frame is never delivered as plaintext | `crates/arreo-core/src/transport/noise.rs:957` |
| a replayed handshake flight does not establish a session | `crates/arreo-core/src/transport/noise.rs:1124` |
| pairing happy path, wrong code leaves no trace, TTL, revoke, restart | `crates/arreo-cli/tests/pairing.rs` — six tests over three real processes, with `snapshot_tree` for the byte-identical assertion |
| two daemons + relay end to end, budgets as gates | `--slice mesh` (T-0047) and `--slice relay` (T-0034), both in CI |

The `--weakened` mutation control is retired with T-0034's, for the reason recorded there: the
experiment showed the marker/tamper scans cannot fail on their own, because what the transport hands
the relay is ciphertext — so validating them would need a crypto-weakening path in shipped code,
which costs more than it buys when the primary properties are asserted where they are decided
(`noise.rs`'s three tests above assert the refusals directly).

**What would revive this task:** if the crypto assertions ever move out of the test suite (a feature
flag, an external harness), the aggregate slice becomes the only place they run — and this retirement
should be revisited rather than assumed.
