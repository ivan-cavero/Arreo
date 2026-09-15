//! Device identity and its fingerprint (T-0104).
//!
//! One sentence: a phone is a *device* — an ed25519 keypair and a
//! server-signed certificate that pins it — and this module hands the foreign
//! side the keypair as a **seed it supplies** and the certificate as bytes it
//! stores, so the secret lives in the platform keystore (Keychain, Android
//! Keystore) and never in a path this crate chooses.
//!
//! **Why the seed comes in rather than out.** `arreo_core::identity::DeviceKey`
//! deliberately exposes no way to read the secret back, and the CLI's answer is
//! a 0600 file — which is a filesystem, which is the one thing this crate may
//! not have. So the boundary is inverted: the foreign side generates 32 bytes
//! with its own CSPRNG (`SecureRandom`, `SecRandomCopyBytes`), hands them in,
//! and keeps them; every call here is a pure function of what it is given. That
//! is also strictly better for a phone — the secret never crosses the boundary
//! in either direction, and there is nothing to leak into a crash report.
//!
//! The **fingerprint** is `DeviceId::from_key`: `sha256(pubkey)` truncated to
//! 128 bits, lowercase hex. It is the device's identity — what a certificate
//! binds to and what an audit row names — and the `dev_` form is the display
//! spelling the CLI prints.

use std::sync::Arc;

use arreo_core::identity::{DeviceCert, DeviceId, DeviceKey, Role, RootKey, VerifyingKey};

use crate::errors::{CertFfiError, KeyFfiError, RoleFfiError};

/// A device's role, as the certificate carries it.
///
/// The same two values `arreo_core::identity::Role` has, in the same words
/// `Role::as_str` prints, so a phone renders "owner" exactly where the CLI does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRole {
    Owner,
    Viewer,
}

impl From<Role> for FfiRole {
    fn from(role: Role) -> Self {
        match role {
            Role::Owner => Self::Owner,
            Role::Viewer => Self::Viewer,
        }
    }
}

impl From<FfiRole> for Role {
    fn from(role: FfiRole) -> Self {
        match role {
            FfiRole::Owner => Self::Owner,
            FfiRole::Viewer => Self::Viewer,
        }
    }
}

/// The operator's word for a role — the CLI's own spelling (`owner`, `viewer`).
#[uniffi::export]
pub fn role_word(role: FfiRole) -> String {
    Role::from(role).as_str().to_string()
}

/// Parse a role word.
///
/// Accepts both spellings the product uses (`operator` is the roadmap's word for
/// `owner`), because `Role::parse` does — one parser, not two.
#[uniffi::export]
pub fn role_parse(text: String) -> Result<FfiRole, RoleFfiError> {
    Ok(FfiRole::from(Role::parse(&text)?))
}

/// A device keypair. The secret never leaves the object.
///
/// `Debug` is the core's own: `DeviceKey`'s hand-written impl prints the public
/// half and nothing else, because a debug log is not a key store.
#[derive(Debug, uniffi::Object)]
pub struct DeviceKeyHandle {
    key: DeviceKey,
}

/// Build a device keypair from a caller-supplied 32-byte seed.
///
/// The seed is the secret: it is the platform keystore's to hold. A seed of the
/// wrong length is refused rather than padded or truncated — a truncated seed
/// would be a *different device* with the same bytes in front.
#[uniffi::export]
pub fn device_key_from_seed(seed: Vec<u8>) -> Result<Arc<DeviceKeyHandle>, KeyFfiError> {
    Ok(Arc::new(DeviceKeyHandle {
        key: DeviceKey::from_seed(seed_array(&seed)?),
    }))
}

#[uniffi::export]
impl DeviceKeyHandle {
    /// The public key, 64 lowercase hex characters.
    #[must_use]
    pub fn public_hex(&self) -> String {
        self.key.public_hex()
    }

    /// The device's fingerprint: the bare 32-hex identity a certificate binds to.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        DeviceId::from_key(&self.key.public()).as_str().to_string()
    }

    /// The fingerprint in the display spelling the CLI prints: `dev_<hex>`.
    #[must_use]
    pub fn display_id(&self) -> String {
        DeviceId::from_key(&self.key.public()).display_id()
    }

    /// Sign a payload. The bytes are the caller's — this is the primitive the
    /// relay handshake and the machine join proof are both built from.
    #[must_use]
    pub fn sign(&self, payload: Vec<u8>) -> Vec<u8> {
        self.key.sign(&payload).to_bytes().to_vec()
    }
}

impl DeviceKeyHandle {
    /// The keypair, for the sibling modules of this crate (the relay dial).
    pub(crate) fn key_ref(&self) -> &DeviceKey {
        &self.key
    }
}

/// A server's root key: it signs device certificates and nothing else.
#[derive(uniffi::Object)]
pub struct RootKeyHandle {
    key: RootKey,
}

/// Build a root key from a caller-supplied 32-byte seed (see
/// [`device_key_from_seed`] for why the seed comes in).
#[uniffi::export]
pub fn root_key_from_seed(seed: Vec<u8>) -> Result<Arc<RootKeyHandle>, KeyFfiError> {
    Ok(Arc::new(RootKeyHandle {
        key: RootKey::from_seed(seed_array(&seed)?),
    }))
}

#[uniffi::export]
impl RootKeyHandle {
    /// The root public key, 64 lowercase hex — the value an operator registers
    /// on the relay and greps for in a log.
    #[must_use]
    pub fn public_hex(&self) -> String {
        self.key.public_hex()
    }

    // **No `sign` here, and that is the decision, not an omission.** The root key is
    // the account's trust anchor, and an exported method that signs arbitrary
    // caller-supplied bytes is a signing oracle for it — a capability the CLI has no
    // verb for. Nothing in this crate or its tests called it (the certificate path
    // uses `inner()` and `pairing_server_begin` takes the key by reference), so it
    // was a speculative export that only widened what a compromised UI could do with
    // the anchor. `DeviceKeyHandle::sign` stays: signing a pairing flight with your
    // *own* device key is the client's own business, and the contract test uses it.
}

impl RootKeyHandle {
    /// The root key, for the sibling modules of this crate (pairing's issuer).
    pub(crate) fn inner(&self) -> &RootKey {
        &self.key
    }
}

/// A signed device certificate.
#[derive(uniffi::Object)]
pub struct DeviceCertHandle {
    cert: DeviceCert,
}

/// Issue a certificate for a device public key.
///
/// **This is encoding, not policy** — the core says so in the same words, and it
/// matters here: which keys may be *pinned* is the admitting machine's decision,
/// and this call signs whatever it is given. It also takes `issued_at_ms` and
/// `serial` from the caller, because the monotonic serial lives in a store and
/// this crate has none: the phone (or the machine that admits it) owns that
/// counter.
///
/// ## The one refusal this door does make, and why it has to
///
/// "Which keys may be pinned is the caller's decision" is true of *policy* and false
/// of *arithmetic*. A small-order ed25519 point is not a key anybody can prove
/// possession of — ed25519-dalek's own documentation says a signature can be forged
/// for almost any message under one — so a certificate for it pins an identity whose
/// signatures mean nothing. The CLI's door (`DeviceAuthority::issue`) refuses it with
/// `AuthorityError::WeakKey`; this surface is the *only* issuing door a mobile UI has,
/// and the phone supplies the key it hands in (`PairingRequest.public_key`), so
/// without this check a malicious phone pushes a weak key through the one path that
/// had none. Same refusal, same sentence (carried from the core, never restated), so
/// the two doors cannot diverge.
///
/// The rest of the core's pinning policy — revocation (`revocation::may_pin`) — is
/// genuinely the caller's: it needs a store, and this crate deliberately has none.
/// `docs/mobile.md` records that split.
#[uniffi::export]
pub fn device_cert_issue(
    root: Arc<RootKeyHandle>,
    device_public_hex: String,
    name: String,
    role: FfiRole,
    issued_at_ms: i64,
    serial: u64,
) -> Result<Arc<DeviceCertHandle>, KeyFfiError> {
    let public_key = verifying_key_from_hex(&device_public_hex)?;
    // The core's own predicate and the core's own sentence: `is_weak` is
    // ed25519-dalek's small-order check (exactly what `check_pinnable` calls), and the
    // message is `AuthorityError::WeakKey`'s `Display`, so this door and the CLI's
    // cannot say different things about the same key. See the doc comment above for
    // why a boundary that is otherwise "encoding, not policy" has to carry this one.
    if public_key.is_weak() {
        return Err(KeyFfiError::WeakKey(
            arreo_core::identity::AuthorityError::WeakKey.to_string(),
        ));
    }
    Ok(Arc::new(DeviceCertHandle {
        cert: DeviceCert::issue(
            &root.key,
            &public_key,
            &name,
            Role::from(role),
            issued_at_ms,
            serial,
        ),
    }))
}

/// Decode untrusted bytes into a structurally valid certificate.
///
/// Decoding establishes nothing: [`DeviceCertHandle::verify`] is the trust
/// decision, and the core keeps the two questions apart for the same reason.
#[uniffi::export]
pub fn device_cert_decode(bytes: Vec<u8>) -> Result<Arc<DeviceCertHandle>, CertFfiError> {
    Ok(Arc::new(DeviceCertHandle {
        cert: DeviceCert::decode(&bytes)?,
    }))
}

#[uniffi::export]
impl DeviceCertHandle {
    /// The device's identity in the display spelling: `dev_<hex>`.
    #[must_use]
    pub fn device_id(&self) -> String {
        self.cert.device().display_id()
    }

    /// The device's fingerprint: the bare 32-hex identity the certificate binds to.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        self.cert.device().as_str().to_string()
    }

    #[must_use]
    pub fn name(&self) -> String {
        self.cert.name().to_string()
    }

    #[must_use]
    pub fn role(&self) -> FfiRole {
        FfiRole::from(self.cert.role())
    }

    #[must_use]
    pub fn serial(&self) -> u64 {
        self.cert.serial()
    }

    /// Encode for storage or transport — what a phone writes to its keystore.
    pub fn encode(&self) -> Result<Vec<u8>, CertFfiError> {
        Ok(self.cert.encode()?)
    }

    /// Full verification against the pinned root key and the presented key.
    ///
    /// Both are hex public keys, and both are required: a certificate that
    /// verifies for a *different* key must never authorize the key that
    /// presented it, which is why `presented` is a parameter and not a lookup.
    pub fn verify(
        &self,
        root_public_hex: String,
        presented_public_hex: String,
    ) -> Result<(), CertFfiError> {
        let root =
            verifying_key_from_hex(&root_public_hex).map_err(|e| CertFfiError::BadPublicKey {
                which: "pinned root".to_string(),
                detail: e.to_string(),
            })?;
        let presented = verifying_key_from_hex(&presented_public_hex).map_err(|e| {
            CertFfiError::BadPublicKey {
                which: "presented".to_string(),
                detail: e.to_string(),
            }
        })?;
        Ok(self.cert.verify(&root, &presented)?)
    }
}

/// The fingerprint of a public key: `sha256(pubkey)` truncated to 128 bits, hex.
///
/// This is what an admitting machine derives from a phone's public key to know
/// which device id to issue for, and what a UI shows when it has a key and no
/// certificate.
#[uniffi::export]
pub fn fingerprint_of_public_key(public_hex: String) -> Result<String, KeyFfiError> {
    let key = verifying_key_from_hex(&public_hex)?;
    Ok(DeviceId::from_key(&key).as_str().to_string())
}

/// Verify an ed25519 signature over a payload.
///
/// A malformed key is a typed refusal rather than `false`: "this key is not a
/// key" and "this signature does not verify" are different answers, and only the
/// second one means the peer is lying. `arreo_core::identity::keys::verify_bytes`
/// is the one implementation of the check itself.
#[uniffi::export]
pub fn identity_verify(
    public_hex: String,
    payload: Vec<u8>,
    signature: Vec<u8>,
) -> Result<bool, KeyFfiError> {
    let key = verifying_key_from_hex(&public_hex)?;
    Ok(arreo_core::identity::keys::verify_bytes(
        &key, &payload, &signature,
    ))
}

/// Parse a 64-hex ed25519 public key with the one parser the workspace has.
fn verifying_key_from_hex(text: &str) -> Result<VerifyingKey, KeyFfiError> {
    Ok(arreo_core::identity::verifying_key_from_hex(text)?)
}

impl DeviceCertHandle {
    /// The certificate, for the sibling modules of this crate (pairing's reply).
    pub(crate) fn cert_ref(&self) -> &DeviceCert {
        &self.cert
    }

    /// Wrap a certificate the sibling modules already hold.
    pub(crate) fn from_core(cert: DeviceCert) -> Self {
        Self { cert }
    }
}

/// The boundary's seed check (see [`crate::errors::bad_seed`]).
fn seed_array(seed: &[u8]) -> Result<[u8; 32], KeyFfiError> {
    <[u8; 32]>::try_from(seed)
        .map_err(|_| KeyFfiError::BadSeed(crate::errors::bad_seed(seed.len())))
}
