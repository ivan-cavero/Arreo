# The Arreo relay wire protocol, version 1

> Normative for v1. Every statement below is a property of code that ships:
> the vocabulary and the reference client in `crates/arreo-core/src/relay/`
> (Apache-2.0) and the server in `crates/arreo-relay/src/router.rs` (AGPL-3.0).
> The vocabulary lives in the Apache crate on purpose (ROADMAP §7, T-0035): a
> third party can implement a client — or a server — from this document without
> linking AGPL code. Where this document and the code disagree, the code is
> right and this document is the bug.

One sentence: `[u32 little-endian length][MessagePack header][opaque payload]`
carries bytes between two devices in one account, and the relay routes them
without ever decoding the payload.

## 1. Transport

- **QUIC over UDP.** One QUIC connection per session, one bidirectional stream,
  opened by the client and accepted by the relay. The handshake and every
  envelope travel on that one stream, in order.
- **ALPN** `arreo/transport/1`; the QUIC/TLS server name is `arreo.invalid`.
  Trust does not come from either.
- **TLS 1.3 is mandatory** (QUIC requires it) and the relay's certificate is
  generated in memory for the life of the process. A client MUST NOT treat it as
  an identity, and MUST NOT fail the connection because it is self-signed: the
  trust decision is the application-layer certificate check in §3. This is
  ADR 0011's split, reused by the relay (ADR 0013).
- **QUIC only.** There is no TCP or WebSocket path for envelopes in v1. The
  pairing mailbox has its own TCP and unix listeners; that is a different door
  and it carries no envelopes.
- **Bounds at this layer.** The relay gives a peer 10 s to establish the QUIC
  connection (`CONNECT_TIMEOUT`) and the reference client bounds connect and
  handshake at 10 s each (`HANDSHAKE_TIMEOUT`). The relay's own handshake read
  is not bounded by a timer: a peer that connects and then says nothing holds
  one task until it goes away (the accept loop is unaffected — see §5.4).

## 2. Framing

Two message classes, two shapes. Both start with a little-endian `u32` length,
the framing convention this product already uses everywhere (T-0013).

### 2.1 Handshake messages

```text
[u32 LE len][MessagePack body]        len = length of the body
```

- Cap: **16 KiB (16,384 bytes)**, enforced on encode and on read. A length
  prefix over the cap is refused before any allocation, and the session ends.
- The body is exactly one MessagePack value (§2.3): `Hello`, `HelloReply`,
  `Auth` or `AuthReply`.

### 2.2 Envelopes

```text
[u32 LE len][MessagePack header][opaque payload]     len = header + payload
```

- Cap: **1 MiB (1,048,576 bytes)** for header and payload together. The length
  prefix is checked against the cap before anything is allocated.
- The header's own end is not written down anywhere: it is wherever the
  MessagePack decoder stops. Everything after that point in the frame is the
  payload, carried byte for byte and never interpreted by the relay.
- A payload may be empty (`len` = header length) and may be arbitrary bytes —
  it is not required to be MessagePack. Only `status` payloads are MessagePack,
  and only the relay writes those (§4.3).
- Do not read an envelope with the handshake frame reader: the envelope carries
  its own length prefix, and stripping it would make the decoder read the header
  bytes as a length. Use one reader per class.

### 2.3 MessagePack encoding rules

Every value this protocol defines is written by `rmp_serde::to_vec`, which is
the compact form:

| Rust shape | On the wire |
| --- | --- |
| struct | an array of its fields, in declaration order |
| unit enum variant | its name as a string (`"frame"`, `"delivered"`) |
| struct variant | a one-entry map: `{ "VariantName": [fields…] }` |
| `Vec<u8>` field | an array of unsigned integers (not `bin`) |

So a `Hello` is a 3-element array, and `HelloReply::Challenge` is
`{"Challenge": [v, nonce]}`. The enum names are exactly the ones in this
document: `frame`, `status` for the envelope kind, and `delivered`, `offline`,
`no_such_device`, `refused` for an outcome.

Interop notes, both a property of the decoder (`rmp_serde::from_slice` /
`from_read`, compact config):

- a struct may also arrive as a field-name map; the canonical positional array
  is what this implementation writes, and emitting it is the safe choice;
- a byte string may also arrive as MessagePack `bin`; the canonical integer
  array is what this implementation writes;
- integers may use any MessagePack integer marker, not only the smallest.

### 2.4 Two errors worth naming

- **Incomplete** — the buffer does not yet hold a whole frame. This is the only
  condition a stream reader may retry on: everything else is a peer that is
  wrong, not slow.
- **A zero-length envelope** (`len = 0`) is a *header decode failure*, not
  `Incomplete`: there is a complete frame, and its header is missing. Decoding
  is total on garbage — it never panics and never yields a partial envelope.

## 3. The handshake

### 3.1 Sequence

```text
device                                          relay
  Hello { v, account_id, device_id }  ------->
                                      <-------  Challenge { v, nonce }
                                            |  Refused   { v, reason }
  Auth { v, cert, signature }         ------->
                                      <-------  Welcome   { v, account_id, device_id }
                                            |  Refused   { v, reason }
  [u32 len][header][payload]          <------>  route; payload never decoded
```

Rules:

1. The client opens the stream and writes `Hello`. Nothing else is sent until
   the reply is read.
2. The relay either refuses (see §5.1) or answers `Challenge` with a **fresh
   nonce of 32 bytes of OS entropy**, minted per connection.
3. The client signs the nonce (§3.6) and writes `Auth` with its certificate.
4. The relay verifies (§3.8) and answers `Welcome` or `Refused`.
5. From then on the stream carries envelopes in both directions, interleaved.

A refusal is written, then the stream is finished and the connection is held
open for up to 2 s so the peer can read it. A client that drops the connection
the instant it sees a closed stream reports "connection lost" instead of the
reason the relay actually gave — read the frame first.

### 3.2 `Hello` — device → relay

| Field | Type | Meaning |
| --- | --- | --- |
| `v` | u32 | protocol version; must be `1` |
| `account_id` | string | the account the device claims; opaque to the protocol |
| `device_id` | string | the device's own id: 32 lowercase hex characters, `dev_` prefix optional. This is the id of the certificate presented in `Auth`; the proof is signed over the certificate's *canonical* id, not this spelling (§3.6) |

`v` other than 1 is refused with `HelloReply::Refused` and the reason
`protocol version <n> is not supported`. An unknown `account_id` is refused
before any cryptography runs: `unknown account <id>`.

An account exists at the relay only if an operator registered it there — today
with `arreo-relay account add`, because the pairing flow does not register
accounts yet. An account with no registered root key has nothing for a device
certificate to chain to, so it refuses every device; a client that gets
`unknown account` should treat it as an operator problem, not a bug in its
handshake.

### 3.3 `HelloReply` — relay → device

A tagged enum; exactly one of:

| Variant | Fields | Meaning |
| --- | --- | --- |
| `Challenge` | `v: u32`, `nonce: Vec<u8>` | prove you hold your key by signing this nonce; the nonce is 32 bytes |
| `Refused` | `v: u32`, `reason: string` | the session is refused before any proof was asked for |

### 3.4 `Auth` — device → relay

| Field | Type | Meaning |
| --- | --- | --- |
| `v` | u32 | protocol version; must be `1`, checked before anything else |
| `cert` | bytes | the device certificate (§3.7), MessagePack-encoded |
| `signature` | bytes | 64-byte ed25519 signature over the proof payload (§3.6) |

### 3.5 `AuthReply` — relay → device

| Variant | Fields | Meaning |
| --- | --- | --- |
| `Welcome` | `v: u32`, `account_id: string`, `device_id: string` | the session is up; `device_id` is the id the certificate names, in `dev_<hex>` form |
| `Refused` | `v: u32`, `reason: string` | the session is refused; `reason` is written to be shown to a human |

A client must check `v` on `Challenge` and `Welcome` and fail the session if it
is not 1.

### 3.6 The proof payload

The bytes to sign are, exactly:

```text
"arreo-relay-auth-v1" 0x00 <nonce> 0x00 <account_id> 0x00 <device_id>
```

i.e. the fixed 20-byte label (`arreo-relay-auth-v1` plus a terminating NUL),
the 32 nonce bytes, a NUL, the account id bytes, a NUL, the device id bytes —
with no length prefix and no trailing separator. The signature is a plain
ed25519 signature (64 bytes) by the private key whose public half is in the
certificate.

Two details a client cannot get wrong and still interoperate:

- **`device_id` is the canonical id: the bare 32 lowercase hex characters**,
  whatever spelling you announced in `Hello`. The relay builds the payload from
  the id *inside the certificate*, so `dev_<hex>` and bare `<hex>` are the same
  device and the signature is the same bytes either way. Signing the announced
  spelling instead is refused: the crypto deliberately does not depend on a
  cosmetic choice of wire form.
- The fixed label means this signature can never be confused with another
  signature this product makes (a certificate payload, an audit entry).

The binding of `account_id` and `device_id` into the signed bytes is what makes
a proof harvested in one handshake useless in another; the fresh nonce is what
makes a whole recorded handshake useless.

### 3.7 The certificate

`Auth.cert` is opaque to the relay protocol, but the relay will not accept an
arbitrary blob: it must decode as a `DeviceCert` (T-0025), MessagePack-encoded
with field names (`to_vec_named`):

```text
DeviceCert { payload: CertPayload, signature: [u8; 64] }
CertPayload {
  version: u8,          // must be 1
  device: DeviceId,     // 32 hex chars, = fingerprint of public_key
  public_key: [u8; 32], // the device's ed25519 public key
  name: string,         // human label, display only, never trusted
  role: "owner" | "viewer",
  issued_at_ms: i64,
  serial: u64,          // must be >= 1
}
```

`signature` is the account root's ed25519 signature over the MessagePack
encoding of `payload` (field names, same encoder). A device id is
`sha256(public_key)` truncated to 128 bits and hex-encoded; the certificate must
carry the fingerprint of its own key, and the key it carries must be the key
that signed the proof.

### 3.8 The order the relay checks, so you know which refusal you get

`verify_auth` decides the whole trust question in one function, in this order:

1. `Auth.v` is 1 — otherwise a version refusal.
2. `cert` decodes as a certificate — otherwise "undecodable certificate".
3. the certificate carries a usable ed25519 public key.
4. that certificate verifies under the **account root** registered for
   `Hello.account_id`, for the key it carries (version, serial, fingerprint,
   root signature).
5. the announced `device_id` parses, and equals the device the certificate
   names — compared as parsed ids, never as strings.
6. `signature` is exactly 64 bytes, and it verifies over the proof payload
   (§3.6) with the certificate's key.

Everything that fails is answered with `AuthReply::Refused`; steps 4–6 all mean
"not who you claim to be", but the reason string says which check failed.

## 4. Envelopes

### 4.1 `RelayHeader`

The only part of an envelope the relay decodes. There is deliberately nowhere
in it to put pane text, agent state or a key.

| Field | Type | Meaning |
| --- | --- | --- |
| `v` | u32 | protocol version; must be `1`. A different value ends the session (it is not a per-envelope refusal) |
| `account_id` | string | the account this envelope belongs to. The relay requires it to equal the authenticated session's account, so a device cannot reach into another account |
| `src_device` | string | the sender. The relay requires it to equal the session's own device id (parsed and compared, so `dev_<hex>` and `<hex>` are the same device) |
| `dst` | string | the destination device id, in either spelling |
| `seq` | u64 | the sender's own sequence number. The relay does not check or rewrite it; it echoes it on the status it answers with |
| `kind` | `"frame"` or `"status"` | what the envelope is for (§4.2) |

The relay does **not** enforce monotonic `seq` and does not de-duplicate: a
receiver that cares about ordering or replays must do that itself, on the
sender's sequence numbers.

### 4.2 `kind`

| Value | Direction | Payload |
| --- | --- | --- |
| `frame` | device → relay → device | opaque bytes; the relay never decodes or persists them |
| `status` | relay → device only | MessagePack of one `Outcome` (§4.3), written by the relay |

A device that sends `kind = "status"` is refused per-envelope: statuses are the
relay's to originate.

### 4.3 Status envelopes and `Outcome`

Every frame the relay reads earns exactly one status envelope back to its
sender, on the same stream. Its header is: `v = 1`, `account_id` = the session's
account, `src_device = "relay"` (a reserved word — device ids are 32 hex
characters, so it can never collide with one), `dst` = the sender's own device
id in `dev_<hex>` form, `seq` = the sequence number of the frame being reported,
`kind = "status"`. Its payload decodes to:

| Outcome | Name on the wire | Meaning |
| --- | --- | --- |
| `Delivered` | `delivered` | handed to the destination's outbound queue. Not a read receipt: the destination's application may not have looked at it yet |
| `Offline` | `offline` | the destination is a known device that is not connected, or is connected but has stopped reading (its queue is full). Retry later |
| `NoSuchDevice` | `no_such_device` | no device with that id has ever authenticated in this account. A wrong address, not a transient state |
| `Refused` | `refused` | the relay would not carry this envelope at all; the reason is the string described in §5.2 |

A sender that never reads its own stream loses statuses: when the sender's
outbound queue is full, the relay drops the status, logs
`<device> is not reading its delivery reports; dropping one`, and moves on.

## 5. Refusals

### 5.1 Handshake refusals

| Stage | What is refused | Wire answer | Reason string |
| --- | --- | --- | --- |
| Hello | `Hello.v` is not 1 | `HelloReply::Refused` | `protocol version <n> is not supported` |
| Hello | the account is not registered | `HelloReply::Refused` | `unknown account <id>` |
| Hello | the account's stored root key is not a usable ed25519 key | `HelloReply::Refused` | `account root key is unusable: <detail>` |
| Auth | `Auth.v` is not 1 | `AuthReply::Refused` | `relay protocol version <n> is not supported (this peer speaks 1)` |
| Auth | `cert` does not decode | `AuthReply::Refused` | `the peer's certificate does not authorize it: undecodable certificate: <detail>` |
| Auth | the certificate carries no usable key | `AuthReply::Refused` | `the peer's certificate does not authorize it: certificate carries no usable key` |
| Auth | the certificate does not verify under the account root (bad root signature, wrong version, serial 0, key/fingerprint mismatch) | `AuthReply::Refused` | `the peer's certificate does not authorize it: <certificate detail>` |
| Auth | the announced `device_id` is malformed | `AuthReply::Refused` | `the peer's certificate does not authorize it: malformed device id: <detail>` |
| Auth | the certificate names a different device than the one announced | `AuthReply::Refused` | `the peer's certificate does not authorize it: certificate names <a>, not the announced <b>` |
| Auth | the signature is not 64 bytes, or it does not verify over the proof payload | `AuthReply::Refused` | `the peer did not prove it holds its key` |

The unknown-account refusal and every failed-`Auth` refusal are also logged to
the relay's stderr with the peer address and the account. The version and
root-key rows above are not logged. (Durable audit rows are T-0033; today stderr
is the whole record.)

### 5.2 Per-envelope refusals

These arrive as `Outcome::Refused { reason }` on the status envelope for the
offending frame — reported rather than dropped silently, so a misbehaving client
learns why. Each is also logged to stderr as
`refused envelope from <device>: <reason>`.

| What the envelope does | Reason string |
| --- | --- |
| `account_id` is not the session's account | `session is for account <a>, not <b>` |
| `src_device` parses but is not the session's device | `session is <a>, not <b>` |
| `src_device` is not a well-formed device id | `malformed src_device: <detail>` |
| `kind` is `status` | `a device may not send status envelopes` |
| `dst` is not a well-formed device id | `malformed dst: <detail>` |
| the device registry lookup itself fails | `device registry lookup failed: <detail>` |

### 5.3 Not answered at all

Two cases end the session instead of earning a status, because they are read
failures rather than routing decisions:

- a handshake frame over the 16 KiB cap, or a body that is not the expected
  MessagePack value: the connection is dropped, and the relay logs
  `session ended: <error>`;
- an envelope whose header cannot be decoded or whose `v` is not 1: the read
  loop ends, the connection is dropped, and the relay logs the protocol error.
  The sender sees the stream close, not a refusal.

### 5.4 The per-address handshake budget

The relay counts incoming connections **per source IP address**: at most **3 in
any 10-second window**. The 4th is refused at accept time — the connection is
dropped before any QUIC or application work — and nothing is logged for it. A
**successful authentication clears that address's history**, so a phone that
reconnects after a real network drop is not punished for the retries that got it
there. Refusals deliberately do not forgive: a peer that keeps failing is
exactly who the budget is for.

The budget is per process and per address, not global, and it resets on restart.
It is checked when the connection arrives, so it counts **connections**, not
completed handshakes: a peer that completes QUIC and never sends `Hello` still
spends its budget, and a refused connection is logged with the peer's address.
Each session's handshake runs in its own task, so a peer that connects and then
goes quiet occupies one task but does not stop the next device from connecting.

## 6. Caps and limits

| Limit | Value | Where it bites |
| --- | --- | --- |
| protocol version | `1` | any other version is refused loudly (§5.1, §4.1) |
| envelope | 1 MiB (1,048,576 bytes), header + payload | a sender cannot encode more; a receiver refuses a length prefix over it before allocating |
| handshake message | 16 KiB (16,384 bytes) | encode refuses it; a reader refuses the prefix |
| outbound queue per connection | 64 items | a destination that falls behind counts as `Offline`; a sender that stops reading loses its statuses |
| handshake budget | 3 connections / 10 s / source IP | the 4th connection is dropped at accept |
| QUIC connection timeout | 10 s | the relay drops a peer that does not complete the QUIC handshake |
| refusal grace | 2 s | how long the relay keeps a refused connection open so the peer can read the reason |

There is no global cap on concurrent connections and no cap on how long a peer
may stay silent mid-handshake; a flood from many addresses is bounded only by
these per-connection limits.

## 7. Versioning

`v1` is this document. Every message and every envelope header carries `v`, and
the relay's rule is uniform: **an unsupported version is refused loudly, never
guessed at.**

- `Hello.v`, `Auth.v`, `RelayHeader.v` must be `1`.
- A wrong `Hello.v` or `Auth.v` gets a typed refusal with the version named in
  the reason; a wrong `RelayHeader.v` ends the session.
- New message types and new `kind` values (a presence heartbeat is T-0031, an
  inbox acknowledgement is T-0030) are additive changes governed by this field,
  not silent reinterpretations of v1.

## 8. Security properties, and their limits

### 8.1 What the nonce and the signed proof buy

- **No certificate-only impersonation.** A certificate is a public document:
  anyone who has seen one can send its bytes. The relay therefore asks for a
  signature over a nonce it just minted, which only the holder of the private
  key can produce. A valid certificate with no proof, or with a signature over
  another nonce, is refused as "did not prove it holds its key".
- **No replay.** The nonce is fresh per connection, so a recorded handshake is
  worthless on the next one.
- **No cross-account replay.** The signed bytes cover `account_id` and
  `device_id`, so a proof harvested in one account's handshake cannot be
  presented in another's, and a device cannot speak as another device — the
  certificate must name the announced device (compared as parsed ids), and the
  envelope header's
  `src_device` must match the session again, per envelope.
- **No cross-account routing.** The header's `account_id` must equal the
  session's account; a device cannot reach a destination in an account it is not
  part of, and an unknown destination is a typed answer rather than a guess.

### 8.2 What the relay can see

| Can see | Cannot see |
| --- | --- |
| the header: account, sender, destination, sequence, kind | the payload's meaning — it is bytes, and the decode path stops at the end of the header |
| the device certificates and the proof of possession | the payload's *meaning*: it holds the bytes, so a payload sent in the clear is readable, and an end-to-end encrypted one is not (§8.3) |
| when a device authenticated (first/last seen) | pane text, agent state or keys: no relay-side type can hold them, and the schema test fails if such a column appears |

### 8.3 Limits, stated plainly

- **The relay does not encrypt payloads.** It carries bytes it cannot read; it
  does not make them unreadable. End-to-end confidentiality is the daemons'
  Noise session (T-0023), and wiring the daemon side to the relay is T-0050. **A
  client that sends plaintext gives the relay plaintext.**
- **No revocation list (T-0026).** The relay verifies the certificate chain and
  the proof of possession; it has no revocation list, so a revoked device's
  certificate still verifies here.
- **No durable inbox (T-0030).** An offline destination is answered `offline`;
  the envelope is not queued or stored, and the sender must decide what to do.
- **No presence (T-0031).** `relay_device` records first/last seen, but nothing
  derives online/offline from it yet, so `offline` means "no live session right
  now", not "last seen 2 days ago".
- **No durable audit rows (T-0033).** Refusals are written to stderr.
- **The transport's TLS is unauthenticated.** A middlebox that terminates TLS
  can drop or delay traffic — a denial-of-service surface — but can never read
  or forge an envelope, because the trust decision happens above it.
- **The handshake budget is per address, not global** (§5.4).

### 8.4 What a third-party client must do

1. Speak QUIC with ALPN `arreo/transport/1`, without verifying the relay's
   self-signed certificate as an identity.
2. Send `Hello` with `v = 1` and its own device id, then read one frame.
3. On `Challenge`, sign the proof payload of §3.6 over the **canonical**
   `device_id` (bare hex, §3.6), and send `Auth` with a certificate that chains
   to the account root.
4. Treat `Refused` as final and show the reason; check `v` on `Challenge` and
   `Welcome`.
5. Address every envelope with its own `account_id`, `src_device`, `dst`,
   a `seq` it can match against the status it gets back, and `kind = "frame"`.
6. Read statuses: they are the only statement about delivery. `delivered` means
   queued, not read; `offline` means retry; `no_such_device` means the address
   is wrong; `refused` means the relay rejected the envelope itself.
