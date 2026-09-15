//! The typed errors that cross the boundary (T-0104).
//!
//! One sentence: one flat `#[derive(uniffi::Error)]` enum per `arreo-core`
//! error type, one variant per core variant, and the variant's payload is the
//! **core error's own rendered sentence** — so the message a phone shows and
//! the message the CLI prints are the same string by construction rather than
//! by a comment asking two format strings to stay in step.
//!
//! Why the payload is the sentence and not the structured fields: UniFFI's flat
//! errors cross as `(variant index, Display string)` — the fields never reach
//! the foreign side — and a rich error would make the foreign `message`
//! `field=${field}` instead of the sentence (measured against
//! `bindings/kotlin/templates/ErrorTemplate.kt` in uniffi 0.32). So the
//! sentence is the payload, the `From` impls are exhaustive over the core
//! enums, and a new core variant is a **compile error here** rather than a
//! silently-unmapped state.
//!
//! The one thing each enum adds is its own boundary precondition, marked
//! `boundary` in the source and in `docs/mobile.md`: the CLI has no sentence for
//! "this seed is not 32 bytes" because the CLI never accepts a seed from
//! outside — a phone does.

use arreo_core::identity::{CertError, KeyError, RoleError};
use arreo_core::mesh::DirectoryError;
use arreo_core::pairing::PairingError;
use arreo_core::proto::codec::CodecError;
use arreo_core::relay::session::SessionError;
use arreo_core::relay::ClientError;
use arreo_core::theme::ColorError;

/// The boundary's seed precondition, in one place.
///
/// Both [`KeyFfiError`] and [`PairingFfiError`] accept a device seed (one to
/// build a keypair, one to pair with), so the sentence lives here rather than
/// twice: the CLI has no equivalent because it never takes a seed from outside —
/// a phone supplies its own, from the platform CSPRNG.
pub(crate) fn bad_seed(got: usize) -> String {
    format!("a device seed is exactly 32 bytes, got {got}")
}

/// Everything `arreo_core::pairing` can refuse, as the CLI prints it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum PairingFfiError {
    #[error("{0}")]
    Entropy(String),
    #[error("{0}")]
    CodeShape(String),
    #[error("{0}")]
    UnknownWord(String),
    #[error("{0}")]
    BadInvite(String),
    #[error("{0}")]
    Mailbox(String),
    #[error("{0}")]
    Timeout(String),
    #[error("{0}")]
    Spake(String),
    #[error("{0}")]
    CodeMismatch(String),
    #[error("{0}")]
    Confirmation(String),
    #[error("{0}")]
    Identity(String),
    #[error("{0}")]
    Certificate(String),
    /// The boundary's own precondition (see [`bad_seed`]).
    #[error("{0}")]
    BadSeed(String),
}

impl From<PairingError> for PairingFfiError {
    fn from(error: PairingError) -> Self {
        match &error {
            PairingError::Entropy(_) => Self::Entropy(error.to_string()),
            PairingError::CodeShape { .. } => Self::CodeShape(error.to_string()),
            PairingError::UnknownWord { .. } => Self::UnknownWord(error.to_string()),
            PairingError::BadInvite(_) => Self::BadInvite(error.to_string()),
            PairingError::Mailbox(_) => Self::Mailbox(error.to_string()),
            PairingError::Timeout { .. } => Self::Timeout(error.to_string()),
            PairingError::Spake(_) => Self::Spake(error.to_string()),
            PairingError::CodeMismatch => Self::CodeMismatch(error.to_string()),
            PairingError::Confirmation => Self::Confirmation(error.to_string()),
            PairingError::Identity(_) => Self::Identity(error.to_string()),
            PairingError::Certificate => Self::Certificate(error.to_string()),
        }
    }
}

/// Everything `arreo_core::identity::keys` can refuse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum KeyFfiError {
    #[error("{0}")]
    Entropy(String),
    #[error("{0}")]
    Io(String),
    #[error("{0}")]
    Malformed(String),
    #[error("{0}")]
    Permissions(String),
    #[error("{0}")]
    Missing(String),
    #[error("{0}")]
    PublicKey(String),
    #[error("{0}")]
    Format(String),
    /// The boundary's own precondition (see [`bad_seed`]).
    #[error("{0}")]
    BadSeed(String),
    /// A public key that cannot be pinned: a small-order ed25519 point.
    ///
    /// **The sentence is the core's, not a copy.** `arreo_core::identity` refuses the
    /// same key with `AuthorityError::WeakKey`, and this variant carries that error's
    /// own `Display` — so the FFI door and the CLI door say the same thing by
    /// construction, and a reworded core sentence cannot leave this one behind. See
    /// `device_cert_issue` for why the check has to be repeated at this boundary.
    #[error("{0}")]
    WeakKey(String),
}

impl From<KeyError> for KeyFfiError {
    fn from(error: KeyError) -> Self {
        match &error {
            KeyError::Entropy(_) => Self::Entropy(error.to_string()),
            KeyError::Io { .. } => Self::Io(error.to_string()),
            KeyError::Malformed { .. } => Self::Malformed(error.to_string()),
            KeyError::Permissions { .. } => Self::Permissions(error.to_string()),
            KeyError::Missing { .. } => Self::Missing(error.to_string()),
            KeyError::PublicKey(_) => Self::PublicKey(error.to_string()),
            KeyError::Format(_) => Self::Format(error.to_string()),
        }
    }
}

/// Everything `arreo_core::identity::cert` can refuse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum CertFfiError {
    #[error("{0}")]
    Decode(String),
    #[error("{0}")]
    Encode(String),
    #[error("{0}")]
    Version(String),
    #[error("{0}")]
    DeviceMismatch(String),
    #[error("{0}")]
    BadSignature(String),
    #[error("{0}")]
    ZeroSerial(String),
    #[error("{0}")]
    BadDeviceId(String),
    #[error("{0}")]
    NoCert(String),
    #[error("{0}")]
    KeyMismatch(String),
    #[error("{0}")]
    Revoked(String),
    #[error("{0}")]
    RotatedAway(String),
    #[error("{0}")]
    Io(String),
    /// The boundary's own refusal: a key that is not 64 hex characters cannot be
    /// a pinned root or a presented device key, and saying so beats reporting it
    /// as a certificate mismatch — the mismatch sentence is about a *valid* key
    /// that is the wrong one.
    #[error("the {which} public key is unusable: {detail}")]
    BadPublicKey { which: String, detail: String },
}

impl From<CertError> for CertFfiError {
    fn from(error: CertError) -> Self {
        match &error {
            CertError::Decode(_) => Self::Decode(error.to_string()),
            CertError::Encode(_) => Self::Encode(error.to_string()),
            CertError::Version { .. } => Self::Version(error.to_string()),
            CertError::DeviceMismatch { .. } => Self::DeviceMismatch(error.to_string()),
            CertError::BadSignature => Self::BadSignature(error.to_string()),
            CertError::ZeroSerial => Self::ZeroSerial(error.to_string()),
            CertError::BadDeviceId(_) => Self::BadDeviceId(error.to_string()),
            CertError::NoCert(_) => Self::NoCert(error.to_string()),
            CertError::KeyMismatch { .. } => Self::KeyMismatch(error.to_string()),
            CertError::Revoked(_) => Self::Revoked(error.to_string()),
            CertError::RotatedAway { .. } => Self::RotatedAway(error.to_string()),
            CertError::Io { .. } => Self::Io(error.to_string()),
        }
    }
}

/// Everything `arreo_core::identity::role` can refuse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum RoleFfiError {
    #[error("{0}")]
    Unknown(String),
    #[error("{0}")]
    Denied(String),
}

impl From<RoleError> for RoleFfiError {
    fn from(error: RoleError) -> Self {
        match &error {
            RoleError::Unknown(_) => Self::Unknown(error.to_string()),
            RoleError::Denied { .. } => Self::Denied(error.to_string()),
        }
    }
}

/// Everything a live relay session can refuse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum SessionFfiError {
    /// The session's inner client error, already rendered.
    #[error("{0}")]
    Client(String),
    /// The relay answered and said no.
    ///
    /// Split out from `Client` because it is the one distinction the product
    /// itself acts on: the daemon's reconnect loop retries a transport that is
    /// absent and does *not* retry a refusal (`arreo-server/src/relay_client.rs`
    /// matches `ClientError::Refused` for exactly this), and a phone that retried
    /// a bad certificate in a tight loop would be a phone that looks hung. The
    /// sentence is the core's own, unchanged.
    #[error("{0}")]
    Refused(String),
    #[error("{0}")]
    Closed(String),
    #[error("{0}")]
    NoStream(String),
    #[error("{0}")]
    Timeout(String),
    /// The boundary's own precondition: the relay address is not `IP:PORT`.
    /// The CLI's equivalent sentence is the CLI's own (`machines: cannot reach
    /// the relay at …`); this one names the *parse*, because that is the step
    /// that failed here, before any socket existed.
    #[error("the relay address {text:?} is not an IP:PORT address: {detail}")]
    BadAddress { text: String, detail: String },
    /// The boundary's own precondition: the relay confirmed an identity that is
    /// not the one this device's own key names.
    ///
    /// The relay authenticates a device from its certificate and answers
    /// `AuthReply::Welcome` with the id it verified. This crate derives that id
    /// from the key it dialed with and compares the two, because the id a phone
    /// reports and asserts over a peer stream is a claim about *itself*: a relay
    /// that names a different device is broken or lying, and its word is not
    /// this device's identity. Unreachable with an honest relay — the id it
    /// echoes is derived from the certificate it just verified, and that
    /// certificate's key is the one the proof was signed with.
    #[error("the relay confirmed {confirmed} for a device whose own key names {derived}")]
    RelayIdentityMismatch { confirmed: String, derived: String },
    /// The boundary's own precondition: the pinned machine key is not a public
    /// key. The sentence is `arreo_core::identity::KeyError`'s own `Display` —
    /// the key parser is the core's, so the words a phone shows are the words
    /// the CLI shows for the same bad hex.
    #[error("{0}")]
    BadPeerKey(String),
    /// Talking to a machine's daemon failed: the secure channel could not be
    /// established, it broke mid-answer, or the frame could not be built for it.
    /// The sentence is the core's own — `TransportError`'s for the Noise
    /// handshake, `mesh::ClientError`'s for a channel or a codec that failed
    /// under a client — so the words are the ones the CLI's remote client
    /// renders for the same state.
    #[error("{0}")]
    Peer(String),
    /// The machine's daemon answered a request with a refusal, in its own
    /// sentence — which is what the CLI prints for the same answer
    /// (`metrics history: {message}`, `notify act: {detail}`). Both shapes a
    /// refusal arrives in cross here: an answered `Message::Error` (the trust
    /// gate's sentence, or a verb the daemon does not know) and a
    /// `NotifyActReply { ok: false }` (the pane's state, the state gate, the reply
    /// bound).
    #[error("{0}")]
    Daemon(String),
}

impl From<SessionError> for SessionFfiError {
    fn from(error: SessionError) -> Self {
        match &error {
            SessionError::Client(ClientError::Refused { .. }) => Self::Refused(error.to_string()),
            SessionError::Client(_) => Self::Client(error.to_string()),
            SessionError::Closed => Self::Closed(error.to_string()),
            SessionError::NoStream(_) => Self::NoStream(error.to_string()),
            SessionError::Timeout => Self::Timeout(error.to_string()),
        }
    }
}

/// Everything the machine directory can refuse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum DirectoryFfiError {
    #[error("{0}")]
    BadMachineId(String),
    #[error("{0}")]
    InvalidName(String),
    #[error("{0}")]
    NameTaken(String),
    #[error("{0}")]
    NoSuchMachine(String),
    #[error("{0}")]
    NoSuchName(String),
    #[error("{0}")]
    Ticket(String),
}

impl From<DirectoryError> for DirectoryFfiError {
    fn from(error: DirectoryError) -> Self {
        match &error {
            DirectoryError::BadMachineId(_) => Self::BadMachineId(error.to_string()),
            DirectoryError::InvalidName(_) => Self::InvalidName(error.to_string()),
            DirectoryError::NameTaken(_) => Self::NameTaken(error.to_string()),
            DirectoryError::NoSuchMachine(_) => Self::NoSuchMachine(error.to_string()),
            DirectoryError::NoSuchName(_) => Self::NoSuchName(error.to_string()),
            DirectoryError::Ticket { .. } => Self::Ticket(error.to_string()),
        }
    }
}

/// Everything the wire codec can refuse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum CodecFfiError {
    #[error("{0}")]
    Encode(String),
    #[error("{0}")]
    Decode(String),
    #[error("{0}")]
    Version(String),
    #[error("{0}")]
    Truncated(String),
    /// The boundary's own refusal: a value the wire carries as `u64` that this
    /// platform cannot address. Unreachable on the 64-bit targets the product
    /// ships, and here rather than an `as` cast because a silent wrap would be a
    /// line number pointing into scrollback the caller never meant to read.
    #[error("{field} = {value} is larger than this platform can address")]
    OutOfRange { field: String, value: u64 },
}

impl From<CodecError> for CodecFfiError {
    fn from(error: CodecError) -> Self {
        match &error {
            CodecError::Encode(_) => Self::Encode(error.to_string()),
            CodecError::Decode(_) => Self::Decode(error.to_string()),
            CodecError::Version { .. } => Self::Version(error.to_string()),
            CodecError::Truncated { .. } => Self::Truncated(error.to_string()),
        }
    }
}

/// Everything the theme engine's color model can refuse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum ColorFfiError {
    #[error("{0}")]
    Malformed(String),
    #[error("{0}")]
    OutOfRange(String),
}

impl From<ColorError> for ColorFfiError {
    fn from(error: ColorError) -> Self {
        match &error {
            ColorError::Malformed(_) => Self::Malformed(error.to_string()),
            ColorError::OutOfRange(_) => Self::OutOfRange(error.to_string()),
        }
    }
}
