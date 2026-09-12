---
id: T-0065
title: The daemon's refusal frame is written but never reaches a remote peer
phase: 2
priority: 2
status: done
depends_on: [T-0064]
scope:
  - crates/arreo-core/src/relay/session.rs
  - crates/arreo-server/src/daemon.rs
  - crates/arreo-server/tests/relay_client.rs
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

## The hypothesis, confirmed

`RelayStream`'s write direction is a `DuplexStream` plus **one forwarding task**. The bytes
the daemon writes go into the duplex; that task is what actually sends them as relay
envelopes. The task belongs to the `RelayStream`, and the `RelayStream` is dropped when
`serve_session` returns — i.e. immediately after `FINAL_FRAME_GRACE`. If the forwarding task is
aborted by that drop before it has drained the duplex, the frame dies with it. **Confirmed** — and the
local path behaves as predicted: it has no `RelayStream` and no forwarding task, so the bytes
arrive, which is exactly what T-0046's (all local) tests show and why they never caught this.

The test that settles it: write a frame, call `shutdown`, drop the stream, and assert the
frame arrived — on both transports. Local passes today; the relay half is the bug.

## Acceptance criteria

- [x] A frame written immediately before `shutdown` reaches a relay peer:
      `a_frame_written_immediately_before_closing_reaches_the_peer` in
      `crates/arreo-server/tests/relay_client.rs` (the fence named `relay.rs`, which does not exist —
      corrected here), over a real relay binary. Proved load-bearing by mutation: restoring the
      `abort` fails it.
- [x] The daemon's refusal at Hello arrives verbatim at a remote client: T-0047's mesh slice now
      PASSES "an untrusted device is refused with the actionable message" — the message carries
      `arreo machines trust … --machine … --role … --yes`, and the exit code is the trust
      vocabulary's.
- [x] T-0047's mesh checks flip to PASS: the split (bounded failure / missing sentence) is
      **collapsed back into one assertion**, because both facts are true again — the device is
      refused, the exit code is 5, and the message names the fix. The slice is also faster
      (11.9 s vs 29 s) because nothing waits on a timeout any more.
- [x] The local path keeps working: `crates/arreo-server/tests/trust.rs` (the local trust refusals)
      and the whole 474-test workspace suite are green.
- [x] Every caller audited, and the fix is at the right layer: the only write-then-close path is
      `serve_session`'s wrapper (`crates/arreo-server/src/daemon.rs:957`), and both relay consumers
      (`serve_peer`, `probe_peer`) wrap the `RelayStream` in a `SecureChannel` that reaches
      `RelayStream::drop`. Fixing the drop fixes all of them; no call site needed changing.

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

## Outcome

Fixed and pushed. The forwarding task is no longer aborted: `RelayStream::drop` closes the write
half and lets it drain, so a frame written immediately before closing reaches the peer. The task
still ends on its own — the duplex reports end-of-stream after the buffer, and a dead session
fails its `send` — so nothing leaks and no handle needs keeping.

**The mechanism, confirmed:** `Drop for RelayStream` called `forward.abort()`, killing the one task
that hands bytes to the session. Anything still in the duplex died with it. `serve_session`'s
`FINAL_FRAME_GRACE` pause made room for the drain; the abort is what made the room useless.

**Why only the relay path showed it:** locally there is no forwarding task and no duplex — the
bytes go straight into a socket the same process owns. T-0046's tests are all local, which is
exactly why the refusal they assert never arrived over the wire.

The refactor paid for itself too: `tokio::task::JoinHandle` left the file, and the stream is
simpler than before (`sink: Option<DuplexStream>` and no handle).
