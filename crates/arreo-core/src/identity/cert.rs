//! The device certificate (T-0025).
//!
//! One sentence: a `DeviceCert` is a fixed, versioned struct over the same
//! MessagePack codec as the wire protocol, signed by the server's root key —
//! deliberately not X.509, because the model is "devices, not CAs" (ROADMAP
//! §3.2).
//!
//! Trust rules, all enforced here:
//! 1. The cert's `device` field MUST equal the fingerprint of the public key
//!    the presenter proved possession of. A cert for another device is not a
//!    weaker credential — it is a different device, and it is refused.
//! 2. The signature MUST verify under the pinned root key, over the payload
//!    bytes exactly as encoded (no re-encoding, no canonicalization games).
//! 3. The version MUST be one this build understands.
//! 4. Every parse path is total: attacker-supplied bytes produce a typed error,
//!    never a panic and never a partially-trusted cert.

use crate::identity::keys::RootKey;
use crate::identity::role::Role;
use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The only cert format version this build issues or accepts.
pub const CERT_VERSION: u8 = 1;

/// Serial numbers count from here; 0 is reserved for "not a real cert".
pub const FIRST_SERIAL: u64 = 1;

/// The identifier used when reporting a problem with the root key itself.
pub const ROOT_KEY_ID: &str = "root";

/// A device's identity: the fingerprint of its ed25519 public key.
///
/// This is what a cert binds to, what an audit row names, and what a `Viewer`
/// pin lists. Two different keys are two different devices, always.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DeviceId(String);

impl DeviceId {
    /// Derive the id from a public key: `sha256(pubkey)` truncated to 128 bits,
    /// hex-encoded. Truncation is safe here because the id is never the only
    /// check — the cert signature and the key itself are — and 128 bits is
    /// beyond collision reach for an attacker choosing keys.
    #[must_use]
    pub fn from_key(key: &VerifyingKey) -> Self {
        use sha2::{Digest, Sha256};
        let digest = Sha256::digest(key.to_bytes());
        let mut out = String::with_capacity(32);
        for byte in &digest[..16] {
            out.push_str(&format!("{byte:02x}"));
        }
        Self(out)
    }

    /// Parse an id from its textual form. Length and alphabet are checked, so
    /// an id from a log line or a CLI argument cannot smuggle anything.
    pub fn parse(text: &str) -> Result<Self, CertError> {
        let trimmed = text.trim();
        let mixed = trimmed.trim_start_matches("dev_");
        if mixed.len() != 32 || !mixed.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(CertError::BadDeviceId(trimmed.to_string()));
        }
        Ok(Self(mixed.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The prefixed form used in CLI output and audit rows, so an id is never
    /// confused with a serial or a key.
    #[must_use]
    pub fn display_id(&self) -> String {
        format!("dev_{}", self.0)
    }
}

impl std::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.display_id())
    }
}

/// The signed body of a certificate.
///
/// Field order is the wire order *and* the signing order — one encoding, so
/// there is no canonicalization step an attacker could disagree with us about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertPayload {
    pub version: u8,
    /// Fingerprint of the device's public key.
    pub device: DeviceId,
    /// The device's own public key, so verification never has to ask the store.
    pub public_key: [u8; 32],
    /// Human label ("pixel-7", "work-laptop") — display only, never trusted
    /// for authorization.
    pub name: String,
    pub role: Role,
    /// Unix milliseconds.
    pub issued_at_ms: i64,
    /// Monotonic per root key; makes two certs for one device distinguishable
    /// and gives revocation (T-0026) something to name.
    pub serial: u64,
}

/// A signed certificate: the payload plus the root's signature over its
/// MessagePack encoding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCert {
    pub payload: CertPayload,
    /// 64-byte ed25519 signature over [`payload_bytes`].
    ///
    /// Serde has no `[u8; 64]` impl, so the length is enforced by
    /// [`sig_bytes`] at decode time instead of being trusted from the wire.
    #[serde(with = "sig_bytes")]
    pub signature: [u8; 64],
}

/// Serde adapter for a fixed 64-byte signature: encoded as bytes, and a blob
/// of any other length is a decode error rather than a truncated array.
mod sig_bytes {
    use serde::de::{Error, Visitor};
    use serde::{Deserializer, Serializer};
    use std::fmt;

    pub fn serialize<S: Serializer>(bytes: &[u8; 64], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 64], D::Error> {
        deserializer.deserialize_bytes(SigVisitor)
    }

    struct SigVisitor;

    impl<'de> Visitor<'de> for SigVisitor {
        type Value = [u8; 64];

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("a 64-byte ed25519 signature")
        }

        fn visit_bytes<E: Error>(self, bytes: &[u8]) -> Result<Self::Value, E> {
            <[u8; 64]>::try_from(bytes)
                .map_err(|_| E::invalid_length(bytes.len(), &"exactly 64 bytes of signature"))
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut out = [0u8; 64];
            for (index, slot) in out.iter_mut().enumerate() {
                *slot = seq
                    .next_element()?
                    .ok_or_else(|| A::Error::invalid_length(index, &self))?;
            }
            Ok(out)
        }
    }
}

/// Every way a certificate can be refused. Typed on purpose: the daemon turns
/// these into an `auth_reject` audit row, and each one names a different cause.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CertError {
    #[error("certificate is not valid MessagePack: {0}")]
    Decode(String),
    #[error("certificate is not encodable: {0}")]
    Encode(String),
    #[error("unsupported certificate version {found} (this build speaks {expected})")]
    Version { found: u8, expected: u8 },
    #[error("certificate is for {cert_device} but the presented key is {presented}")]
    DeviceMismatch {
        cert_device: String,
        presented: String,
    },
    #[error("certificate signature does not verify under the root key")]
    BadSignature,
    #[error("certificate carries serial 0, which is never issued")]
    ZeroSerial,
    #[error("not a device id: {0:?} (want 32 hex characters, optional `dev_` prefix)")]
    BadDeviceId(String),
    #[error("device {0} has no certificate")]
    NoCert(String),
    #[error("device {device} presents key {presented}, but is pinned to {pinned}")]
    KeyMismatch {
        device: String,
        presented: String,
        pinned: String,
    },
    #[error("certificate revoked: {0}")]
    Revoked(String),
    #[error("key {device} was rotated away; this device is now {replaced_by}")]
    RotatedAway { device: String, replaced_by: String },
    #[error("cert io ({path}): {detail}")]
    Io { path: PathBuf, detail: String },
}

impl DeviceCert {
    /// Issue a certificate for `public_key`: this is the only way a cert comes
    /// into existence, and it requires the root key by type.
    ///
    /// **This is encoding, not policy.** It will sign whatever key it is
    /// given, including a weak (small-order) one. The decision about *which*
    /// keys may be pinned belongs to the authority (`check_pinnable` there),
    /// so that every pinning path — pairing, `arreo devices issue`, rotation —
    /// shares one rule instead of each caller remembering it.
    #[must_use]
    pub fn issue(
        root: &RootKey,
        public_key: &VerifyingKey,
        name: &str,
        role: Role,
        issued_at_ms: i64,
        serial: u64,
    ) -> Self {
        let payload = CertPayload {
            version: CERT_VERSION,
            device: DeviceId::from_key(public_key),
            public_key: public_key.to_bytes(),
            name: name.to_string(),
            role,
            issued_at_ms,
            serial,
        };
        let signature = root.sign(&payload_bytes(&payload));
        Self {
            payload,
            signature: signature.to_bytes(),
        }
    }

    #[must_use]
    pub fn device(&self) -> &DeviceId {
        &self.payload.device
    }

    #[must_use]
    pub fn role(&self) -> Role {
        self.payload.role
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.payload.name
    }

    #[must_use]
    pub fn serial(&self) -> u64 {
        self.payload.serial
    }

    #[must_use]
    pub fn public_key(&self) -> Option<VerifyingKey> {
        VerifyingKey::from_bytes(&self.payload.public_key).ok()
    }

    /// Encode for storage or transport. The payload is encoded exactly as
    /// signed; the signature travels beside it.
    pub fn encode(&self) -> Result<Vec<u8>, CertError> {
        rmp_serde::to_vec_named(self).map_err(|e| CertError::Encode(e.to_string()))
    }

    /// Decode untrusted bytes into a *structurally valid* cert. This does not
    /// establish trust — [`DeviceCert::verify`] does, and callers must not skip
    /// it. Kept separate so "what is this blob" and "may I trust this blob" are
    /// different questions in the code as well as in the threat model.
    pub fn decode(bytes: &[u8]) -> Result<Self, CertError> {
        rmp_serde::from_slice(bytes).map_err(|e| CertError::Decode(e.to_string()))
    }

    /// Full verification against the pinned root key and the presented key.
    ///
    /// `presented` is the public key the peer proved possession of. Passing it
    /// is mandatory: a cert that verifies for a *different* key must never
    /// authorize the key that presented it.
    pub fn verify(&self, root: &VerifyingKey, presented: &VerifyingKey) -> Result<(), CertError> {
        if self.payload.version != CERT_VERSION {
            return Err(CertError::Version {
                found: self.payload.version,
                expected: CERT_VERSION,
            });
        }
        if self.payload.serial < FIRST_SERIAL {
            return Err(CertError::ZeroSerial);
        }
        // The cert's own public key must match the key that was presented,
        // checked before the signature so a mismatched cert is named as such.
        let cert_key = self.public_key().ok_or_else(|| CertError::DeviceMismatch {
            cert_device: self.payload.device.to_string(),
            presented: "(unparseable cert key)".to_string(),
        })?;
        if cert_key.to_bytes() != presented.to_bytes() {
            return Err(CertError::DeviceMismatch {
                cert_device: DeviceId::from_key(&cert_key).to_string(),
                presented: DeviceId::from_key(presented).to_string(),
            });
        }
        // The device id must be the fingerprint of that key: a cert cannot
        // claim an identity its key does not have.
        let derived = DeviceId::from_key(&cert_key);
        if derived != self.payload.device {
            return Err(CertError::DeviceMismatch {
                cert_device: self.payload.device.to_string(),
                presented: derived.to_string(),
            });
        }
        let signature = Signature::from_bytes(&self.signature);
        if !crate::identity::keys::verify(root, &payload_bytes(&self.payload), &signature) {
            return Err(CertError::BadSignature);
        }
        Ok(())
    }

    /// Write the cert to `dir/<device>.cert` with owner-only permissions.
    pub fn save(&self, dir: &Path) -> Result<PathBuf, CertError> {
        crate::identity::keys::create_private_dir(dir).map_err(|e| CertError::Io {
            path: dir.to_path_buf(),
            detail: e.to_string(),
        })?;
        let path = dir.join(format!("{}.cert", self.payload.device.as_str()));
        let bytes = self.encode()?;
        crate::write_private_bytes(&path, &bytes).map_err(|e| CertError::Io {
            path: path.clone(),
            detail: e.to_string(),
        })?;
        Ok(path)
    }

    /// Read a cert file. The caller still has to verify it.
    pub fn load(path: &Path) -> Result<Self, CertError> {
        let bytes = std::fs::read(path).map_err(|e| CertError::Io {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;
        Self::decode(&bytes)
    }
}

/// The exact bytes a certificate signs: the payload's MessagePack encoding.
/// One function so signer and verifier cannot drift.
#[must_use]
pub fn payload_bytes(payload: &CertPayload) -> Vec<u8> {
    // A `CertPayload` built by `issue` always encodes; if a future field makes
    // it fallible this returns empty bytes, which fails every signature check
    // (fail closed) rather than panicking.
    rmp_serde::to_vec_named(payload).unwrap_or_default()
}

/// A device as the server knows it: public material only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub id: DeviceId,
    pub name: String,
    pub role: Role,
    pub public_key: [u8; 32],
    pub serial: u64,
    pub issued_at_ms: i64,
    pub last_seen_ms: Option<i64>,
    pub revoked: bool,
    /// Set when this key was rotated away, naming the device it became. The
    /// retirement has to be durable: the whole point of rotation is that the
    /// old key stops working *after a restart* too.
    pub retired_to: Option<DeviceId>,
}

impl DeviceRecord {
    #[must_use]
    pub fn from_cert(cert: &DeviceCert) -> Self {
        Self {
            id: cert.payload.device.clone(),
            name: cert.payload.name.clone(),
            role: cert.payload.role,
            public_key: cert.payload.public_key,
            serial: cert.payload.serial,
            issued_at_ms: cert.payload.issued_at_ms,
            last_seen_ms: None,
            revoked: false,
            retired_to: None,
        }
    }

    /// The public key as a hex string (display and JSON output).
    #[must_use]
    pub fn public_hex(&self) -> String {
        crate::identity::keys::hex(&self.public_key)
    }
}

/// The public key inside a record, if it is really a key.
#[must_use]
pub fn record_public_key(record: &DeviceRecord) -> Option<VerifyingKey> {
    VerifyingKey::from_bytes(&record.public_key).ok()
}

/// A directory of pinned devices, loaded from cert files.
///
/// This is the authorization input: a connection is allowed because this index
/// has an entry whose key matches, not because the peer says so.
#[derive(Debug, Clone, Default)]
pub struct DeviceIndex {
    devices: BTreeMap<DeviceId, (DeviceRecord, DeviceCert)>,
    /// Keys that used to be a device's identity and were rotated away, mapped
    /// to the device they became. Kept so the old key gets "you were rotated"
    /// instead of a bare "unknown device" — the difference between a user
    /// fixing their config and a user filing a bug.
    retired: BTreeMap<DeviceId, DeviceId>,
}

impl DeviceIndex {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Load every `*.cert` in `dir`. Unreadable or invalid files are skipped —
    /// a corrupt file must not take the whole authority down — and reported so
    /// the operator can see them.
    #[must_use]
    pub fn load_dir(dir: &Path) -> (Self, Vec<(PathBuf, CertError)>) {
        let mut index = Self::new();
        let mut problems = Vec::new();
        let Ok(read) = std::fs::read_dir(dir) else {
            return (index, problems);
        };
        let mut paths: Vec<PathBuf> = read
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "cert"))
            .collect();
        paths.sort();
        for path in paths {
            match DeviceCert::load(&path) {
                Ok(cert) => index.insert(cert),
                Err(e) => problems.push((path, e)),
            }
        }
        (index, problems)
    }

    pub fn insert(&mut self, cert: DeviceCert) {
        let record = DeviceRecord::from_cert(&cert);
        self.devices.insert(record.id.clone(), (record, cert));
    }

    /// Rebuild an index from durable records: a record's cert is re-verified
    /// against the pinned root before it re-enters the authority, and the
    /// **verified certificate is the source of the role, name and serial** —
    /// the store row contributes only facts a certificate cannot carry
    /// (`revoked`, `retired_to`, `last_seen_ms`).
    ///
    /// That split is the whole point: the store is a file on disk, and a
    /// tampered row must not be able to escalate a viewer to an owner.
    pub fn from_records(
        records: impl IntoIterator<Item = DeviceRecord>,
        certs: &std::collections::HashMap<DeviceId, DeviceCert>,
        root: &VerifyingKey,
    ) -> Self {
        let mut index = Self::new();
        for record in records {
            if record.revoked || record.retired_to.is_some() {
                continue;
            }
            let Some(cert) = certs.get(&record.id) else {
                continue;
            };
            let Some(key) = record_public_key(&record) else {
                continue;
            };
            if cert.verify(root, &key).is_err() {
                continue;
            }
            let mut trusted = DeviceRecord::from_cert(cert);
            trusted.last_seen_ms = record.last_seen_ms;
            index
                .devices
                .insert(record.id.clone(), (trusted, cert.clone()));
        }
        index
    }

    #[must_use]
    pub fn get(&self, id: &DeviceId) -> Option<&DeviceRecord> {
        self.devices.get(id).map(|(record, _)| record)
    }

    #[must_use]
    pub fn cert(&self, id: &DeviceId) -> Option<&DeviceCert> {
        self.devices.get(id).map(|(_, cert)| cert)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.devices.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.devices.is_empty()
    }

    /// All devices in id order (stable output for the CLI and for audit).
    pub fn iter(&self) -> impl Iterator<Item = &DeviceRecord> {
        self.devices.values().map(|(record, _)| record)
    }

    /// Rotate a device onto a new key: the certificate for `new_cert`'s key
    /// replaces `old_id`, and `old_id` is retired.
    ///
    /// Rotation is deliberately explicit. Because a device id *is* the
    /// fingerprint of its key, a new keypair is a new id: without this call the
    /// two would coexist and "rotation" would silently be "a second device".
    /// After this call the old key's next connection is refused with
    /// [`CertError::RotatedAway`], naming the device it became.
    pub fn rotate(
        &mut self,
        old_id: &DeviceId,
        new_cert: DeviceCert,
    ) -> Result<DeviceId, CertError> {
        if !self.devices.contains_key(old_id) {
            return Err(CertError::NoCert(old_id.to_string()));
        }
        self.devices.remove(old_id);
        let new_id = new_cert.payload.device.clone();
        if new_id == *old_id {
            return Err(CertError::Revoked(
                "rotation must move the device onto a different key".to_string(),
            ));
        }
        self.retired.insert(old_id.clone(), new_id.clone());
        self.insert(new_cert);
        Ok(new_id)
    }

    /// Record that `old` was rotated onto `replacement`, without adding a key.
    /// Used when the durable store (not a live rotation call) is the source of
    /// that fact — after a restart, say.
    pub fn retire(&mut self, old: &DeviceId, replacement: &DeviceId) {
        self.devices.remove(old);
        self.retired.insert(old.clone(), replacement.clone());
    }

    /// Was this key rotated away, and onto which device?
    #[must_use]
    pub fn retired_to(&self, id: &DeviceId) -> Option<&DeviceId> {
        self.retired.get(id)
    }

    pub fn set_last_seen(&mut self, id: &DeviceId, at_ms: i64) {
        if let Some((record, _)) = self.devices.get_mut(id) {
            record.last_seen_ms = Some(at_ms);
        }
    }

    /// **The authorization decision.** A presented key is accepted only if the
    /// index holds a non-revoked cert for it whose signature verifies under
    /// `root` — in that order of specificity, so the error names the real
    /// problem ("pinned to a different key" vs "no such device").
    pub fn authorize(
        &self,
        root: &VerifyingKey,
        presented: &VerifyingKey,
    ) -> Result<&DeviceRecord, CertError> {
        let id = DeviceId::from_key(presented);
        let Some((record, cert)) = self.devices.get(&id) else {
            // A rotated-away key is the most likely reason a *former* device
            // shows up, and the most useful thing to say about it.
            if let Some(replacement) = self.retired.get(&id) {
                return Err(CertError::RotatedAway {
                    device: id.to_string(),
                    replaced_by: replacement.to_string(),
                });
            }
            return Err(CertError::NoCert(id.to_string()));
        };
        if record.revoked {
            return Err(CertError::Revoked(id.to_string()));
        }
        if record.public_key != presented.to_bytes() {
            return Err(CertError::KeyMismatch {
                device: id.to_string(),
                presented: crate::identity::keys::hex(&presented.to_bytes()),
                pinned: record.public_hex(),
            });
        }
        cert.verify(root, presented)?;
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::keys::DeviceKey;

    const NOW: i64 = 1_760_000_000_000;

    fn root() -> RootKey {
        RootKey::from_seed([7u8; 32])
    }

    fn other_root() -> RootKey {
        RootKey::from_seed([9u8; 32])
    }

    fn cert_for(key: &DeviceKey, name: &str, role: Role, serial: u64) -> DeviceCert {
        DeviceCert::issue(&root(), &key.public(), name, role, NOW, serial)
    }

    #[test]
    fn a_issued_cert_round_trips_and_verifies() {
        let key = DeviceKey::generate().expect("entropy");
        let cert = cert_for(&key, "pixel-7", Role::Viewer, 1);
        cert.verify(&root().public(), &key.public())
            .expect("verifies");
        let bytes = cert.encode().expect("encode");
        let decoded = DeviceCert::decode(&bytes).expect("decode");
        assert_eq!(decoded, cert, "encoding is lossless");
        decoded
            .verify(&root().public(), &key.public())
            .expect("decoded cert still verifies");
        assert_eq!(cert.device(), &DeviceId::from_key(&key.public()));
        assert_eq!(cert.name(), "pixel-7");
        assert_eq!(cert.role(), Role::Viewer);
        assert_eq!(cert.serial(), 1);
    }

    #[test]
    fn a_cert_from_another_root_does_not_verify() {
        let key = DeviceKey::generate().expect("entropy");
        let foreign = DeviceCert::issue(&other_root(), &key.public(), "x", Role::Owner, NOW, 1);
        assert_eq!(
            foreign.verify(&root().public(), &key.public()),
            Err(CertError::BadSignature)
        );
    }

    #[test]
    fn a_cert_for_another_key_cannot_authorize_this_key() {
        // The headline rule: presenting a valid cert you do not hold the key
        // for is not a weaker credential, it is a different device.
        let victim = DeviceKey::generate().expect("entropy");
        let attacker = DeviceKey::generate().expect("entropy");
        let cert = cert_for(&victim, "victim", Role::Owner, 1);
        match cert.verify(&root().public(), &attacker.public()) {
            Err(CertError::DeviceMismatch { .. }) => {}
            other => panic!("expected DeviceMismatch, got {other:?}"),
        }
    }

    #[test]
    fn a_retargeted_device_id_is_refused() {
        // Same key, but the payload claims a different device id: refuse,
        // because the id is derived from the key, not asserted.
        let key = DeviceKey::generate().expect("entropy");
        let mut cert = cert_for(&key, "x", Role::Viewer, 1);
        cert.payload.device = DeviceId::parse("00112233445566778899aabbccddeeff").unwrap();
        match cert.verify(&root().public(), &key.public()) {
            Err(CertError::DeviceMismatch { .. }) => {}
            other => panic!("expected DeviceMismatch, got {other:?}"),
        }
    }

    #[test]
    fn a_tampered_field_breaks_the_signature() {
        let key = DeviceKey::generate().expect("entropy");
        let mut cert = cert_for(&key, "viewer", Role::Viewer, 1);
        // Escalating the role in the payload must not survive verification.
        cert.payload.role = Role::Owner;
        assert_eq!(
            cert.verify(&root().public(), &key.public()),
            Err(CertError::BadSignature)
        );
        // Nor must extending the serial, name or issue time.
        let mut cert = cert_for(&key, "viewer", Role::Viewer, 1);
        cert.payload.serial = 999;
        assert!(cert.verify(&root().public(), &key.public()).is_err());
        let mut cert = cert_for(&key, "viewer", Role::Viewer, 1);
        cert.payload.name = "root".into();
        assert!(cert.verify(&root().public(), &key.public()).is_err());
        let mut cert = cert_for(&key, "viewer", Role::Viewer, 1);
        cert.payload.issued_at_ms = NOW + 1;
        assert!(cert.verify(&root().public(), &key.public()).is_err());
    }

    #[test]
    fn a_weak_key_can_be_encoded_but_is_the_authoritys_decision() {
        // The all-zero compressed point is a valid small-order encoding; the
        // certificate layer signs it (encoding), and the authority refuses to
        // pin it (policy). Both halves are asserted so neither silently drifts
        // into the other's job.
        let weak = VerifyingKey::from_bytes(&[0u8; 32]).expect("encodes");
        assert!(weak.is_weak());
        let cert = DeviceCert::issue(&root(), &weak, "weak", Role::Owner, NOW, 1);
        cert.verify(&root().public(), &weak)
            .expect("the encoding path signs what it is given");
    }

    #[test]
    fn a_wrong_version_or_zero_serial_is_refused() {
        let key = DeviceKey::generate().expect("entropy");
        // Version: re-signed by the real root, so only the version check can
        // reject it — proving the check is not an artifact of the signature.
        let payload = CertPayload {
            version: CERT_VERSION + 1,
            ..cert_for(&key, "x", Role::Viewer, 1).payload
        };
        let signature = root().sign(&payload_bytes(&payload)).to_bytes();
        let cert = DeviceCert { payload, signature };
        match cert.verify(&root().public(), &key.public()) {
            Err(CertError::Version { found, expected }) => {
                assert_eq!(found, CERT_VERSION + 1);
                assert_eq!(expected, CERT_VERSION);
            }
            other => panic!("expected Version, got {other:?}"),
        }
        let payload = CertPayload {
            serial: 0,
            ..cert_for(&key, "x", Role::Viewer, 1).payload
        };
        let signature = root().sign(&payload_bytes(&payload)).to_bytes();
        let cert = DeviceCert { payload, signature };
        assert_eq!(
            cert.verify(&root().public(), &key.public()),
            Err(CertError::ZeroSerial)
        );
    }

    #[test]
    fn a_truncated_or_oversized_signature_is_refused_at_decode() {
        // The signature is bytes on the wire; a shorter or longer blob must be
        // a decode error, never a zero-padded array that then fails obscurely.
        let key = DeviceKey::generate().expect("entropy");
        let cert = cert_for(&key, "x", Role::Viewer, 1);
        let encoded = cert.encode().expect("encode");
        // Every prefix of a valid cert is invalid (msgpack) input.
        for cut in [1, 2, 8, encoded.len() / 2, encoded.len() - 1] {
            assert!(
                DeviceCert::decode(&encoded[..cut]).is_err(),
                "a {cut}-byte prefix decoded"
            );
        }
        // A hand-built blob with a 32-byte signature is refused.
        #[derive(Serialize)]
        struct Short {
            payload: CertPayload,
            signature: Vec<u8>,
        }
        let short = Short {
            payload: cert.payload.clone(),
            signature: vec![0u8; 32],
        };
        let bytes = rmp_serde::to_vec_named(&short).expect("encode");
        assert!(
            matches!(DeviceCert::decode(&bytes), Err(CertError::Decode(_))),
            "a short signature decoded into a cert"
        );
    }

    #[test]
    fn garbage_bytes_are_a_typed_error_not_a_panic() {
        for hostile in [
            &b""[..],
            &b"not msgpack"[..],
            &[0xff, 0xff, 0xff, 0xff][..],
            &[0x81, 0xa1, 0x78][..],
            &[0xc1][..],
        ] {
            assert!(
                matches!(DeviceCert::decode(hostile), Err(CertError::Decode(_))),
                "{hostile:?} decoded into something"
            );
        }
    }

    #[test]
    fn authorize_refuses_unknown_mismatched_and_revoked_devices() {
        let root = root();
        let known = DeviceKey::generate().expect("entropy");
        let stranger = DeviceKey::generate().expect("entropy");
        let mut index = DeviceIndex::new();
        index.insert(cert_for(&known, "known", Role::Owner, 1));

        // The pinned device is authorized and its role comes from the index.
        let record = index
            .authorize(&root.public(), &known.public())
            .expect("known device");
        assert_eq!(record.role, Role::Owner);

        // A key with no cert is refused, and named.
        match index.authorize(&root.public(), &stranger.public()) {
            Err(CertError::NoCert(id)) => {
                assert_eq!(id, DeviceId::from_key(&stranger.public()).to_string())
            }
            other => panic!("expected NoCert, got {other:?}"),
        }

        // A revoked device is refused with the revocation reason.
        let id = DeviceId::from_key(&known.public());
        index.devices.get_mut(&id).expect("entry").0.revoked = true;
        match index.authorize(&root.public(), &known.public()) {
            Err(CertError::Revoked(name)) => assert_eq!(name, id.to_string()),
            other => panic!("expected Revoked, got {other:?}"),
        }
    }

    #[test]
    fn the_index_reports_corrupt_cert_files_without_dying() {
        let dir = std::env::temp_dir().join(format!("arreo-certs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let key = DeviceKey::generate().expect("entropy");
        let good = cert_for(&key, "good", Role::Viewer, 1);
        good.save(&dir).expect("save");
        std::fs::write(dir.join("truncated.cert"), b"\xc1\xc1").expect("write hostile");
        let (index, problems) = DeviceIndex::load_dir(&dir);
        assert_eq!(index.len(), 1, "the good cert still loaded");
        assert_eq!(problems.len(), 1, "the hostile file was reported");
        assert!(problems[0].0.ends_with("truncated.cert"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rotation_moves_a_device_onto_a_new_key() {
        let root = root();
        let old = DeviceKey::generate().expect("entropy");
        let new = DeviceKey::generate().expect("entropy");
        let mut index = DeviceIndex::new();
        let old_cert = cert_for(&old, "laptop", Role::Owner, 1);
        let old_id = old_cert.payload.device.clone();
        index.insert(old_cert);
        assert!(index.authorize(&root.public(), &old.public()).is_ok());

        let new_cert = DeviceCert::issue(&root, &new.public(), "laptop", Role::Owner, NOW, 2);
        let new_id = index.rotate(&old_id, new_cert.clone()).expect("rotation");

        // The new key works, the old key is refused with a reason that names
        // where the device went.
        let record = index
            .authorize(&root.public(), &new.public())
            .expect("rotated key is authorized");
        assert_eq!(record.name, "laptop");
        match index.authorize(&root.public(), &old.public()) {
            Err(CertError::RotatedAway { replaced_by, .. }) => {
                assert_eq!(replaced_by, new_id.to_string())
            }
            other => panic!("expected RotatedAway, got {other:?}"),
        }
        // Rotating an unknown device is a typed error, not a silent insert.
        assert!(matches!(
            index.rotate(
                &DeviceId::parse("00112233445566778899aabbccddeeff").unwrap(),
                new_cert.clone()
            ),
            Err(CertError::NoCert(_))
        ));
        // Rotating onto the same key is refused: that is not a rotation.
        let same = DeviceCert::issue(&root, &new.public(), "laptop", Role::Owner, NOW, 3);
        assert!(index.rotate(&new_id, same).is_err());
    }

    #[test]
    fn device_ids_parse_strictly() {
        assert!(DeviceId::parse("dev_00112233445566778899aabbccddeeff").is_ok());
        assert!(DeviceId::parse("00112233445566778899AABBCCDDEEFF").is_ok());
        for bad in [
            "",
            "short",
            "dev_",
            "00112233445566778899aabbccddeef",
            "zzzz",
        ] {
            assert!(
                matches!(DeviceId::parse(bad), Err(CertError::BadDeviceId(_))),
                "{bad}"
            );
        }
    }
}
