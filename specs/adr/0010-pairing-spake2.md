# ADR 0010: pairing — SPAKE2 over a public mailbox, one guess, nothing half-done

- Status: accepted (2026-09-11, T-0024)
- Context: `arreo pair` is the ritual §1/§3.3 promise — "pair in 30 seconds, no
  password to type, no SSH, no assumption that the LAN is friendly". T-0025 gave
  devices identities and certificates, but nothing could *get* a device pinned
  without copying a public key between machines by hand. The constraint that
  shapes every choice here: the only channel between the two machines may be a
  relay that is assumed hostile, and the only shared secret may be something a
  human can read off one screen and type into another.
- Decision: a **4-word code from a committed 256-word list** (32 bits) is used as
  the password of a **SPAKE2 (RFC 9382, ed25519 group)** exchange; the resulting
  key authenticates both directions with **HMAC-SHA256** confirmation MACs
  covering (label, session id, payload); the *server* is the SPAKE2 identity
  (`id_a = "arreo-server:<root pubkey hex>"`, `id_b = "arreo-session:<sid>"`); a
  successful exchange carries the phone's public key to the server, which issues
  a T-0025 certificate and sends it back over the same authenticated channel. The
  mailbox is a **four-slot, write-once, single-use, TTL-bounded bulletin board**
  (`arreo_relay::pairing`), whose frames are JSON with base64 payloads so an
  operator can *see* that it carries opaque blobs. The code never appears in the
  invite URI: the URI (what a QR would carry) holds the mailbox, the session id,
  the server key and the TTL, and nothing else.
- Why this one:
  - **The code must not be the only thing an attacker needs.** Code-as-PSK under
    plain Noise, or Noise-XX keyed by the code, both hand an eavesdropper an
    *offline* verifier: capture one transcript, guess at leisure against 2^32
    candidates. SPAKE2 gives an online-only attack, and the relay's single-use
    session plus the server's burn-on-mismatch make "online" mean "one guess per
    session". 32 bits is then unguessable in any window a human leaves open.
  - **No TLS, deliberately.** The flights are public by design — a mailbox is a
    bulletin board, and the *code* is what authenticates. A network attacker's
    only moves are to read (nothing secret travels), to publish a forged flight
    early (confirmation fails: a denial of service, never an impersonation), or
    to burn the session (same). Adding TLS here would protect nothing that
    SPAKE2 does not already protect, while adding a certificate story to a
    protocol whose entire point is not having one.
  - **The server key is pinned from the invite, and it is the SPAKE2 identity.**
    Baking it into the identity means a machine-in-the-middle that substitutes
    its own key derives a different shared secret and fails confirmation — the
    phone cannot be steered onto a different server by a hostile mailbox.
  - **Write-once slots.** Anything else lets a participant replace a flight
    after seeing it, which is the classic bulletin-board attack. With write-once,
    the first flight wins and a late forgery is ignored.
  - **Nothing half-done.** The phone generates its keypair in memory and writes
    only after the certificate verifies against the pinned server key; a failed
    pairing leaves an identity directory that is byte-identical (asserted by
    comparing the whole tree, not a hash of it). The server pins only after a
    verified MAC.
- Alternatives rejected:
  - **SRP / hand-rolled PAKE**: older, murkier provenance, and a PAKE is exactly
    where hand-written crypto dies. `spake2` is the RustCrypto implementation of
    a current RFC, on group arithmetic already in the dependency graph via
    ed25519-dalek.
  - **A password or a pairing token in a config file**: that is a long-lived
    secret on a filesystem, which is what device certificates exist to replace.
  - **Trusting the LAN** (mDNS + no authentication): §4's threat model includes
    hostile devices on the same network; the LAN gets no special treatment.
  - **Persisting the mailbox** (so a relay restart does not fail a pairing):
    correct, but it belongs to the durable-inbox work (T-0030). v0 fails that
    pairing loudly and the human re-runs one command.
  - **Burning the session as soon as the certificate is published**: shipped
    first, and the real-process test caught the consequence — the phone's next
    poll found the session gone while its certificate sat unread. Retirement
    moved to the last reader (the phone), with the TTL covering a phone that
    never arrives.
- Consequences: `arreo-core::pairing` holds the code, the mailbox wire types, the
  client and both state machines; `arreo-relay::pairing` holds the mailbox and
  its socket servers (unix and TCP), and the relay binary serves it. The mailbox
  outlives the displayed window by `MAILBOX_GRACE` so the server's deadline — the
  one a human is told about — is what fires, and the last flight has somewhere to
  land. `arreo devices` is untouched: pairing is a client of
  `DeviceAuthority::issue`, so there is still exactly one cert-minting path.
  `pairing_failed` is now an audit kind, which also made `arreo audit` print
  event kinds (without the column, a refused pairing was indistinguishable from
  a prompt). Depth 2 of the security story — transport binding for the pinned
  key — is T-0023; the mailbox's durability and admission control are T-0029/T-0030.
