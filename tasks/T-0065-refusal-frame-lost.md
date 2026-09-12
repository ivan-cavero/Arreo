---
id: T-0065
title: The daemon's refusal frame is written but never reaches a remote peer
phase: 2
priority: 2
status: proposed
depends_on: [T-0064]
scope:
  - crates/arreo-core/src/relay/session.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-server/tests/relay.rs
  - .loop/evidence/T-0065/**
---

## Goal

When a daemon refuses a remote peer — at Hello (the trust gate) or on any later verb — it
writes the refusal and closes. **Over the relay that frame is lost.** The peer sees a bounded
failure (T-0064 saw to that) but never the reason, so the operator's one actionable sentence
never arrives.

Found while fixing T-0064 in T-0047's mesh slice: the refusal *is* produced, beta's own log
proves it, and the client reports "no answer to Hello within 10s (the peer accepted the
connection and then said nothing — it may be refusing this device)". Right diagnosis, missing
sentence.

## Evidence (T-0047's mesh slice, 2026-09-12)

Beta's daemon log, for a device that holds a valid account certificate, is pinned on beta, and
has had its grant cut:

```
arreo-server: relay peer dev_67a590f2ba2867cf3b87d4c77156632f authenticated
daemon: refusing Hello for dev_67a590f2ba2867cf3b87d4c77156632f on this machine: machine
  vmi3525064 revoked this device's grant, so Hello is refused. Re-grant it with:
  arreo machines trust dev_67a590f2ba2867cf3b87d4c77156632f --machine vmi3525064 --role
  viewer --yes
```

The client, on the same call:

```
panes: beta-machine: mesh client handshake: no answer to Hello within 10s (the peer accepted
  the connection and then said nothing — it may be refusing this device)
```

So: refusal written, flush attempted (`serve_session`'s wrapper does `writer.shutdown()` then
sleeps `FINAL_FRAME_GRACE`), and nothing arrives.

## The hypothesis to test first

`RelayStream`'s write direction is a `DuplexStream` plus **one forwarding task**. The bytes
the daemon writes go into the duplex; that task is what actually sends them as relay
envelopes. The task belongs to the `RelayStream`, and the `RelayStream` is dropped when
`serve_session` returns — i.e. immediately after `FINAL_FRAME_GRACE`. If the forwarding task is
aborted by that drop before it has drained the duplex, the frame dies with it. If that is the
mechanism, a refusal on a *local* socket (no RelayStream, no forwarding task) would arrive
fine — which is exactly what T-0046's tests show, and why they never caught this.

The test that settles it: write a frame, call `shutdown`, drop the stream, and assert the
frame arrived — on both transports. Local passes today; the relay half is the bug.

## Acceptance criteria

- [ ] A frame written immediately before `shutdown` reaches a relay peer. Proven by a test over
      a real relay (the pattern `crates/arreo-server/tests/relay.rs` already uses), not by
      inspection.
- [ ] The daemon's refusal at Hello arrives verbatim at a remote client, and the CLI maps it to
      the trust exit code (5) — the sentence T-0046 ships for exactly this moment.
- [ ] T-0047's mesh check "the refusal's own message reaches the operator" flips from SKIP to
      PASS, and that skip is removed rather than left as history.
- [ ] The local path keeps working (no regression): `crates/arreo-cli/tests/remote_machine.rs`
      and `crates/arreo-server/tests/trust.rs` stay green.
- [ ] Any other caller that writes-then-closes over the relay is audited for the same defect —
      the shape is general, so the fix belongs in the stream, not at one call site.

## Notes

- **Why this is separate from T-0064:** T-0064 is the *hang* (an unbounded read) and is fixed;
  this is a *lost frame*. Fixing the bound turned a mystery into a named failure, which is what
  made this visible at all — the two are ordered, not merged.
- Severity: this is the difference between "access denied, run `arreo machines trust …`" and
  "the peer said nothing". The first is actionable; the second sends the operator to look at the
  network.
- The relay's own delivery accounting is a useful check while fixing: the relay reports
  `Delivered` for a frame it hands to a session's queue, so if the frame was handed over and
  then dropped inside the client's stream, the relay's report will say `Delivered` while the
  caller saw nothing (the same class of gap ADR 0013 records for T-0060).

## Verification

```console
cargo test -p arreo-core --lib relay::session
cargo test -p arreo-server --test relay
cargo xtask e2e --slice mesh
```
