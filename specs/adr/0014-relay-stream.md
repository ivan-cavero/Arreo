# ADR 0014: the relay stream — adapting a message transport to a byte stream

- Status: accepted (2026-09-11, T-0050)
- Context: T-0023 gave the product one encrypted transport: Noise-KK inside QUIC, exposed as an
  `AsyncRead + AsyncWrite` byte stream so everything above it (framing, `Hello`/`Welcome`, every
  verb) is the same code on a remote connection as on a local socket. T-0029 gave the relay a
  *message* transport: discrete envelopes with a header, a destination, and an opaque payload. The
  daemon now has to carry pane traffic between two machines through the relay, which means the two
  shapes have to meet.
- Decision: **adapt the transport, not the layers above it.** `arreo-server::relay_client` turns
  relay envelopes into a byte stream: writes are chunked into envelopes (`MAX_CHUNK`, 32 KiB),
  incoming envelopes are reassembled in order, and the result implements `AsyncRead + AsyncWrite`,
  so T-0023's `SecureChannel` runs over it unchanged. Each peer gets one stream; a chunk whose
  delivery the relay reports as anything but delivered/queued ends that stream with an error.
- Why this one:
  - **One crypto path.** A Noise implementation that speaks messages would be a second crypto path
    to audit, and T-0023 rejected exactly that when it chose to expose a byte stream rather than a
    datagram API. Reusing `SecureChannel` means the relay's traffic is protected by the code that
    already has tests, an ADR, and a reviewer's attention.
  - **Nothing above the transport changes.** The daemon protocol, its framing and its verbs were
    built against a byte stream (ADR 0007). Re-framing them over envelopes would change every layer
    above the transport — the thing T-0023 bought by choosing a stream shape in the first place.
  - **A chunking boundary is a real boundary.** One envelope holds at most `MAX_ENVELOPE_BYTES`
    (1 MiB); a write of any size has to be split somewhere. Putting the split at the transport means
    it is invisible above it, which is what "the stream is a stream" has to mean.
  - **Delivery failure has to be loud.** The relay reports per-envelope outcomes, and a chunk the
    relay could not deliver is a *gap* in a byte stream — which the Noise layer would later see as a
    decryption failure with no explanation. So sequence numbers are tracked and a non-delivery ends
    the affected stream with a reason, rather than letting a gap travel upward.
- Alternatives rejected:
  - **A message-oriented Noise layer** (a second implementation over `snow` with datagram framing):
    a second crypto path, and the reason T-0023's ADR exists.
  - **Re-framing the daemon protocol over envelopes**: changes every layer above the transport, and
    makes the local and remote paths different code again — the opposite of what T-0023 achieved.
  - **One relay connection per peer**: the relay's live map is keyed by `(account, device)`, so a
    second connection from one device replaces the first; peers must be multiplexed over one
    connection.
  - **Buffering without bound when a peer is slow**: the relay's per-connection queue already bounds
    what it will hold (T-0029), and the session drops a chunk it cannot hand on rather than growing
    — a slow reader is a broken reader, and the honest response is to fail that stream.
- Known trade-offs, stated rather than hidden:
  - **The session multiplexes peers over one relay connection**, so a peer's stream shares the
    connection's fate: losing the relay loses every peer stream at once. That is correct for
    reconnection (a new handshake per peer follows), but it means one slow peer's backpressure is
    felt on the shared outbound queue rather than isolated per peer.
  - **The session's own backpressure bound is defensive, not the usual path.** Each peer's inbound
    channel is bounded, and overflowing it ends that stream with a reason rather than leaving a gap.
    In practice the relay's per-connection queue is the same size and QUIC flow control means it
    cannot deliver faster than the local consumer drains, so through the relay this branch is close
    to unreachable — an honest "could not break it" finding, not a tested path. It stays because the
    alternative to a bound is unbounded memory, and the alternative to failing loudly is a silent
    gap in a byte stream that the layer above cannot detect.
  - **The backoff ceiling applies to the base, and jitter is added on top.** The delay is
    `min(base · 2^attempt, 30 s)` plus up to 25% of it, so the largest printed delay is about
    37.5 s rather than exactly 30 s. Stated because "capped at 30 s" would be wrong in the one
    number an operator might time.
  - **The idle timeout is 15 s** (down from QUIC's 30 s default) so a relay that vanished is noticed
    in seconds: keep-alives hold a live connection open, and the idle timer is what catches the
    dead one. A shorter timeout trades a little robustness on a badly lossy link for a bounded
    reconnect, and it is one constant to revisit.
  - **The read direction is polled, the write direction has one task.** An earlier shape gave both
    directions a task over one duplex, which let a caller that stopped reading wedge the read task
    inside a `write_all` — a stream that had already recorded its failure but could never report it.
    Polling the read channel directly removes that; the write direction keeps a duplex because a
    `poll_write` needs somewhere to put bytes that neither blocks the caller nor loses a waker.
  - **The stream is not a session.** A relay reconnect produces a *new* byte stream, so the Noise
    session above it must be re-established; stale queued chunks from a previous generation fail the
    new handshake loudly rather than silently corrupting it. That is the intended shape, and it is
    why the reconnect policy is a caller's loop rather than something the stream pretends to do.

## Consequences

- The relay path is `RelaySession` (lifecycle, peers, delivery attribution) + `RelayStream` (the
  byte stream), both in `arreo-server`; the protocol halves they use (`RelayClient::into_split`,
  `RelayWriter`, `RelayReader`) live in Apache-licensed `arreo-core::relay`, so a third-party client
  can build the same thing without touching AGPL code (T-0035).
- The daemon's use of it — configuration, boot, pane traffic, and the two-real-daemon proof — is
  T-0051; this ADR covers the transport decision only.
