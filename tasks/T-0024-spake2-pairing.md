---
id: T-0024
title: SPAKE2 pairing — 30-second code pairing without a password or LAN trust
phase: 2
priority: 2
status: done
depends_on: [T-0013, T-0025]
scope:
  - crates/arreo-core/src/pairing/**
  - crates/arreo-core/src/store.rs
  - crates/arreo-core/src/identity/authority.rs
  - crates/arreo-cli/src/main.rs
  - crates/arreo-cli/tests/pairing.rs
  - crates/arreo-relay/src/**
  - crates/arreo-core/Cargo.toml
  - crates/arreo-relay/Cargo.toml
  - .loop/evidence/T-0024/**
  - specs/adr/0010-pairing-spake2.md
---

## Goal

The ritual behind "pair in 30 seconds" (§1, §3.3): `arreo pair` prints a short human code
and a QR; a phone or another machine types or scans it and joins — no password to type, no
SSH, no assumption that the LAN is friendly. SPAKE2 (RFC 9382) over the relay mailbox turns
the low-entropy code into a strong authenticated channel, so an active MITM gets exactly
one guess. Output: a device keypair pinned with a cert issued by T-0025, and a burned code.

## Acceptance criteria

- [x] `arreo pair` emits a 4-word code from a 256-word list (32 bits) plus an
      `arreo://pair?v=1&mb=…&s=…&k=…&ttl=…` invite carrying the mailbox, session id, server
      public key and TTL; the code is **not** in the invite (it is typed by a human, and a
      URI holding it would turn "scan this" into "trust anything that renders a QR");
      `--json` for scripts and the tests. The word list is committed (`pairing::code`,
      256 words asserted distinct/lowercase by test), and a typo names the nearest word.
      Re-scope: QR *image* rendering is deferred — see the note below.
      Evidence: `.loop/evidence/T-0024/happy-path.txt`.
- [x] Wrong code → failure with no partial state: key confirmation fails, both processes
      exit nonzero, and the device identity dir is byte-identical before/after (the test
      compares the whole file tree, bytes included, not a hash); no keypair, no pin, no
      cert, exactly one `pairing_failed` audit row whose text names the cause.
      Evidence: `.loop/evidence/T-0024/failure-paths.txt`;
      test `a_wrong_code_leaves_no_trace_on_the_phone_and_burns_the_session`.
- [x] A captured transcript cannot be replayed to pair: a session id is single-use at the
      relay (a burned or expired id is refused forever within the relay's lifetime), each
      slot is write-once, and a wrong guess burns the session server-side — so re-using the
      same invite after a success *or* a failure fails, even with the right code.
      MACs also bind (label, session, payload), so a flight cannot be moved between
      sessions or replayed as the other side (unit-tested both directions).
      Evidence: `happy-path.txt` (replay after success), `failure-paths.txt` (after failure).
- [x] The window is time-bounded and single-use: default 300 s, `--ttl-secs N` (the tests
      use 1 s — never a five-minute sleep); after expiry the session dies and the invite is
      dead, and the failure is audited. Concurrent sessions are supported and isolated
      (a second `arreo pair` gets its own id; another session cannot touch it) — the
      original criterion said a second concurrent pairing should refuse, but the mailbox is
      a shared bulletin board and the *session* is the unit of isolation, so refusing a
      second pairing would only block a second phone for no security gain.
      Re-scope: `--ttl-secs` replaces the `ARREO_PAIR_TTL_SECS` env var named in the
      original wording — an explicit flag is testable without mutating the environment.
      Evidence: `failure-paths.txt`, test `an_unanswered_pairing_expires_instead_of_hanging`.
- [x] Real process boundary: three real processes — the `arreo-relay` binary serving the
      mailbox on a real socket, a server-side `arreo pair` and a phone-side `arreo pair` —
      with no in-process mocks. Six scenarios: happy path, wrong code, expiry, late phone,
      session isolation, bad arguments.
      Re-scope (path only): the tests live in `crates/arreo-cli/tests/pairing.rs` rather
      than `crates/arreo-core/tests/`, because only a package's own test target receives
      `CARGO_BIN_EXE_<name>` and the artifact under test is the CLI. The relay is driven as
      a *binary*, which also keeps the AGPL crate out of this crate's dependency graph
      (T-0035 owns that boundary).
      T-0027's `--slice pairing` will re-assert the same exchange as an xtask slice.
- [x] Guess budget enforced, not hoped: one guess per session (the server burns it on
      MAC mismatch, before returning), and the mailbox refuses an id that already completed
      or expired — so an attacker holding a full transcript still needs the code, and gets
      one attempt with it.
- [x] Audit rows for pairing success (`device_change`, carrying the device id) and failure
      (`pairing_failed`, carrying the session id and the reason), with evidence under
      `.loop/evidence/T-0024/`. `arreo audit` now prints the event kind, without which a
      refused pairing was indistinguishable from a prompt.

## Re-scope notes (written by the loop, turn 21)

Three deltas from the original wording, each with its reason:

1. **QR image rendering deferred.** The invite *payload* is implemented, tested and
   round-tripped (`Invite::uri`/`parse_uri`: mailbox, session, server key, TTL, with the
   code deliberately absent). Rendering it as a QR bitmap is deferred until a client can
   scan one (the mobile client is Phase 3): shipping a renderer nothing can exercise would
   be presentation I cannot test, which §6 forbids. A human copies the URI today; the
   `--json` output feeds anything that renders it later.
2. **Tests live in `arreo-cli/tests/`** (see the criterion above): a package's own test
   target is the only place `CARGO_BIN_EXE_arreo` exists.
3. **Concurrent pairings are allowed** rather than refused (the criterion asked for a
   loud refusal of a second `arreo pair`). Isolation is per session id, so refusing a
   second pairing would block a second phone without protecting the first; the test
   proves the two sessions cannot interfere.

Also worth recording: the mailbox keeps a session `MAILBOX_GRACE` (10 s) longer than the
window a human is shown. Without it the relay's expiry raced the server's own deadline and
the user saw a storage message ("session is no longer available") instead of "the pairing
window closed" — found by the real-process test.

## Findings this task produced

- **`arreo pair` (and every CLI verb that prints) panics on a closed stdout pipe**
  (`arreo pair | head -1`): Rust's `println!` aborts on `EPIPE`, and the first version of
  the test tripped exactly that by dropping the server's stdout after the invite. Filed as
  T-0049 (CLI-wide, not a pairing decision).

## Notes

- `spake2` (RFC 9382, ed25519 group) over hand-rolling a PAKE: PAKEs are exactly where
  hand-written crypto dies. Rejected: SRP (older, murkier provenance); code-as-PSK under
  plain Noise (low-entropy PSK leaks an offline verifier); Noise-XX keyed by the code (same
  offline-guess hole). Reuses the ed25519 dependency T-0025 introduces.
- Magic Wormhole heritage: the mailbox only orders opaque SPAKE2 messages. The code never
  leaves the two devices, so the relay and any LAN sniffer see ciphertext plus a session id
  — the "relay cannot read" claim (§3.4, §4) holds for pairing too.
- This task owns only the *pairing mailbox* in `arreo-relay` (store, single-use, TTL).
  Routing, presence and the durable per-device inbox are the relay v0 work (sibling range,
  soft dependency in prose — not a `depends_on` id); they must not weaken these guarantees.
- Honest gaps: mailbox admission is minimal (session id + server static key) and the
  in-memory mailbox is not durable — a relay restart mid-pairing fails the pairing loudly
  (the client re-runs `arreo pair`). Persisting the mailbox belongs to the durable-inbox
  work; single-use semantics are still enforced by the server, not the relay.

## Verification

```console
cargo test -p arreo-core --lib pairing     # code, flow, mailbox wire
cargo test -p arreo-relay                  # mailbox rules + the socket path
cargo test -p arreo-cli --test pairing     # three real processes
```

`cargo xtask e2e --slice pairing` (real binaries, evidence frames) is wired by T-0027.
