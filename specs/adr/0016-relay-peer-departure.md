# ADR 0016 — A device's departure is announced to its peers

Status: accepted (T-0054)
Context: ROADMAP §3.7 (remote attach), §3.14 (offline machines), ADR 0013 (relay routing),
ADR 0014 (relay stream), ADR 0015 (remote attach)

## The problem

The relay routes envelopes between devices and keeps one stream per peer. It never told a device
that another device had *left*, so a peer that vanished was noticed only when one of its reads or
writes failed. Two consequences, both real and both measured while building T-0032:

- The far end kept a **dead stream** for the departed peer, and delivered the peer's next handshake
  into it, where the Noise layer read it as garbage and ended the stream. The *following* attempt was
  the one that worked.
- So a reconnect took as long as it took the far end to give up — measured at **over 60 seconds** —
  and no amount of client-side retrying removes the wait, because the wait is the far end's.

## The decision

**The relay announces departures, account-wide, as a new envelope kind.**

- New `RelayKind::PeerGone` (wire name `peergone`): relay → device, no payload. The header carries
  everything — `src_device` is the device that left, `dst` is the recipient, and `RELAY_SENDER` is
  the sender, so a receiver can never mistake it for a device's frame.
- **Account-wide, not addressed.** On deregister the relay sends the notice to every other live
  device in the account. It does *not* track who holds a stream to whom: a subscription table is
  state that can be wrong, and a missed notice is a stuck stream. The cost is one small envelope per
  live device per disconnect, bounded by the account's size; the receiver decides whether it cared.
- **A receiver with no stream for that peer ignores it.** The notice is news, not an instruction.
- **A device may not originate it.** The relay's read loop refuses `Status` *and* `PeerGone` from a
  device: a forged departure would let any device in an account make another device's peers drop
  their streams. The match on `RelayKind` is exhaustive, so a future kind forces this decision
  rather than defaulting to "allowed".
- **Best-effort, like every other relay message.** A device that is itself offline during the
  departure learns nothing from it and learns from the relay's outcome when it next writes. The
  notice removes a *wait*, and does not become a delivery guarantee.

**A dropped session now closes.** `RelaySession` aborts its two pump tasks on drop. Without that,
dropping a session left the pumps — which own the QUIC streams — alive as detached tasks, so the
relay never saw the device leave at all: the notice above would have had nothing to announce. This is
the same defect class as `SecureChannel`'s (a dropped handle must stop its task, not merely forget
it), and it is why the fix is two-part.

## Consequences

- `arreo-core::relay::session` ends a peer's stream with the reason `the peer went offline` when the
  notice arrives, so the layer above sees a typed end rather than a stall or a decryption failure.
- `v1` is **extended, not renumbered**. An older device cannot decode the new kind and would end its
  session on receiving one. That is acceptable before the first release and it is the reason T-0028
  (the protocol N−1 window) exists; a deployed fleet would need the version bump instead.
- The daemon's reconnect path is unchanged in shape (ADR 0015): the notice only makes the far end
  *able* to accept the next connection promptly. Measured on this box after the change: a reconnect
  from a killed client to a fresh session, transcript intact, in **77 ms**.
- The inbox path is untouched: a message to a device that is offline still queues durably (T-0030),
  and a write into a stream whose peer has left is still queued rather than refused. The notice is
  about the *reader* learning promptly, not about refusing writes.

## What this rules out

- **Inferring departures from failing reads.** It works, it is what shipped before this, and it costs
  the reconnect the far end's failure detection time — over a minute in practice.
- **A per-peer subscription table in the relay.** More state, more ways to be wrong, and the failure
  mode (a missed notice) is exactly the bug being fixed.
- **Letting a device send the notice.** It is the relay's own news; a device that could forge it
  could disconnect other people's sessions.
