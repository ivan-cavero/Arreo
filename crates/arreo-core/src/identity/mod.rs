//! Device identity (T-0025).
//!
//! One sentence: a client is a *device* — an ed25519 keypair plus a
//! server-signed certificate that pins it — so authorizing means "this exact
//! device", never "whoever holds a copy of a config" (ROADMAP §3.3, §4).
//!
//! Layout:
//! - [`keys`] — key material, on-disk encoding, zeroization.
//! - [`cert`] — the `DeviceCert` structure, signing and total verification.
//! - [`role`] — the v1 capability model (owner / viewer) and the verb policy.
//!
//! Design rules this module keeps:
//! - **Devices, not a CA.** No X.509, no chain building, no path validation:
//!   one root key signs a flat list of device certs, and verification is a
//!   signature check plus a field check. Every parse path is total (no panic
//!   on attacker-controlled bytes) and covered by proptest.
//! - **Private keys never reach the database.** The store keeps public keys and
//!   certs; key files live in a 0700 directory with 0600 files.
//! - **The certificate is the pin.** A device's identity *is* the fingerprint
//!   of its public key, so a cert that verifies for a different key is not a
//!   weaker credential — it is a different device.

#[cfg(feature = "sqlite")]
pub mod authority;
#[cfg(feature = "sqlite")]
pub use authority::{sidecar_db, AuthorityError, DeviceAuthority, Layout, VerbDenial};
pub mod cert;
pub mod keys;
pub mod revocation;
pub mod role;

pub use cert::{
    record_public_key, CertError, CertPayload, DeviceCert, DeviceId, DeviceIndex, DeviceRecord,
    ROOT_KEY_ID,
};
pub use keys::{
    create_private_dir, identity_dir, identity_root, verifying_key_from_hex, DeviceKey, KeyError,
    RootKey, IDENTITY_DIR_ENV,
};
pub use revocation::{authorized_role, may_connect, may_pin, Denied};
pub use role::{Capability, Role, RoleError};

/// The ed25519 public key type, re-exported so the server and CLI depend on
/// `arreo-core` for key handling instead of pulling the crypto crate in
/// themselves (one place to audit, one place to upgrade).
pub use ed25519_dalek::VerifyingKey;
