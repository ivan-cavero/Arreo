//! The push payload (T-0117): **what a delivered notification carries to a
//! device, and how it travels without the relay reading it.**
//!
//! One sentence: a notification the policy *delivers* is also a bounded,
//! self-sufficient message — the pane, the machine, the state, the sentence the
//! audit row carries, the quick actions that answer it and when it happened —
//! sealed to the receiving device's key so the relay's durable inbox stores
//! ciphertext, which is what T-0030 says its rows are.
//!
//! ## Why the payload is sealed, and why it is one-way
//!
//! A push is addressed to a device that may be **offline**, and the relay's
//! inbox is what holds it until the device returns (T-0030). That rules out an
//! interactive handshake: the sender's half would have to wait for a reply that
//! cannot come, and a recorded first flight is useless once its initiator has
//! gone. So the payload is sealed with the *one-way* Noise pattern
//! ([`PUSH_PARAMS`]): the sender needs only the recipient's static key, writes
//! one flight, and the device opens it whenever it comes back.
//!
//! The relay is the reason this exists at all. It routes bytes it cannot read
//! (§4: "cannot read traffic" is an engineering constraint, not a slogan), and
//! the pane id, the machine name and the agent's own sentence are exactly the
//! content that must never be in a relay's database. Sealing with the primitive
//! the transport already uses (`snow`, behind the `transport` feature) keeps
//! that promise without a second crypto stack.
//!
//! **Not replayed-proof, deliberately.** A recorded blob can be replayed by the
//! relay, and the device will decrypt it again — the same property every
//! redelivery has, which is why the criterion names the consumer's `(device,
//! seq)` dedupe rather than a nonce here (T-0030). A nonce would not help: the
//! device cannot tell a replayed envelope from a redelivered one, because they
//! *are* the same envelope.
//!
//! ## The framing, and the reader's half of it
//!
//! A device's byte stream carries every push the relay queued for it,
//! concatenated (one peer stream, one order — that is what makes the drain
//! ordered). So each sealed push is length-prefixed:
//!
//! ```text
//! [u32 LE total][u16 first-flight length][first flight][ciphertext]
//! ```
//!
//! `total` covers everything after itself. The first flight's length is on the
//! wire rather than derived from the pattern, so a reader does not have to know
//! which pattern produced the blob to find where the ciphertext starts.
//! [`open_from`] is that reader: it takes the buffer, answers `Incomplete` until
//! a whole frame has arrived, and reports how many bytes it consumed — the same
//! contract [`crate::proto::codec::decode_frame`] and
//! [`crate::relay::RelayEnvelope::decode`] have, so a stream reader is written
//! once.

use crate::proto::AgentState;
use serde::{Deserialize, Serialize};

use super::NotifyAction;

/// The largest a push's framed encoding may be, in bytes.
///
/// A bound rather than a guess: the payload is peer-derived (a pane id, a
/// machine name, a sentence built from the engine's own matched pattern), and a
/// notification that cannot be delivered inside this many bytes is refused —
/// never truncated, because a truncated sentence is a notification that lies
/// about what the agent is asking.
pub const MAX_PUSH_BYTES: usize = 8 * 1024;

/// The largest the one-way handshake's first flight may be, in bytes.
///
/// The pattern's only wire token in that flight is `e` (32 bytes on 25519), and
/// the slack is deliberate: a reader that had to derive this from `snow`'s
/// internals would be a reader that breaks on a library upgrade, and 64 bytes
/// of bound is cheaper than that coupling.
const MAX_FIRST_FLIGHT: usize = 64;

/// The largest a *sealed* push may be: the framing (4 + 2), the first flight,
/// the plaintext bound and the AEAD tag.
///
/// Checked **before** the buffer is used, so a length prefix from a hostile or
/// corrupt stream is a refusal rather than an allocation.
pub const MAX_SEALED_PUSH_BYTES: usize = 4 + 2 + MAX_FIRST_FLIGHT + MAX_PUSH_BYTES + 16;

/// The one-way pattern a push is sealed with: `K` is the spec's
/// "initiator knows the responder's static key" one-way pattern.
///
/// Spelled in full because the choice is a compatibility surface, exactly like
/// [`crate::transport::noise::NOISE_PARAMS`] — and *different* from it: the
/// transport is interactive (`KK`) because both ends are up, and a push cannot
/// be.
pub const PUSH_PARAMS: &str = "Noise_K_25519_ChaChaPoly_BLAKE2s";

/// What one delivered notification carries to a device.
///
/// **Self-sufficient on purpose**: everything a surface needs to render the
/// notification and offer its answers is here, so a phone that wakes from a
/// push does not need a second round trip to say what happened (the criterion's
/// own words: "a push that needs a second round trip to be renderable is not a
/// push").
///
/// The fields are the audit row's, not a second vocabulary: `sentence` is the
/// same string [`super::Decision::Notify`] carries and the daemon writes to
/// `prompt`, and `actions` is [`super::actions_for`] of `state` — so the row an
/// operator reads and the push a phone renders cannot describe the same
/// transition differently.
///
/// **Append-only, like every wire shape here** (ADR 0017): a new field must be
/// `#[serde(default)]`-optional and appended, because `rmp-serde` encodes a
/// struct as a positional array and an older reader fails the whole decode on a
/// longer one — the rule [`crate::proto::message`] states in full.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushPayload {
    /// The pane the notification is about.
    pub pane: String,
    /// The machine the pane runs on, so a phone with several machines can say
    /// which one is asking.
    pub machine: String,
    /// The state the transition landed in — the same value the row's `state=`
    /// prefix carries.
    pub state: AgentState,
    /// The sentence a human reads: T-0093's row `prompt`, byte for byte.
    pub sentence: String,
    /// The bounded quick actions that answer this notification (T-0094), from
    /// the one function the daemon's act gate also reads.
    pub actions: Vec<NotifyAction>,
    /// When the transition happened, in Unix milliseconds — the transition's
    /// own timestamp, never the moment a tick noticed it.
    pub at_ms: u64,
}

/// What can be wrong with a push, on either side.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PushError {
    /// The encoded payload is past [`MAX_PUSH_BYTES`] — or a sealed blob's
    /// length prefix names more than [`MAX_SEALED_PUSH_BYTES`], which is checked
    /// before the bytes are touched.
    #[error("a push of {size} bytes exceeds the {bound}-byte bound")]
    TooLarge { size: usize, bound: usize },
    /// The frame has not fully arrived: a stream reader retries on this and on
    /// nothing else.
    #[error("a push frame needs {want} bytes, {have} have arrived")]
    Incomplete { want: usize, have: usize },
    #[error("cannot encode the push: {0}")]
    Encode(String),
    #[error("cannot decode the push: {0}")]
    Decode(String),
    /// The plaintext decoded, and it is not a push: a sealed blob that opens to
    /// some other message is a refusal, never a push with invented fields.
    #[error("the sealed blob is not a push")]
    NotAPush,
    /// Sealing or opening failed — the peer's key is not the one that sealed
    /// it, or the bytes are not a sealed push at all. The reason is the
    /// library's own; it names no key material.
    #[error("the push could not be sealed or opened: {0}")]
    Crypto(String),
}

/// The plaintext one push carries: `Message::NotifyPush`, bounded.
///
/// The push travels as an ordinary protocol message *inside* the seal, so every
/// surface decodes it with the one schema it already speaks (`codec`) rather
/// than with a second parser written for this feature — the same reason
/// [`crate::proto::message`] refuses to grow a shape beside the enum. The body
/// is the *bare* message, not a length-prefixed frame: the seal already knows
/// its plaintext's length, and a second prefix inside it would be a second
/// answer to one question.
pub fn encode(payload: &PushPayload) -> Result<Vec<u8>, PushError> {
    let message = crate::proto::Message::NotifyPush {
        v: crate::proto::VERSION,
        payload: Box::new(payload.clone()),
    };
    let body =
        crate::proto::codec::encode(&message).map_err(|e| PushError::Encode(e.to_string()))?;
    if body.len() > MAX_PUSH_BYTES {
        return Err(PushError::TooLarge {
            size: body.len(),
            bound: MAX_PUSH_BYTES,
        });
    }
    Ok(body)
}

/// The payload a plaintext push carries, or a refusal.
pub fn decode(body: &[u8]) -> Result<PushPayload, PushError> {
    if body.len() > MAX_PUSH_BYTES {
        return Err(PushError::TooLarge {
            size: body.len(),
            bound: MAX_PUSH_BYTES,
        });
    }
    let message: crate::proto::Message =
        crate::proto::codec::decode(body).map_err(|e| PushError::Decode(e.to_string()))?;
    match message {
        crate::proto::Message::NotifyPush { payload, .. } => Ok(*payload),
        _ => Err(PushError::NotAPush),
    }
}

/// Seal `payload` to `recipient`, as the framed blob a device reads.
///
/// `local` is the sender's own device key: the pattern authenticates *both*
/// statics, so the device knows the push came from the machine it paired with
/// and not from any other device in the account.
#[cfg(feature = "transport")]
pub fn seal_to(
    recipient: &crate::identity::VerifyingKey,
    local: &crate::identity::keys::NoiseStatic,
    payload: &PushPayload,
) -> Result<Vec<u8>, PushError> {
    use crate::identity::keys::noise_public_key;
    use snow::Builder;

    let plain = encode(payload)?;
    let params = PUSH_PARAMS
        .parse()
        .map_err(|e: snow::Error| PushError::Crypto(e.to_string()))?;
    let mut handshake = Builder::new(params)
        .local_private_key(&local.secret())
        .remote_public_key(&noise_public_key(recipient))
        .build_initiator()
        .map_err(|e| PushError::Crypto(e.to_string()))?;
    let mut flight = vec![0u8; MAX_FIRST_FLIGHT];
    let flight_len = handshake
        .write_message(&[], &mut flight)
        .map_err(|e| PushError::Crypto(e.to_string()))?;
    let mut transport = handshake
        .into_transport_mode()
        .map_err(|e| PushError::Crypto(e.to_string()))?;
    // `+ 16` is the AEAD tag; the buffer is exact because the plaintext is
    // already bounded.
    let mut ciphertext = vec![0u8; plain.len() + 16];
    let ciphertext_len = transport
        .write_message(&plain, &mut ciphertext)
        .map_err(|e| PushError::Crypto(e.to_string()))?;

    let total = 2 + flight_len + ciphertext_len;
    let mut out = Vec::with_capacity(4 + total);
    out.extend_from_slice(&(total as u32).to_le_bytes());
    out.extend_from_slice(&(flight_len as u16).to_le_bytes());
    out.extend_from_slice(&flight[..flight_len]);
    out.extend_from_slice(&ciphertext[..ciphertext_len]);
    Ok(out)
}

/// Open the first sealed push in `buffer`, returning it and the bytes it used.
///
/// `sender` is the machine's key, which the pattern makes a *precondition*: a
/// blob sealed to anyone else's idea of the sender does not open. `Incomplete`
/// means "read more and try again" and is the only retryable answer.
#[cfg(feature = "transport")]
pub fn open_from(
    sender: &crate::identity::VerifyingKey,
    local: &crate::identity::keys::NoiseStatic,
    buffer: &[u8],
) -> Result<(PushPayload, usize), PushError> {
    use crate::identity::keys::noise_public_key;
    use snow::Builder;

    if buffer.len() < 4 {
        return Err(PushError::Incomplete {
            want: 4,
            have: buffer.len(),
        });
    }
    let total = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
    if total > MAX_SEALED_PUSH_BYTES {
        return Err(PushError::TooLarge {
            size: total,
            bound: MAX_SEALED_PUSH_BYTES,
        });
    }
    if buffer.len() < 4 + total {
        return Err(PushError::Incomplete {
            want: 4 + total,
            have: buffer.len(),
        });
    }
    let body = &buffer[4..4 + total];
    if body.len() < 2 {
        return Err(PushError::Crypto(
            "the frame has no first flight".to_string(),
        ));
    }
    let flight_len = u16::from_le_bytes([body[0], body[1]]) as usize;
    if flight_len > MAX_FIRST_FLIGHT || body.len() < 2 + flight_len {
        return Err(PushError::Crypto(
            "the frame's first flight is not inside it".to_string(),
        ));
    }
    let params = PUSH_PARAMS
        .parse()
        .map_err(|e: snow::Error| PushError::Crypto(e.to_string()))?;
    let mut handshake = Builder::new(params)
        .local_private_key(&local.secret())
        .remote_public_key(&noise_public_key(sender))
        .build_responder()
        .map_err(|e| PushError::Crypto(e.to_string()))?;
    handshake
        .read_message(&body[2..2 + flight_len], &mut [])
        .map_err(|e| PushError::Crypto(e.to_string()))?;
    let mut transport = handshake
        .into_transport_mode()
        .map_err(|e| PushError::Crypto(e.to_string()))?;
    let mut plain = vec![0u8; MAX_PUSH_BYTES];
    let plain_len = transport
        .read_message(&body[2 + flight_len..], &mut plain)
        .map_err(|e| PushError::Crypto(e.to_string()))?;
    plain.truncate(plain_len);
    Ok((decode(&plain)?, 4 + total))
}

/// The framing and the bound are pure Rust and are tested in every build. The four
/// **sealing** tests need `feature = "transport"` (they call `noise_static`,
/// `seal_to` and `open_from`, all transport-gated), and are gated individually
/// rather than with the module so the pure-Rust `check-targets` build still
/// exercises the encoding — which is the half a `--no-default-features` check can
/// actually prove.
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "transport")]
    use crate::identity::DeviceKey;

    fn payload() -> PushPayload {
        PushPayload {
            pane: "build-1".to_string(),
            machine: "workbox".to_string(),
            state: AgentState::Blocked,
            sentence: "blocked (inferred:silence)".to_string(),
            actions: super::super::actions_for(AgentState::Blocked),
            at_ms: 1_789_398_272_891,
        }
    }

    /// The payload survives the round trip with every field it promises —
    /// including the ones a phone renders without asking again.
    #[test]
    fn the_payload_round_trips_through_the_frame() {
        let encoded = encode(&payload()).expect("encode");
        assert_eq!(decode(&encoded).expect("decode"), payload());
        // The frame is a `Message` on the wire, not a bespoke shape: the
        // classifier names it, and the op is the one the enum carries.
        assert_eq!(
            crate::proto::codec::decode_op_for_error(&encoded).as_deref(),
            Some("notify_push")
        );
    }

    /// **The bound is a refusal, never a truncation.** A sentence past the
    /// bound must fail the seal rather than arrive shortened — a truncated
    /// notification is one that lies about what the agent is asking.
    #[test]
    fn an_oversized_push_is_refused_rather_than_truncated() {
        let mut big = payload();
        big.sentence = "x".repeat(MAX_PUSH_BYTES);
        assert!(matches!(encode(&big), Err(PushError::TooLarge { .. })));
    }

    /// A sealed blob opens for the device it was sealed to, and for nobody
    /// else — including a device in the same account.
    #[test]
    #[cfg(feature = "transport")]
    fn a_push_opens_for_its_recipient_and_not_for_another_device() {
        let machine = DeviceKey::generate().expect("entropy");
        let phone = DeviceKey::generate().expect("entropy");
        let other = DeviceKey::generate().expect("entropy");

        let sealed = seal_to(&phone.public(), &machine.noise_static(), &payload()).expect("seal");
        let (opened, used) = open_from(&machine.public(), &phone.noise_static(), &sealed)
            .expect("the recipient opens it");
        assert_eq!(opened, payload());
        assert_eq!(used, sealed.len(), "one frame, consumed whole");

        // The same bytes handed to another device: the pattern's responder side
        // is configured with the *sender*, so a different local key cannot
        // derive the same keys — and the failure is typed, never a panic.
        assert!(open_from(&machine.public(), &other.noise_static(), &sealed).is_err());
        // And a blob sealed to someone else does not open for the phone: the
        // sender's half of the pattern is a precondition, not a hint.
        let wrong = seal_to(&other.public(), &machine.noise_static(), &payload()).expect("seal");
        assert!(open_from(&machine.public(), &phone.noise_static(), &wrong).is_err());
    }

    /// The reader's half of the framing: a partial frame is `Incomplete` (the
    /// one retryable answer), a whole one consumes exactly its own bytes, and
    /// two frames in one buffer come out in the order they were written.
    #[test]
    #[cfg(feature = "transport")]
    fn the_reader_waits_for_a_whole_frame_and_consumes_only_it() {
        let machine = DeviceKey::generate().expect("entropy");
        let phone = DeviceKey::generate().expect("entropy");
        let mut first = payload();
        first.pane = "build-1".to_string();
        let mut second = payload();
        second.pane = "build-2".to_string();
        let a = seal_to(&phone.public(), &machine.noise_static(), &first).expect("seal");
        let b = seal_to(&phone.public(), &machine.noise_static(), &second).expect("seal");

        let local = phone.noise_static();
        let sender = machine.public();
        assert!(matches!(
            open_from(&sender, &local, &a[..3]),
            Err(PushError::Incomplete { .. })
        ));
        assert!(matches!(
            open_from(&sender, &local, &a[..a.len() - 1]),
            Err(PushError::Incomplete { .. })
        ));

        let mut stream = Vec::new();
        stream.extend_from_slice(&a);
        stream.extend_from_slice(&b);
        let (one, used) = open_from(&sender, &local, &stream).expect("the first frame");
        assert_eq!(one.pane, "build-1");
        let (two, _) = open_from(&sender, &local, &stream[used..]).expect("the second frame");
        assert_eq!(two.pane, "build-2");
    }

    /// A length prefix naming more than the bound is refused before anything is
    /// allocated — the same rule every other decoder here has.
    #[test]
    #[cfg(feature = "transport")]
    fn a_length_past_the_bound_is_refused_before_the_bytes_are_used() {
        let machine = DeviceKey::generate().expect("entropy");
        let phone = DeviceKey::generate().expect("entropy");
        let mut hostile = (MAX_SEALED_PUSH_BYTES as u32 + 1).to_le_bytes().to_vec();
        hostile.extend_from_slice(&[0u8; 8]);
        assert!(matches!(
            open_from(&machine.public(), &phone.noise_static(), &hostile),
            Err(PushError::TooLarge { .. })
        ));
    }

    /// A sealed blob whose plaintext is some *other* message is not a push: the
    /// reader refuses it rather than filling the fields it happens to find.
    #[test]
    #[cfg(feature = "transport")]
    fn a_sealed_blob_that_is_not_a_push_is_refused() {
        let body = crate::proto::codec::encode(&crate::proto::Message::Ok {
            v: crate::proto::VERSION,
        })
        .expect("encode");
        assert_eq!(decode(&body), Err(PushError::NotAPush));
    }
}
