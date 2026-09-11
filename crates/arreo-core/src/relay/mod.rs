//! The relay protocol (T-0029): the wire a device and the relay speak, in
//! Apache-licensed code.
//!
//! **Why this lives in `arreo-core` and not in the relay.** ROADMAP §7 draws a
//! license boundary: the relay is AGPL-3.0, the core/daemon/CLI are Apache-2.0,
//! and nothing Apache may link the AGPL crate. But both sides have to agree on
//! the bytes. So the *vocabulary* — the header, the framing, the handshake
//! messages — lives here, where a third party can implement a client or a
//! server from it without touching AGPL code (T-0035 checks the boundary is
//! architecture and not a promise). The relay crate implements the server; this
//! module implements the client.
//!
//! One sentence: `[u32 len][MessagePack header][opaque payload]` carries bytes
//! between two devices in one account, and the relay routes them without ever
//! decoding the payload.
//!
//! Framing reuses the T-0013 convention (a little-endian `u32` length prefix and
//! `rmp-serde`), deliberately: a second codec in the product would be a second
//! thing to fuzz, and `codec.rs` already pins "decode is total on garbage".
//!
//! ## The handshake, and why it is not "present a certificate"
//!
//! A certificate is a *public* document: anyone who has seen one can show it.
//! Presenting it therefore proves nothing about who is calling. So the relay
//! contributes a fresh nonce and the device signs it:
//!
//! ```text
//!   device                                  relay
//!   ------------------------------------    ----------------------------------
//!   Hello { v, account_id, device_id } ---> look the account up (unknown: refuse)
//!                                     <--- Challenge { nonce }
//!   Auth { cert, signature }           ---> cert verifies under the account root
//!                                            AND names the announced device
//!                                            AND the key in the cert signed
//!                                            (nonce, account_id, device_id)
//!                                     <--- Welcome / Refused { reason }
//!   [u32 len][header][payload]         <--> route, payload never decoded
//! ```
//!
//! Binding the signature to `account_id` and `device_id` matters: without it a
//! signature harvested in one account's handshake could be replayed into
//! another's, and the nonce is what stops a whole handshake being replayed.

/// The reference client, which needs the QUIC transport. Gated so the
/// protocol vocabulary above stays available to a build without it — and so
/// `check-targets` keeps a genuinely pure-Rust surface to type-check for
/// foreign targets (the same reason the transport itself is a feature).
#[cfg(feature = "transport")]
pub mod client;

#[cfg(feature = "transport")]
pub use client::{Incoming, RelayClient};

use crate::identity::{DeviceCert, DeviceId, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The relay protocol version. `v1` is this document; a peer that sends another
/// version is refused loudly rather than guessed at.
pub const RELAY_VERSION: u32 = 1;

/// Largest envelope the relay will carry, header and payload together.
///
/// A cap is not a policy — it is what keeps one sender from making the relay
/// hold an unbounded buffer. T-0030 bounds the *queued* case; this bounds the
/// in-flight one.
pub const MAX_ENVELOPE_BYTES: usize = 1 << 20;

/// Largest handshake message (a certificate is a few hundred bytes; this leaves
/// room for the name and signature without accepting an unbounded blob).
pub const MAX_HANDSHAKE_BYTES: usize = 16 * 1024;

/// What an envelope is for.
///
/// The discriminant earns its place immediately: a `Status` is the relay's
/// report on one envelope the sender sent, and carrying it as a *kind* rather
/// than as a second framing is what lets the sender read one stream and branch
/// on one field — no trial decode, no guessing which message arrived. A later
/// kind (presence heartbeat in T-0031, an inbox acknowledgement in T-0030) is an
/// additive change the version field governs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RelayKind {
    /// An opaque payload for another device in the same account. Its payload is
    /// never decoded by the relay.
    Frame,
    /// Relay → sender: what became of one `Frame`. The payload is the
    /// MessagePack encoding of [`Outcome`], written by the relay itself.
    Status,
    /// Device → relay: hand me what is queued for me from `from_seq` on. The
    /// payload is the MessagePack encoding of [`DrainRequest`]; the relay
    /// answers with one [`RelayKind::Frame`] envelope per message plus a
    /// [`RelayKind::Status`] carrying the per-drain counts (T-0030).
    Drain,
    /// Device → relay: I have the messages up to `seq`. The payload is the
    /// MessagePack encoding of [`Ack`]; acking is what advances the cursor and
    /// removes the rows.
    Ack,
}

impl RelayKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Frame => "frame",
            Self::Status => "status",
            Self::Drain => "drain",
            Self::Ack => "ack",
        }
    }
}

/// The `src_device` the relay uses on the messages it originates.
///
/// A reserved word rather than an id: device ids are 32 hex characters, so this
/// can never collide with one, and a receiver can tell "the relay told me
/// something" from "a device sent me something" without trusting anything else.
pub const RELAY_SENDER: &str = "relay";

/// Everything the relay can refuse, or that a device can be told.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RelayError {
    #[error("relay frame: {0}")]
    Frame(String),
    #[error("relay protocol version {found} is not supported (this peer speaks {RELAY_VERSION})")]
    Version { found: u32 },
    #[error("relay envelope is {size} bytes, over the {MAX_ENVELOPE_BYTES}-byte cap")]
    TooLarge { size: usize },
    /// The buffer does not hold a whole envelope yet — the only error a stream
    /// reader may retry on. Everything else is a peer that is wrong, not slow.
    #[error("relay envelope is incomplete (want {want} bytes, have {have})")]
    Incomplete { want: usize, have: usize },
    #[error("the relay refused the session: {reason}")]
    Refused { reason: String },
    #[error("the relay did not answer in time")]
    Timeout,
    #[error("relay transport: {0}")]
    Transport(String),
    #[error("the peer's certificate does not authorize it: {0}")]
    Cert(String),
    #[error("the peer did not prove it holds its key")]
    NoProof,
}

/// The header the relay routes on. It is the *only* part of an envelope the
/// relay decodes, which is what makes "routes bytes it cannot read" checkable:
/// nothing here can hold pane text, agent state, or a key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayHeader {
    pub v: u32,
    pub account_id: String,
    /// Who sent it. The relay checks this against the authenticated session, so
    /// a device cannot speak as another.
    pub src_device: String,
    /// Who it is for.
    pub dst: String,
    /// Monotonic per sender, so a receiver can order and de-duplicate.
    pub seq: u64,
    pub kind: RelayKind,
}

/// One routed message: a header plus bytes the relay never interprets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayEnvelope {
    pub header: RelayHeader,
    pub payload: Vec<u8>,
}

impl RelayEnvelope {
    /// Encode as `[u32 LE len][MessagePack header][payload]`.
    ///
    /// `len` counts the header and the payload together, so a reader knows the
    /// whole envelope before allocating; the header's own length comes from how
    /// far the MessagePack decoder got, which is how the payload stays opaque
    /// without a second length field.
    pub fn encode(&self) -> Result<Vec<u8>, RelayError> {
        let header = rmp_serde::to_vec(&self.header)
            .map_err(|e| RelayError::Frame(format!("header encode: {e}")))?;
        let total = header.len() + self.payload.len();
        if total > MAX_ENVELOPE_BYTES {
            return Err(RelayError::TooLarge { size: total });
        }
        let mut out = Vec::with_capacity(4 + total);
        out.extend_from_slice(&(total as u32).to_le_bytes());
        out.extend_from_slice(&header);
        out.extend_from_slice(&self.payload);
        Ok(out)
    }

    /// Decode one envelope from `buffer`, returning it and the bytes consumed.
    ///
    /// Total on garbage: a short buffer is [`RelayError::Incomplete`] (the one
    /// error a stream reader retries on), anything else is a failure — never a
    /// panic and never a partial read.
    pub fn decode(buffer: &[u8]) -> Result<(Self, usize), RelayError> {
        if buffer.len() < 4 {
            return Err(RelayError::Incomplete {
                want: 4,
                have: buffer.len(),
            });
        }
        let len = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
        if len > MAX_ENVELOPE_BYTES {
            return Err(RelayError::TooLarge { size: len });
        }
        if buffer.len() < 4 + len {
            return Err(RelayError::Incomplete {
                want: 4 + len,
                have: buffer.len(),
            });
        }
        let body = &buffer[4..4 + len];
        // Decode the header from a cursor and ask where it stopped: the rest is
        // the payload, untouched.
        let mut cursor = std::io::Cursor::new(body);
        let header: RelayHeader = rmp_serde::from_read(&mut cursor)
            .map_err(|e| RelayError::Frame(format!("header decode: {e}")))?;
        if header.v != RELAY_VERSION {
            return Err(RelayError::Version { found: header.v });
        }
        let header_len = cursor.position() as usize;
        let payload = body[header_len..].to_vec();
        Ok((Self { header, payload }, len + 4))
    }
}

/// Device → relay, first message: who is calling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub v: u32,
    pub account_id: String,
    pub device_id: String,
}

/// Relay → device: the answer to [`Hello`].
///
/// A tagged enum rather than two message types the client must tell apart by
/// trying both: a refusal and a challenge are both "a reply to Hello", and
/// making that explicit is what keeps the client's parse unambiguous.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HelloReply {
    /// Prove you hold your key by signing this nonce.
    Challenge { v: u32, nonce: Vec<u8> },
    /// The session is refused before any proof is asked for.
    Refused { v: u32, reason: String },
}

/// Device → relay: the certificate, and the signature over the challenge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Auth {
    pub v: u32,
    /// The T-0025 certificate, MessagePack-encoded by [`DeviceCert::encode`].
    pub cert: Vec<u8>,
    /// ed25519 signature over [`proof_payload`].
    pub signature: Vec<u8>,
}

/// Relay → device: the answer to [`Auth`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthReply {
    /// The session is up.
    Welcome {
        v: u32,
        account_id: String,
        device_id: String,
    },
    /// Refused, with the reason to show a human.
    Refused { v: u32, reason: String },
}

/// What became of one envelope, carried as the payload of a
/// [`RelayKind::Status`] envelope from [`RELAY_SENDER`].
///
/// Delivery is reported rather than assumed: a sender told `Offline` knows its
/// message was not delivered, which is the honest shape until the durable inbox
/// (T-0030) makes that case a queue instead of an answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Delivered,
    /// The destination is a known device that is not connected (or has stopped
    /// reading). T-0030 turns this branch into a durable queue.
    Offline,
    /// The message was committed to the destination's durable inbox and will be
    /// delivered when it drains (T-0030). Distinct from `Offline` on purpose: a
    /// sender that is told `Queued` knows its bytes are on disk, and one told
    /// `Offline` knows they are not.
    Queued {
        /// What is queued for that device now, so a sender can see a queue
        /// growing rather than discovering it later.
        queued: u64,
    },
    /// No such device in this account.
    NoSuchDevice,
    /// The relay would not carry this envelope at all — a spoofed sender, a
    /// foreign account, a device trying to originate a status. Reported rather
    /// than dropped silently, so a misbehaving client learns why.
    Refused {
        reason: String,
    },
}

/// 32 bytes of OS entropy for a handshake nonce.
///
/// Lives here so the relay needs no dependency of its own for it, and so the
/// one place that mints a nonce is next to the one place that checks one.
pub fn fresh_nonce() -> Result<[u8; 32], RelayError> {
    let mut nonce = [0u8; 32];
    getrandom::fill(&mut nonce).map_err(|e| RelayError::Transport(format!("no entropy: {e}")))?;
    Ok(nonce)
}

/// Device → relay: drain my inbox from this cursor (T-0030).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrainRequest {
    pub v: u32,
    /// The first sequence number wanted; a device that has acked nothing asks
    /// from 1.
    pub from_seq: u64,
}

/// Device → relay: I hold everything up to and including `seq` (T-0030).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {
    pub v: u32,
    pub seq: u64,
}

/// Relay → device, as the payload of a [`RelayKind::Status`] envelope after a
/// drain: what the drain did, including the drops it is reporting.
///
/// A drop the consumer is never told about is a drop that silently lost
/// something, so the count travels on every drain — including the zeroes, which
/// are the reassurance that nothing was lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrainReport {
    pub v: u32,
    /// Messages delivered by this drain.
    pub delivered: u64,
    /// Messages dropped (evicted by a bound, or expired) since the last drain.
    pub dropped: u64,
    /// Messages that expired during this drain.
    pub expired: u64,
    /// What is still queued for this device afterwards.
    pub queued: u64,
    /// The cursor to resume from next time.
    pub next_seq: u64,
}

/// What can go wrong on the device side of the protocol.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("{0}")]
    Protocol(#[from] RelayError),
    #[error("relay transport: {0}")]
    Transport(String),
    #[error("the relay did not answer in time")]
    Timeout,
    #[error("the relay refused the session: {reason}")]
    Refused { reason: String },
}

/// The bytes a device signs to prove it holds its key.
///
/// Bound to the account and the device so a signature harvested in one
/// handshake cannot be replayed into another session, and prefixed with a fixed
/// label so this signature can never be mistaken for one this product uses
/// elsewhere (a cert payload, an audit entry).
///
/// `device_id` must be the **canonical** id ([`DeviceId::as_str`], the bare 32
/// hex characters), not whatever spelling the peer announced. That is what makes
/// the proof independent of the wire form: a client may announce `dev_<hex>` or
/// the bare hex (both parse), and signs the same bytes either way. Signing the
/// announcement instead would make the crypto depend on a cosmetic choice, and
/// a third-party client would have to keep two spellings in sync to be accepted.
#[must_use]
pub fn proof_payload(nonce: &[u8], account_id: &str, device_id: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(nonce.len() + account_id.len() + device_id.len() + 32);
    out.extend_from_slice(b"arreo-relay-auth-v1\0");
    out.extend_from_slice(nonce);
    out.push(0);
    out.extend_from_slice(account_id.as_bytes());
    out.push(0);
    out.extend_from_slice(device_id.as_bytes());
    out
}

/// Frame one MessagePack message as `[u32 LE len][body]`.
///
/// Shared by both sides so the handshake and the envelope stream cannot drift
/// apart on their framing, and so there is exactly one place that enforces the
/// cap.
pub fn encode_message<T: Serialize>(message: &T) -> Result<Vec<u8>, RelayError> {
    let body = rmp_serde::to_vec(message)
        .map_err(|e| RelayError::Frame(format!("message encode: {e}")))?;
    if body.len() > MAX_HANDSHAKE_BYTES {
        return Err(RelayError::TooLarge { size: body.len() });
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decode one framed MessagePack message.
pub fn decode_message<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, RelayError> {
    if body.len() > MAX_HANDSHAKE_BYTES {
        return Err(RelayError::TooLarge { size: body.len() });
    }
    rmp_serde::from_slice(body).map_err(|e| RelayError::Frame(format!("message decode: {e}")))
}

/// Encode a value for carriage *inside* an envelope payload.
///
/// No length prefix: the envelope already delimits its payload, and a prefix
/// here would be read as data by the other side (which is exactly the bug this
/// pair exists to prevent — a `decode_message` on a payload that carried a
/// prefix fails with "invalid value: integer 10, expected variant index").
pub fn encode_payload<T: Serialize>(value: &T) -> Result<Vec<u8>, RelayError> {
    rmp_serde::to_vec(value).map_err(|e| RelayError::Frame(format!("payload encode: {e}")))
}

/// Decode a value carried inside an envelope payload (see [`encode_payload`]).
pub fn decode_payload<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, RelayError> {
    rmp_serde::from_slice(bytes).map_err(|e| RelayError::Frame(format!("payload decode: {e}")))
}

/// Write one already-encoded frame and flush it.
pub async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &[u8],
) -> Result<(), RelayError> {
    use tokio::io::AsyncWriteExt;
    writer
        .write_all(frame)
        .await
        .map_err(|e| RelayError::Transport(e.to_string()))?;
    writer
        .flush()
        .await
        .map_err(|e| RelayError::Transport(e.to_string()))
}

/// Read one frame body into `buf`, returning the bytes of that frame.
///
/// `cap` is the largest body this reader will accept — the handshake cap for a
/// handshake, the envelope cap for the stream — so a peer cannot make the
/// reader allocate whatever it asks for. A partial frame stays in `buf`, so a
/// caller may read the same buffer again after the next chunk arrives.
pub async fn read_frame<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
    cap: usize,
) -> Result<Vec<u8>, RelayError> {
    use tokio::io::AsyncReadExt;
    loop {
        if buf.len() >= 4 {
            let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
            if len > cap {
                return Err(RelayError::TooLarge { size: len });
            }
            if buf.len() >= 4 + len {
                let body = buf[4..4 + len].to_vec();
                buf.drain(..4 + len);
                return Ok(body);
            }
        }
        let mut chunk = [0u8; 8192];
        let read = reader
            .read(&mut chunk)
            .await
            .map_err(|e| RelayError::Transport(e.to_string()))?;
        if read == 0 {
            return Err(RelayError::Transport("peer closed the session".into()));
        }
        buf.extend_from_slice(&chunk[..read]);
    }
}

/// Read one whole envelope from a stream.
///
/// The companion to [`RelayEnvelope::encode`], and the only correct way to read
/// one: the envelope carries its own length prefix, so a reader must hand
/// `decode` the prefix *and* the body. Using the generic [`read_frame`] here
/// would strip the prefix and hand `decode` a body it would then read as a
/// length — a double-prefix confusion that produces a bogus size (observed, and
/// why this function exists).
pub async fn read_envelope<R: tokio::io::AsyncRead + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> Result<RelayEnvelope, RelayError> {
    use tokio::io::AsyncReadExt;
    loop {
        match RelayEnvelope::decode(buf) {
            Ok((envelope, consumed)) => {
                buf.drain(..consumed);
                return Ok(envelope);
            }
            // The one retryable case: not enough bytes yet.
            Err(RelayError::Incomplete { .. }) => {}
            Err(other) => return Err(other),
        }
        let mut chunk = [0u8; 8192];
        let read = reader
            .read(&mut chunk)
            .await
            .map_err(|e| RelayError::Transport(e.to_string()))?;
        if read == 0 {
            return Err(RelayError::Transport("peer closed the session".into()));
        }
        buf.extend_from_slice(&chunk[..read]);
    }
}

/// What the relay learned about a session, once it authenticated.
///
/// Returned by the relay's verification so the routing layer never re-derives
/// identity from the wire: `device` is the id the *certificate* names, and the
/// checks below have already established that it equals the announced one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedDevice {
    pub account_id: String,
    pub device_id: DeviceId,
    pub public_key: VerifyingKey,
    pub cert: DeviceCert,
}

/// Verify an [`Auth`] against the account's root key and the relay's nonce.
///
/// This is the whole trust decision, in one function so it has one test suite
/// and one reviewer. It refuses, in order: a certificate that does not verify
/// under the account root, a certificate for a different device than the one
/// announced, and a missing or invalid proof of possession.
pub fn verify_auth(
    account_id: &str,
    announced_device: &str,
    root: &VerifyingKey,
    nonce: &[u8],
    auth: &Auth,
) -> Result<AuthenticatedDevice, RelayError> {
    if auth.v != RELAY_VERSION {
        return Err(RelayError::Version { found: auth.v });
    }
    let cert = DeviceCert::decode(&auth.cert)
        .map_err(|e| RelayError::Cert(format!("undecodable certificate: {e}")))?;
    let key = cert
        .public_key()
        .ok_or_else(|| RelayError::Cert("certificate carries no usable key".into()))?;

    // 1. The certificate must be the account's: signed by the root the account
    //    registered, for the key it carries.
    cert.verify(root, &key)
        .map_err(|e| RelayError::Cert(e.to_string()))?;

    // 2. It must be the announced device's certificate. Without this, a device
    //    could present *someone else's* valid certificate and speak as them.
    //
    //    Compared as parsed ids, never as strings: the wire carries the
    //    `dev_<hex>` display form while the certificate holds the bare hex, and
    //    a string comparison across that boundary refuses every honest device
    //    (the defect T-0023 hit in the transport's resolver). Parsing first
    //    makes both spellings the same value, and a malformed announcement is a
    //    refusal rather than a mismatch.
    let announced = DeviceId::parse(announced_device)
        .map_err(|e| RelayError::Cert(format!("malformed device id: {e}")))?;
    if cert.device() != &announced {
        return Err(RelayError::Cert(format!(
            "certificate names {}, not the announced {}",
            cert.device(),
            announced
        )));
    }
    // The canonical id, from the *certificate* — which the check above has just
    // established is the announced device — so the proof does not depend on the
    // spelling the peer chose to announce.
    let canonical = cert.device().as_str();

    // 3. And it must prove possession: the certificate is a public document, so
    //    the signature over the relay's fresh nonce is what makes this a
    //    session rather than a claim.
    if auth.signature.len() != 64 {
        return Err(RelayError::NoProof);
    }
    let mut signature = [0u8; 64];
    signature.copy_from_slice(&auth.signature);
    let payload = proof_payload(nonce, account_id, canonical);
    if !crate::identity::keys::verify(
        &key,
        &payload,
        &ed25519_dalek::Signature::from_bytes(&signature),
    ) {
        return Err(RelayError::NoProof);
    }

    Ok(AuthenticatedDevice {
        account_id: account_id.to_string(),
        device_id: cert.device().clone(),
        public_key: key,
        cert,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{DeviceKey, Role, RootKey};

    fn envelope(seq: u64, payload: &[u8]) -> RelayEnvelope {
        RelayEnvelope {
            header: RelayHeader {
                v: RELAY_VERSION,
                account_id: "acct-1".into(),
                src_device: "dev_11111111111111111111111111111111".into(),
                dst: "dev_22222222222222222222222222222222".into(),
                seq,
                kind: RelayKind::Frame,
            },
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn an_envelope_round_trips_and_the_payload_is_never_parsed() {
        // A payload that is deliberately not valid MessagePack: the relay must
        // carry it byte-for-byte, and decoding must not touch it.
        let payload = b"\x00\xffnot-msgpack\x80\x81\xc1";
        let encoded = envelope(7, payload).encode().expect("encode");
        let (decoded, consumed) = RelayEnvelope::decode(&encoded).expect("decode");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.header.seq, 7);
        assert_eq!(decoded.header.kind, RelayKind::Frame);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn a_truncated_or_oversized_envelope_is_refused_not_guessed() {
        let encoded = envelope(1, b"hello").encode().expect("encode");
        for cut in 1..encoded.len() {
            assert!(
                matches!(
                    RelayEnvelope::decode(&encoded[..cut]),
                    Err(RelayError::Incomplete { .. })
                ),
                "a {cut}-byte prefix is incomplete, which is the only retryable case"
            );
        }
        // A length prefix claiming more than the cap is refused before any
        // allocation is attempted.
        let mut oversized = ((MAX_ENVELOPE_BYTES + 1) as u32).to_le_bytes().to_vec();
        oversized.extend_from_slice(b"x");
        assert!(matches!(
            RelayEnvelope::decode(&oversized),
            Err(RelayError::TooLarge { .. })
        ));
        // And a foreign version is refused by name.
        let mut foreign = envelope(1, b"x");
        foreign.header.v = 99;
        let bytes = foreign.encode().expect("encode");
        assert!(matches!(
            RelayEnvelope::decode(&bytes),
            Err(RelayError::Version { found: 99 })
        ));
    }

    #[test]
    fn an_envelope_over_the_cap_cannot_be_encoded() {
        let huge = envelope(1, &vec![0u8; MAX_ENVELOPE_BYTES]);
        assert!(matches!(huge.encode(), Err(RelayError::TooLarge { .. })));
    }

    /// The whole trust decision, exercised through its refusals — the shape a
    /// security test should have.
    #[test]
    fn auth_accepts_a_real_device_and_refuses_every_impostor() {
        let root = RootKey::generate().expect("entropy");
        let device = DeviceKey::generate().expect("entropy");
        let cert = DeviceCert::issue(&root, &device.public(), "phone", Role::Owner, 1_000, 1);
        let account = "acct-1";
        let device_id = cert.device().display_id();
        // The proof is over the canonical id, not the announced spelling.
        let canonical = cert.device().as_str().to_string();
        let nonce = b"a-fresh-nonce-from-the-relay".to_vec();

        let sign = |payload: &[u8]| {
            let sig = device.sign(payload);
            sig.to_bytes().to_vec()
        };
        let good = Auth {
            v: RELAY_VERSION,
            cert: cert.encode().expect("encode"),
            signature: sign(&proof_payload(&nonce, account, &canonical)),
        };

        // The happy path.
        let authenticated =
            verify_auth(account, &device_id, &root.public(), &nonce, &good).expect("verified");
        assert_eq!(authenticated.device_id.as_str(), cert.device().as_str());
        assert_eq!(
            authenticated.public_key.to_bytes(),
            device.public().to_bytes()
        );

        // 1. A certificate from another root is refused: this is the "unknown
        //    device" case wearing a real certificate.
        let other_root = RootKey::generate().expect("entropy");
        assert!(matches!(
            verify_auth(account, &device_id, &other_root.public(), &nonce, &good),
            Err(RelayError::Cert(_))
        ));

        // 2. Announcing somebody else's device id is refused, even with a valid
        //    certificate: a device may only speak as itself.
        let other_device = DeviceKey::generate().expect("entropy");
        let other_cert = DeviceCert::issue(
            &root,
            &other_device.public(),
            "laptop",
            Role::Owner,
            1_000,
            2,
        );
        let as_other = Auth {
            v: RELAY_VERSION,
            cert: cert.encode().expect("encode"),
            signature: sign(&proof_payload(
                &nonce,
                account,
                &other_cert.device().display_id(),
            )),
        };
        assert!(matches!(
            verify_auth(
                account,
                &other_cert.device().display_id(),
                &root.public(),
                &nonce,
                &as_other
            ),
            Err(RelayError::Cert(_))
        ));

        // 3. No proof of possession: the certificate alone authorizes nothing.
        //    (Anyone who has *seen* a certificate can send these bytes.)
        let no_proof = Auth {
            v: RELAY_VERSION,
            cert: cert.encode().expect("encode"),
            signature: vec![0u8; 64],
        };
        assert!(matches!(
            verify_auth(account, &device_id, &root.public(), &nonce, &no_proof),
            Err(RelayError::NoProof)
        ));

        // 4. A signature over a *different* nonce (a replayed handshake) fails.
        let replayed = Auth {
            v: RELAY_VERSION,
            cert: cert.encode().expect("encode"),
            signature: sign(&proof_payload(b"an-old-nonce", account, &canonical)),
        };
        assert!(matches!(
            verify_auth(account, &device_id, &root.public(), &nonce, &replayed),
            Err(RelayError::NoProof)
        ));

        // 5. And a signature harvested for one account cannot be replayed into
        //    another, because the account id is inside the signed payload.
        let cross_account = Auth {
            v: RELAY_VERSION,
            cert: cert.encode().expect("encode"),
            signature: sign(&proof_payload(&nonce, "acct-2", &canonical)),
        };
        assert!(matches!(
            verify_auth(account, &device_id, &root.public(), &nonce, &cross_account),
            Err(RelayError::NoProof)
        ));

        // 6. A foreign protocol version is refused before any crypto.
        let mut wrong_version = good.clone();
        wrong_version.v = 99;
        assert!(matches!(
            verify_auth(account, &device_id, &root.public(), &nonce, &wrong_version),
            Err(RelayError::Version { found: 99 })
        ));
    }

    /// The proof is over the *canonical* id, so a peer may announce either
    /// spelling and sign the same bytes. Without this, a third-party client that
    /// announced the bare hex would have to know to sign the bare hex too — a
    /// cosmetic choice leaking into the crypto.
    #[test]
    fn the_proof_is_independent_of_how_the_device_id_is_announced() {
        let root = RootKey::generate().expect("entropy");
        let device = DeviceKey::generate().expect("entropy");
        let cert = DeviceCert::issue(&root, &device.public(), "phone", Role::Owner, 1_000, 1);
        let account = "acct-1";
        let canonical = cert.device().as_str().to_string();
        let nonce = b"nonce".to_vec();

        // A client that signs the canonical id, announced two ways.
        let signature = device
            .sign(&proof_payload(&nonce, account, &canonical))
            .to_bytes()
            .to_vec();
        let auth = Auth {
            v: RELAY_VERSION,
            cert: cert.encode().expect("encode"),
            signature,
        };

        let prefixed = cert.device().display_id();
        assert_ne!(prefixed, canonical, "the two spellings really do differ");
        for announced in [canonical.as_str(), prefixed.as_str()] {
            let authenticated = verify_auth(account, announced, &root.public(), &nonce, &auth)
                .unwrap_or_else(|e| panic!("announcing {announced:?} must be accepted: {e}"));
            assert_eq!(authenticated.device_id.as_str(), canonical);
        }

        // And signing the *announced* spelling is not accepted, which is what
        // makes the rule "canonical" rather than "whatever you sent".
        let spelling_bound = Auth {
            v: RELAY_VERSION,
            cert: cert.encode().expect("encode"),
            signature: device
                .sign(&proof_payload(&nonce, account, &prefixed))
                .to_bytes()
                .to_vec(),
        };
        assert!(matches!(
            verify_auth(account, &prefixed, &root.public(), &nonce, &spelling_bound),
            Err(RelayError::NoProof)
        ));
    }

    #[test]
    fn an_undecodable_certificate_is_refused() {
        let root = RootKey::generate().expect("entropy");
        let auth = Auth {
            v: RELAY_VERSION,
            cert: b"not a certificate".to_vec(),
            signature: vec![0u8; 64],
        };
        assert!(matches!(
            verify_auth("acct", "dev_x", &root.public(), b"n", &auth),
            Err(RelayError::Cert(_))
        ));
    }
}
