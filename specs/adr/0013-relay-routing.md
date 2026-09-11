# ADR 0013: relay routing — authenticated devices, opaque envelopes, no second codec

- Status: accepted (2026-09-11, T-0029)
- Context: ROADMAP §3.4 makes the relay the way two NAT'd peers find each other and §7 makes it a
  self-hostable AGPL binary, while §4's whole premise is that the relay is *not* trusted with
  content: "routes bytes it cannot read". T-0023 gave devices an encrypted transport to a daemon and
  T-0025/T-0024 gave them identities and a way to pin one; nothing yet let two devices in one
  account reach each other through a third party. The constraints that shape every choice below:
  the relay must be implementable as a single self-hosted binary, it must be able to authenticate a
  device it has never seen, and it must have nowhere to put the bytes it moves.
- Decision:
  - **The wire vocabulary lives in `arreo-core` (Apache-2.0); the relay (AGPL) implements the
    server.** One framing, one set of message types, and a third party can build a client without
    linking AGPL code.
  - **Transport is QUIC** (the same endpoints T-0023 built), with the relay's ephemeral certificate
    unverified by the client — TLS is a transport detail, and the trust anchor is the device
    certificate verified at the application layer.
  - **Authentication is a certificate *plus a proof of possession* over a relay-chosen nonce:**
    `Hello` → `Challenge { nonce }` → `Auth { cert, signature }` → `Welcome`. The signed payload
    binds the nonce to the account and to the **canonical** device id (the bare hex from the
    certificate, never the announced spelling), so the crypto does not depend on a cosmetic choice
    of wire form.
  - **Routing is per-envelope and stateless beyond a live map:** `[u32 LE len][MessagePack
    header][opaque payload]`, header `{v, account_id, src_device, dst, seq, kind}`, checked against
    the authenticated session, then either handed to the destination's queue or answered with a
    typed `Outcome` (`delivered` / `offline` / `no_such_device` / `refused`).
  - **Framing reuses T-0013's convention** (little-endian `u32` prefix + `rmp-serde`), not a second
    codec.
  - **`arreo-relay account add` is the operator's door** to the account registry, which holds each
    account's root public key — the anchor every device certificate in it is verified against.
- Why this one:
  - **Certificate + proof, not "present a certificate".** A certificate is a public document:
    anyone who has seen one can show it. Presenting it therefore proves nothing about who is
    calling, and a design that treats presentation as authentication is broken in a way that looks
    fine in a demo. The nonce is what makes the exchange a *session*: it is fresh per connection, so
    a recorded handshake is worthless, and the signature covers `account_id` and `device_id`, so a
    proof harvested in one account cannot be replayed into another. One function
    (`arreo_core::relay::verify_auth`) makes the whole decision, so it has one test suite and one
    reviewer.
  - **Noise-KK does not fit the relay.** T-0023's transport is a perfect fit for *daemon* sessions,
    where each side knows the other's pinned key in advance. A relay cannot: it serves many
    devices, and a Noise-KK responder must know the initiator's static key before the first flight.
    The ways around that are worse than not using Noise here — registering every device's key with
    the relay leaks the account's device list to the router and adds a second registration flow
    whose failure mode is "the device is pinned on the daemon but unknown to the relay". A
    certificate that chains to the account root the relay already holds needs no such registry.
  - **QUIC, and TLS left unverified.** The relay needs connection migration and congestion control
    for the same reasons the daemon does (a phone changes networks), and `arreo_core::transport`
    already ships both. The relay's own certificate is ephemeral and deliberately not pinned: it
    rotates, it is not an identity, and pinning it would put a second, weaker trust decision
    beside the certificate check that actually authorizes the device.
  - **Typed outcomes instead of silence.** A sender that is told `no_such_device` has a bug to fix;
    one told `offline` should retry later (and gets a queue in T-0030). Collapsing the two into one
    error would make the sender guess, and reporting nothing at all would make "was it delivered?"
    unanswerable — so every envelope gets exactly one report, on its sequence number.
  - **The relay has nowhere to put a payload.** The envelope type carries `Vec<u8>` and the decode
    path stops at the end of the header; no relay-side type can hold pane text or agent state, and
    the schema test fails if such a column appears. That is what makes the §4 promise checkable
    rather than aspirational — the integration test asserts a marker string appears nowhere in the
    state directory or the logs.
  - **One framing, reused.** `codec.rs` already pins "decode is total on garbage" and is fuzzed;
    a second codec in the same product would be a second thing to fuzz and a second place for the
    two to disagree.
- Alternatives rejected:
  - **Routing on the payload** (peeking at a pane id or a verb to decide where it goes): that makes
    "cannot read" a lie, and it puts the relay's routing logic inside a format it would then have to
    parse and version.
  - **A device registry pushed to the relay** (so the relay could pin keys and use Noise): leaks the
    account's device list to the router and adds a flow that can silently fall out of sync with the
    daemon's authority.
  - **TLS client certificates as the device identity**: makes the transport's identity a different
    object from the device identity the authority and the audit log already use (§4 pins *devices*,
    not certificates issued by anyone).
  - **A `status` as a second framing** beside the envelope: the reader would have to guess which
    shape arrived, so the status is a `kind` on the same framing — one decode, one branch.
  - **A durable queue in this task**: T-0030 owns the inbox (bounds, exactly-once, drop counting).
    Answering `offline` today is a real, tested behavior, not a stub.
  - **Managed-only relay features**: §3.14 — the self-hosted binary gets the identical feature set.
- Known trade-offs, stated rather than hidden:
  - **The relay sees the payload bytes; it does not read them.** Confidentiality is the *daemons'*
    job: a real payload is wrapped in the daemon-to-daemon Noise session (T-0023), so what the relay
    carries is ciphertext. A client that sends plaintext gives the relay plaintext — the guarantee
    is about what the relay does, not about what a careless client sends. Wiring the daemon side is
    T-0050.
  - **No revocation list yet.** The relay verifies the certificate chain and the proof of
    possession, but a revoked device's certificate still verifies; revocation propagation is T-0026.
  - **Presence is not derived.** `relay_device` records first/last seen; nothing computes
    online/offline from it yet (T-0031).
  - **Refusals are logged to stderr, not to a durable audit log** (T-0033), so an operator can see
    them live but cannot yet query them.
  - **The handshake budget is per address, not global, and it counts connections.** 3 per 10 s per
    peer, checked as the connection arrives (so a peer that completes QUIC and never sends `Hello`
    still spends budget), forgiven on a successful authentication so a reconnecting phone is not
    punished, and logged with the peer's address when it fires. A distributed flood from many
    addresses is bounded only by the per-frame cap and the connection's outbound queue (64 items,
    after which the destination counts as unreachable).
  - **The account registry is the operator's to fill.** Until the pairing flow registers accounts,
    `arreo-relay account add` is a manual step, and an account that is not registered refuses every
    device with `unknown account`.
  - **The TCP/WebSocket fallback behind a TLS terminator is documented, not shipped.** v1 is QUIC.

## Consequences

- `arreo_core::relay` is the only place that knows the relay's wire format, and it holds both the
  types and the reference client — so the daemon (T-0050), the CLI (T-0044) and any third party
  share one implementation, and the AGPL boundary is architecture rather than a promise (T-0035).
- `arreo-relay::store` remains the single relay SQLite connection and migration owner; v2 added the
  account root key and `relay_device`, which T-0030 (inbox), T-0031 (presence) and T-0033 (audit)
  extend rather than replacing.
- The relay binary gains a tokio runtime and two new doors (`serve`, `account add`) while the T-0024
  pairing CLI keeps its exact behavior — a compatibility surface with tests.

## Follow-up: one route per device, and the session it replaces (T-0060)

The rule above — *a reconnecting device keeps its newer session* — is right for a
**reconnect** and was wrong for a **concurrent** session that is about to disappear.
Both look identical to the relay: a second authenticated session for a device id it
already has.

What the implementation did: `Router::register` replaced the map entry and nothing
else. The replaced session kept its connection and its read loop (which holds its own
sender clone, so the queue never closed), was never routed to again, and was never
told. So a machine whose daemon was replaced stayed **dark**: connected, unrouted,
unaware.

That is not a corner case. A machine's daemon and its CLI share one device identity
(one identity per device), so *any* relay-touching verb run on the machine that hosts
a daemon replaces that daemon's session — and `arreo machines list` is what an
operator runs while diagnosing. Measured: the machine stayed unreachable for as long
as the test waited (20 s), with the daemon's log showing a healthy session the whole
time.

**Decision: the replaced session is told, and it ends.** `register` sends the
displaced session's queue an `Outbound::Displaced`; its writer task returns, which
drops the QUIC send stream, and the device's client learns the way it learns about
any other ending. Recovery is then the client's ordinary reconnect path —
`backoff_delay(0)` ≈ 250 ms measured, bounded by `BACKOFF_CEILING` — and frames that
arrive during the gap are **queued durably** rather than handed to the session that
is on its way out, so nothing is lost.

**Rejected: reference-counting sessions per device, keeping the previous route as a
fallback.** It looks strictly better — the daemon would never be interrupted at all —
and it fails on delivery. While a short-lived CLI session is "on top", the relay
would hand it the next frame addressed to that device; the CLI cannot serve it, and
the relay has already reported `Delivered`, so a peer's message is lost. Displacing
and promptly ending costs a quarter-second of queuing and loses nothing; the
fallback stack costs nothing visible and can lose a message. Correctness over
convenience, as in every other routing decision here.

**Honest consequence, stated rather than hidden:** two processes sharing one device
identity will always contend for the route, and a *long-running* CLI verb (an
`attach`, say) can lose its session to its own machine's daemon reconnecting. That is
the price of one route per device, and the fix for it is a client that does not share
the daemon's identity — not a routing rule that lets two sessions both think they are
the device. Recorded here because the next person will meet it.

Tested at both levels: `crates/arreo-relay/tests/router.rs` (two sessions, one device;
the first ends within 5 s) and `crates/arreo-cli/tests/remote_machine.rs` (a CLI verb
on the daemon-hosting machine, then another machine attaches successfully).
