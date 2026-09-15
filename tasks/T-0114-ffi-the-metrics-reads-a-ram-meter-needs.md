---
id: T-0114
title: FFI: the metrics reads a RAM meter needs
phase: 3
priority: 2
status: done
depends_on: [T-0104, T-0040]
scope:
  - crates/arreo-core-ffi/**
  - docs/mobile.md
  - .loop/evidence/T-0114/**
verify:
  - cargo test -p arreo-core-ffi
  - cargo xtask ffi --check
evidence:
  - .loop/evidence/T-0114/ffi-metrics-reads.txt
  - .loop/evidence/T-0114/real-daemon-run.txt
---

## Goal

Phase 3's "RAM meters": a phone shows each agent's memory over time. The data exists —
T-0040's `metrics_series` table and the `MetricsHistory`/`MetricsSeries` socket verbs — and
T-0104 gave the client a typed boundary. What is missing is the boundary's half: the session
handle can dial, drain, ack, heartbeat and read the directory, but it cannot ask for metrics.

## Acceptance criteria

- [x] `RelaySessionHandle` gains the two reads the meter needs: a history request (pane, step,
      window) and a series request — **the same two verbs the CLI uses**, no new protocol.
- [x] The reply crosses as a typed record (`WireMetricsPoint`-shaped: ts, avg/peak RSS, cpu,
      pids), and the downshift note T-0040's verbs carry crosses too — a UI that silently shows
      a coarser step than it asked for is showing the wrong graph.
- [x] An empty series is an empty list, never an error: a pane that just started has no
      history, and that is a state a meter renders (T-0040's own rule).
- [x] The contract test drives the exported reads through a real session against a test relay,
      as `the_session_dials_drains_acks_and_reads_the_directory` does for the directory.
- [x] `docs/mobile.md` gains the two reads and what a meter must do about the step.

## Notes

- Deliberately no charting, no smoothing, no formatting: that is the UI's, and this crate
  carries data, not presentation.

## Review findings and the decided fix (planner, after a security review)

A `security-reviewer` pass over the first implementation found that **the read works once and
fails from then on against a real daemon** — the contract test could not see it because its
fixture is not a real daemon. Verdicts it established: the peer key **is** genuinely pinned
(`SecureChannel::connect` feeds it into Noise-KK as the remote static, so a substituted key
cannot complete the handshake), and **the daemon's per-verb gate and role semantics do apply**
(a relay peer runs the same `serve_session` behind the same `check_verb` + trust ledger;
`Message::MetricsHistory` → `Verb::Metrics` → `Capability::Observe`, which a Viewer holds — the
intended rule, identical to the CLI's remote path).

### The defect (p1)

`metrics_history` opened a **fresh stream and a fresh Noise handshake per call** and "closed" by
dropping the `PeerSession`. Nothing goes on the wire when it drops: the relay protocol has no
per-stream close (`PeerGone` exists only for a device's *relay session* ending), and the daemon's
`serve_session` **keeps the session open after a verb**. So the second call's bytes — a 32-byte
cleartext hint plus a new handshake flight — arrive in the *stale* session, whose pump reads the
hint as a u16-BE frame length (`'d','e'` ≈ 25,701 bytes, under the 60,018 budget) and waits for
bytes that never come. The phone's one unretried 10 s attempt then fails, and it keeps failing
until the stale pump finally accumulates enough to error.

The core's own client knows this failure mode: `mesh/session.rs` retries on fresh streams
(`REMOTE_HANDSHAKE_ATTEMPTS`, 250 ms pause) with the comment *"the peer may still be holding an
earlier stream — the far end needs its next read to fail before it will accept a new stream"*.
The FFI had no retry, and a retry alone would not fix it, because a long-lived phone session
never produces the `PeerGone` that ends the stale conversation.

**Verified independently before acting** (both claims, not just read): the fixture does
`writer.shutdown()` + `FINAL_FRAME_GRACE` after each answer, which forces the *reconnect* path
for the next call — the client's expectation, not the daemon's behaviour; and the core client's
retry-and-pause loop with that comment is real.

### The decided fix

**One long-lived daemon conversation per peer, reused for every verb.**

1. `RelaySessionHandle` caches the conversation (`PeerSession`) keyed by peer, under one
   `tokio::sync::Mutex`. `daemon_to` returns the cached conversation when it has one and
   handshakes only when it does not: one handshake, many verbs.
2. **Per-peer serialization** is the same lock. Concurrent reads (two meters, or a refresh
   racing a poll) must not interleave two conversations on one channel: `stream_to` *replaces*
   the live peer's routing (`stream_for` inserts over the live entry), so the older stream would
   never see a reply while both write the same per-device wire channel. The existing comment
   claiming a caller "cannot accidentally create two streams whose chunks would interleave"
   states the inverse of the truth — correct it. (Review finding B; the lock subsumes it.)
3. **Recovery without a retry loop**: a call that fails with a transport/IO error drops the
   cached conversation, so the next call handshakes cleanly. Probe the retry shape against a
   real daemon rather than guessing.
4. **Bound the send** with the same `DAEMON_REPLY_TIMEOUT` as the read (the write half was
   unbounded: a peer that stops reading stalls `write_all` once the 64 KB duplex fills), and
   **fail fast on a non-truncated frame error** rather than treating every `decode_frame` error
   as "keep reading" — an over-budget declared length is currently indistinguishable from an
   incomplete frame, so the loop appends until the timeout instead of refusing. (Findings C/D.)
5. **The Noise identity hint comes from this device's own key**, not from the relay's
   `AuthReply::Welcome` echo. The hint is an identity *assertion* on this path; it is currently
   the relay's word. Derive it locally (`DeviceId::from_key(&self.device.key_ref().public())`);
   the failure mode is fail-closed today, but a lying relay should not be able to make this
   device announce another identity. (Finding E.)
6. **The fixture must model a real daemon**: keep one conversation open and answer *N* verbs over
   it, rather than closing after each answer. That is the missing proof — without it the two
   sequential reads "prove" the one case a real daemon breaks.
7. **Falsify the pinning claim**: a *wrong-but-valid* key (another device's public key) must be
   refused with `SessionFfiError::Peer(_)`. The existing negative case passes a *malformed* key,
   which `verifying_key_from_hex` rejects before any stream exists — so the test as written would
   pass unchanged if `metrics_history` ignored `server_key` entirely and trusted the relay's
   directory. (Finding F.)
8. Retest against a **real `arreo-server`**, not only the fixture.

### Accepted as-is

- A Viewer may read metrics: that is the intended capability (`Capability::Observe`), identical
  to the CLI's remote path. Not a finding.
- `finish`'s `handshake_remote != expected_remote` check is a tautology on the initiator side
  (snow returns the static the builder was given); pinning is enforced by the KK DH, not that
  comparison. The comment overstates what it adds — noted, and the transport file is outside this
  task's fence.

## Outcome

Done — and the first implementation was **green and wrong**, which is the part worth reading.

It passed the whole battery (936 tests, 14/14 slices, bench 6/6, vet, deny, audit, check-targets)
and was broken against a real daemon: the read worked once and failed from then on. It opened a
fresh Noise handshake per call and "closed" by dropping the session, but nothing goes on the wire
when it drops — the relay has no per-stream close, and a daemon keeps its session open after a
verb — so the second call's bytes landed in the stale session, whose pump read the 32-byte
identity hint as a ~25 KB frame length and waited. The contract test passed because its fixture
closed after each answer: the client's expectation, not a daemon's behaviour.

A security review found it, and established the two things the design rests on: the peer key **is**
genuinely pinned (it becomes Noise-KK's remote static, so a substituted key cannot complete the
handshake) and the per-verb gate **does** apply (a Viewer may read metrics — the intended
`Capability::Observe`).

The fix, pinned in this file before dispatch and implemented against it: one cached long-lived
conversation per peer under one mutex (one handshake, many verbs; the lock also stops concurrent
reads interleaving two conversations on one channel), unconditional invalidation on error so
recovery needs no retry loop, a bounded send, fail-fast on a non-truncated frame error, the Noise
identity hint derived from this device's own key with a dial-time assertion, the fixture rewritten
to keep its session open across verbs, and a wrong-but-valid key asserted as the pinning
falsification.

Seven mutations, each with its result in the evidence — including two that are **not** red and are
reported as such: M4 (the accessor's source swapped back, behaviourally invisible while the
assertion stands) and M6, which came back green and exposed **dead code** (the `conversation_survives`
predicate was unreachable, because a refusal arrives as an answered `Message::Error` rather than an
`Err`) — deleted rather than kept as a branch that always evaluates the same way. M2 was green on
its first run and the test was **strengthened** until it was red: the first version asserted only
`Peer(_)`, and a read served under the wrong key also fails later on a dead channel.

The **real-daemon probe** in `.loop/evidence/T-0114/` reproduced the p1 on real processes and shows
the fix stopping it (two reads on one conversation, real metrics, wrong key refused); it is
re-run by the integrator on the final tree. It also found a real defect in the shipped docs — the
peer to dial is the device id of the published `daemon_key`, not the directory row's `machine_id`
— corrected in `relay.rs` and `docs/mobile.md`.

The probe is scratch (under gitignored `target/`), so the durable home for this check is filed as
**T-0125**: an `xtask e2e --slice ffi` that drives real binaries, with the pre-fix red as its
acceptance evidence.
