# ADR 0011: the remote transport — Noise-KK inside QUIC, one device key, TLS as a transport detail

- Status: accepted (2026-09-11, T-0023)
- Context: ROADMAP §3.2 promises a session that survives a subway tunnel and
  resumes where it left off; §3.3 says the phone generates an ed25519 device key,
  the server pins it, and "every later connection is Noise-KK mutual auth"; §4
  promises remote access with zero inbound ports and device trust instead of a
  CA. T-0025 shipped the identities and T-0024 the pairing that pins them, so
  what was missing is the wire: a pinned device had nothing to connect *to*, and
  the daemon spoke only over the local Unix socket.
- Decision: **Noise-KK (`Noise_KK_25519_ChaChaPoly_BLAKE2s`, `snow`) runs inside
  a QUIC connection (`quinn`)**, one bidirectional stream per session carrying
  the existing length-delimited MessagePack frames from T-0013. QUIC's mandatory
  TLS 1.3 uses an **in-memory ephemeral certificate (`rcgen`) that the client
  does not verify**: the trust anchor is the pinned ed25519 device key, and the
  Noise static is *derived from it* (public: the birational map
  `ed25519_pk.to_montgomery()`; secret: the clamped expanded scalar) rather than
  distributed as a second key. Sessions are client-initiated; the shipped daemon
  opens no listener at all, and the loopback listener behind
  `ARREO_TRANSPORT_TEST_LISTEN` is a documented test seam. Replay is refused by a
  per-process **flight guard** in addition to per-peer rate limiting.
- Why this one:
  - **quinn over quiche.** Pure Rust, tokio-native, no BoringSSL/OpenSSL C
    toolchain — it cross-compiles for the mobile core and keeps the daemon size
    budget. Over hand-rolled reliability: congestion control, stream muxing and
    connection migration are precisely where guessing costs the §5 latency
    budgets, and migration is what makes §3.2's resume story work on a phone
    that changes networks.
  - **Noise over TLS-PKI for authentication.** §4 pins *devices*, not
    certificates issued by anyone; a CA hierarchy would be overhead plus another
    parser to fuzz, and would put the trust decision in a place that rotates
    (ephemeral certs are not identities).
  - **Noise over a patched quinn.** Replacing QUIC's TLS with Noise inside the
    stack (`quinn-hyphae`'s approach) puts an unaudited fork on the
    security-critical path. Running Noise *inside a stream* costs one extra round
    trip and keeps both layers stock. Measured: the handshake is 2 round trips
    instead of 1, and the attach budget still holds (`.loop/evidence/T-0023/`).
  - **One key, not two.** A separate transport key would be a second thing to
    pair, pin, rotate and revoke, and every one of those is a place for the two
    to disagree — a rotated identity with a stale transport key authenticates as
    somebody it is not. Deriving the Noise static from the pinned identity means
    the key a device pinned during pairing is the key it authenticates with, and
    `identity::keys` asserts the derivation against an independent computation
    (`scalar × B_montgomery`) rather than restating the implementation.
  - **The announcement is a cleartext device id.** Noise-KK needs the responder
    to know the initiator's static key before the first flight, but a server has
    many pinned devices and cannot know which one is calling. The caller
    announces its id — a public-key fingerprint — and the handshake still fails
    unless it holds the matching secret. A forged hint costs a refused connection
    and an `auth_reject` audit row, never an impersonation.
- Known trade-offs, stated rather than hidden:
  - **Noise does not authenticate QUIC's transport layer.** A machine-in-the-
    middle that terminates TLS can drop or delay traffic; it can never read or
    forge it. That is a denial-of-service surface, and it is accepted because the
    alternative (pinning the TLS certificate) duplicates the trust decision in a
    second, weaker place.
  - **The root key does both jobs.** The server's Noise static derives from the
    root signing key, so the key that signs certificates is also the key that
    authenticates the transport. The device keys sign nothing in this product
    (their Ed25519 half only identifies them, which is what makes reusing them
    for Diffie-Hellman safe), but the root key *does* sign. If device keys ever
    gain a signing role, or the root key's exposure surface grows, the move is a
    signed prekey (a separate X25519 key bound by an Ed25519 signature) rather
    than this derivation.
  - **The first handshake flight is static-key-dependent, so it is replayable in
    principle.** The flight guard refuses a repeat within a window; without it a
    fresh responder would answer a recorded flight and believe in a session with
    an absent device (no keys are derivable, so nothing is readable — but a
    session the server believes in is worth refusing). This is a property of
    KK's first message, not of this implementation.
  - **0-RTT and tickets are acceleration only.** No security decision depends on
    resumption (§3.14).
  - **The listener's denial-of-service controls are per-peer, not global.** The
    rate limiter bounds handshake attempts per address (3 per 10 s), and each
    accepted connection's handshake runs in its own bounded task so a quiet peer
    cannot wedge the accept loop. There is no global cap on concurrent
    connections; the shipped posture (zero inbound ports) is what bounds the
    surface, and a LAN-direct mode would need that cap.
- Alternatives rejected:
  - **TLS with pinned certificates, no Noise**: the pin would live in the
    certificate, which rotates; and it makes the transport's identity a
    different object from the device identity the authority checks.
  - **Plain Noise over UDP**: rebuilding QUIC's reliability, muxing and migration
    by hand.
  - **Reusing the Unix-socket framing on top of TCP + Noise**: no connection
    migration, so a phone changing networks drops the session — exactly the
    §3.2 scenario.
  - **Adding a transport-only keypair**: see "one key, not two" above.

## Consequences

- `arreo-core::transport` is the only place that knows quinn or snow exists; the
  daemon depends on the module's re-exports, so swapping either is a
  single-crate change.
- Every remote session runs the *same* `serve_session` loop as the local socket
  (`crates/arreo-server/src/daemon.rs`), with a per-verb gate
  (`DeviceAuthority::check_verb`) in front of it. One protocol implementation,
  two ways to reach it.
- New dependency surface (`quinn`, `rustls`, `rcgen`, `snow`, `ring` and their
  trees) is covered by the T-0020 supply-chain gates in the same commit that adds
  them.
