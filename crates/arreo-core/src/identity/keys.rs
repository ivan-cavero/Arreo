//! Key material: ed25519 keypairs, on-disk encoding, zeroization (T-0025).
//!
//! One sentence: a keypair is generated from OS entropy, written to a
//! permission-restricted file, and its secret half is wiped from memory when
//! the value is dropped.
//!
//! What lives where (acceptance criterion 5):
//! - server root key: `$XDG_DATA_HOME/arreo/identity/root.key` (0600, dir 0700)
//! - server-side device certs: `$XDG_DATA_HOME/arreo/identity/devices/<id>.cert`
//! - client keypair: the client's own identity dir, same layout
//! - SQLite: public keys and certs only — never a secret.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use std::path::{Path, PathBuf};
use zeroize::Zeroize;

/// Overrides the identity directory (tests, and users who keep state
/// elsewhere). Relative paths are used as-is.
pub const IDENTITY_DIR_ENV: &str = "ARREO_IDENTITY_DIR";

/// Key material failures. Every variant is a typed refusal — nothing here
/// panics on hostile input.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("no OS entropy available: {0}")]
    Entropy(String),
    #[error("identity io ({path}): {detail}")]
    Io { path: PathBuf, detail: String },
    #[error("key file {path} is malformed (want 32 bytes of lowercase hex, got {got} bytes)")]
    Malformed { path: PathBuf, got: usize },
    #[error("key file {path} has permissions {mode:o}; {required:o} is required")]
    Permissions {
        path: PathBuf,
        mode: u32,
        required: u32,
    },
    #[error("no identity at {path}")]
    Missing { path: PathBuf },
}

/// The permission bits every key file and its directory must carry (Unix).
#[cfg(unix)]
const FILE_MODE: u32 = 0o600;
#[cfg(unix)]
const DIR_MODE: u32 = 0o700;

/// A device keypair: the public half is the device's identity material, the
/// secret half never leaves this process except to its own key file.
pub struct DeviceKey {
    signing: SigningKey,
    verifying: VerifyingKey,
}

impl std::fmt::Debug for DeviceKey {
    /// Never print key material — a debug log is not a key store.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKey")
            .field("public", &hex(&self.verifying.to_bytes()))
            .finish_non_exhaustive()
    }
}

impl DeviceKey {
    /// Generate a fresh keypair from OS entropy.
    pub fn generate() -> Result<Self, KeyError> {
        let seed = random_seed()?;
        Ok(Self::from_seed(seed))
    }

    /// Deterministic construction from a 32-byte seed (tests, and loading a
    /// key file). The seed is moved in and zeroized by the caller's buffer.
    #[must_use]
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let signing = SigningKey::from_bytes(&seed);
        let verifying = signing.verifying_key();
        Self { signing, verifying }
    }

    #[must_use]
    pub fn public(&self) -> VerifyingKey {
        self.verifying
    }

    #[must_use]
    pub fn public_hex(&self) -> String {
        hex(&self.verifying.to_bytes())
    }

    /// Sign a payload. Signatures are over the exact bytes the caller passes —
    /// [`crate::identity::cert`] defines what those bytes are.
    #[must_use]
    pub fn sign(&self, payload: &[u8]) -> Signature {
        self.signing.sign(payload)
    }

    /// Load the key at `path`, or generate + persist one if it is absent.
    /// The file is created with 0600 inside a 0700 directory; an existing file
    /// with looser permissions is refused rather than silently trusted.
    pub fn load_or_generate(path: &Path) -> Result<Self, KeyError> {
        match Self::load(path) {
            Ok(key) => Ok(key),
            Err(KeyError::Missing { .. }) => {
                let key = Self::generate()?;
                key.save(path)?;
                Ok(key)
            }
            Err(other) => Err(other),
        }
    }

    /// Read a key file. The file is 64 hex characters (32 bytes).
    pub fn load(path: &Path) -> Result<Self, KeyError> {
        if !path.exists() {
            return Err(KeyError::Missing {
                path: path.to_path_buf(),
            });
        }
        check_permissions(path)?;
        let text = std::fs::read_to_string(path).map_err(|e| KeyError::Io {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?;
        let mut bytes = decode_hex(text.trim(), path)?;
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        bytes.zeroize();
        let key = Self::from_seed(seed);
        seed.zeroize();
        Ok(key)
    }

    /// Write this key to `path` with owner-only permissions.
    pub fn save(&self, path: &Path) -> Result<(), KeyError> {
        if let Some(parent) = path.parent() {
            create_private_dir(parent)?;
        }
        let secret = self.signing.to_bytes();
        let text = hex(&secret);
        let result = write_private_file(path, text.as_bytes());
        // The local copies of the secret die here regardless of the io result.
        let mut secret = secret;
        let mut text = text;
        secret.zeroize();
        text.zeroize();
        result
    }
}

/// The server's root key: it signs device certificates and nothing else.
///
/// Keeping this a separate type means a device key can never be mistaken for
/// the authority that issues device keys — the compiler enforces the
/// distinction that the threat model cares about.
pub struct RootKey {
    inner: DeviceKey,
}

impl std::fmt::Debug for RootKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootKey")
            .field("public", &self.inner.public_hex())
            .finish_non_exhaustive()
    }
}

impl RootKey {
    pub fn generate() -> Result<Self, KeyError> {
        Ok(Self {
            inner: DeviceKey::generate()?,
        })
    }

    #[must_use]
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self {
            inner: DeviceKey::from_seed(seed),
        }
    }

    #[must_use]
    pub fn public_hex(&self) -> String {
        self.inner.public_hex()
    }

    #[must_use]
    pub fn public(&self) -> VerifyingKey {
        self.inner.public()
    }

    /// Sign a certificate payload (the root's only signing operation).
    #[must_use]
    pub fn sign(&self, payload: &[u8]) -> Signature {
        self.inner.sign(payload)
    }

    pub fn load_or_generate(path: &Path) -> Result<Self, KeyError> {
        Ok(Self {
            inner: DeviceKey::load_or_generate(path)?,
        })
    }

    pub fn save(&self, path: &Path) -> Result<(), KeyError> {
        self.inner.save(path)
    }
}

/// The X25519 view of an identity key: the same secret, used for the Noise
/// handshake (T-0023).
///
/// **Why one key does both.** ROADMAP §3.3 says it plainly — "phone generates
/// an ed25519 device keypair; server pins it; every later connection is
/// Noise-KK mutual auth" — and there is exactly one pin per device. A second,
/// separate transport key would mean a second thing to pair, pin, rotate and
/// revoke, and every one of those is a place for the two to disagree (a rotated
/// identity with a stale transport key is a device that authenticates as
/// somebody it is not). So the Noise static is *derived*:
///
/// - public: `ed25519_pk.to_montgomery()` — the birational map, a library call
/// - secret: the ed25519 expanded scalar (`SigningKey::to_scalar_bytes`), which
///   X25519 clamps identically (both clear the low three bits, clear the top
///   bit and set the second-highest), so scalar × B_montgomery is the same
///   point as `to_montgomery()`. `identity::keys` tests assert that equality
///   rather than assuming it.
///
/// This is the conversion libsodium ships (`crypto_sign_ed25519_*_to_curve25519`).
/// `ed25519-dalek` documents that reusing a signing key for Diffie-Hellman is
/// not recommended in general, and that is the right default — the exception
/// here is that a *device* key signs nothing in this product (its Ed25519 half
/// only identities it; the root key does the signing), so there is no
/// signing oracle to combine with the DH half. The root key does sign, which is
/// recorded as a known trade-off in ADR 0011; if device keys ever gain a signing
/// role, the move is a signed prekey (bind a separate X25519 key with an Ed25519
/// signature) rather than this derivation.
#[cfg(feature = "transport")]
pub struct NoiseStatic {
    secret: [u8; 32],
    public: [u8; 32],
}

#[cfg(feature = "transport")]
impl std::fmt::Debug for NoiseStatic {
    /// Never print the secret half.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NoiseStatic")
            .field("public", &crate::identity::keys::hex(&self.public))
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "transport")]
impl NoiseStatic {
    /// The public half, as the peer needs it.
    #[must_use]
    pub fn public(&self) -> [u8; 32] {
        self.public
    }

    /// The secret half. Crate-visible: only the transport's handshake may hold
    /// it, and only for the length of a handshake.
    #[must_use]
    pub(crate) fn secret(&self) -> [u8; 32] {
        self.secret
    }
}

#[cfg(feature = "transport")]
impl Zeroize for NoiseStatic {
    fn zeroize(&mut self) {
        self.secret.zeroize();
    }
}

#[cfg(feature = "transport")]
impl Drop for NoiseStatic {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

/// The X25519 public key of an identity (the peer side of [`NoiseStatic`]).
#[must_use]
#[cfg(feature = "transport")]
pub fn noise_public_key(identity: &VerifyingKey) -> [u8; 32] {
    identity.to_montgomery().to_bytes()
}

/// Derive the Noise static from an ed25519 signing key: the expanded scalar for
/// the secret, the Montgomery image of the public key for the public half.
#[cfg(feature = "transport")]
fn noise_static_from(signing: &SigningKey) -> NoiseStatic {
    let mut secret = signing.to_scalar_bytes();
    // Clamp here as well as in X25519, so `secret` and `public` are consistent
    // *as stored* (a caller that reads `secret()` must not have to know that
    // X25519 will clamp later).
    secret[0] &= 248;
    secret[31] &= 127;
    secret[31] |= 64;
    NoiseStatic {
        secret,
        public: noise_public_key(&signing.verifying_key()),
    }
}

impl DeviceKey {
    #[cfg(feature = "transport")]
    /// The Noise static this device authenticates with (see [`NoiseStatic`]).
    #[must_use]
    pub fn noise_static(&self) -> NoiseStatic {
        noise_static_from(&self.signing)
    }
}

impl RootKey {
    #[cfg(feature = "transport")]
    /// The Noise static the *server* authenticates with.
    #[must_use]
    pub fn noise_static(&self) -> NoiseStatic {
        static_secret_of_root(&self.inner)
    }
}

/// `RootKey` wraps a `DeviceKey`; this keeps the derivation in one place.
#[cfg(feature = "transport")]
fn static_secret_of_root(inner: &DeviceKey) -> NoiseStatic {
    noise_static_from(&inner.signing)
}

/// Verify a signature without owning the key (the server does this for device
/// signatures, clients for certs).
#[must_use]
pub fn verify(public: &VerifyingKey, payload: &[u8], signature: &Signature) -> bool {
    public.verify(payload, signature).is_ok()
}

/// 32 bytes of OS entropy.
fn random_seed() -> Result<[u8; 32], KeyError> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| KeyError::Entropy(e.to_string()))?;
    Ok(seed)
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn decode_hex(text: &str, path: &Path) -> Result<Vec<u8>, KeyError> {
    if text.len() != 64 || !text.len().is_multiple_of(2) {
        return Err(KeyError::Malformed {
            path: path.to_path_buf(),
            got: text.len(),
        });
    }
    let mut out = Vec::with_capacity(32);
    for pair in text.as_bytes().chunks(2) {
        let hi = (pair[0] as char).to_digit(16);
        let lo = (pair[1] as char).to_digit(16);
        match (hi, lo) {
            (Some(hi), Some(lo)) => out.push(((hi << 4) | lo) as u8),
            _ => {
                return Err(KeyError::Malformed {
                    path: path.to_path_buf(),
                    got: text.len(),
                })
            }
        }
    }
    Ok(out)
}

/// Create (or validate) a directory only its owner may enter.
pub fn create_private_dir(path: &Path) -> Result<(), KeyError> {
    std::fs::create_dir_all(path).map_err(|e| KeyError::Io {
        path: path.to_path_buf(),
        detail: e.to_string(),
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(DIR_MODE)).map_err(|e| {
            KeyError::Io {
                path: path.to_path_buf(),
                detail: e.to_string(),
            }
        })?;
    }
    Ok(())
}

/// Write a secret to a file only its owner can read, atomically enough for a
/// key: the file is created 0600 before any byte is written into it.
fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), KeyError> {
    use std::io::Write;
    let io = |e: std::io::Error| KeyError::Io {
        path: path.to_path_buf(),
        detail: e.to_string(),
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(FILE_MODE);
    }
    let mut file = options.open(path).map_err(io)?;
    file.write_all(bytes).map_err(io)?;
    file.write_all(b"\n").map_err(io)?;
    file.sync_all().map_err(io)
}

/// Refuse a key file anyone else can read or write (Unix).
#[cfg(unix)]
fn check_permissions(path: &Path) -> Result<(), KeyError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)
        .map_err(|e| KeyError::Io {
            path: path.to_path_buf(),
            detail: e.to_string(),
        })?
        .permissions()
        .mode()
        & 0o777;
    if mode & !FILE_MODE != 0 {
        return Err(KeyError::Permissions {
            path: path.to_path_buf(),
            mode,
            required: FILE_MODE,
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_permissions(_path: &Path) -> Result<(), KeyError> {
    // Windows ACLs are the equivalent control; the Phase 2 work that ships
    // Windows remote sessions owns the ACL hardening (tracked on the roadmap).
    Ok(())
}

/// The identity root: `$ARREO_IDENTITY_DIR`, else `$XDG_DATA_HOME/arreo`, else
/// `$HOME/.local/share/arreo`.
#[must_use]
pub fn identity_dir() -> PathBuf {
    if let Some(custom) = std::env::var_os(IDENTITY_DIR_ENV) {
        return PathBuf::from(custom);
    }
    if let Some(data) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(data).join("arreo");
    }
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("arreo");
    }
    PathBuf::from(".arreo")
}

/// `<identity root>/identity` — the directory holding `root.key` and
/// `devices/`.
#[must_use]
pub fn identity_root() -> PathBuf {
    identity_dir().join("identity")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "transport")]
    fn the_noise_static_is_the_same_point_the_public_key_maps_to() {
        // The property the whole transport design rests on: a peer that derives
        // our X25519 public key from our ed25519 identity key gets exactly the
        // key our stored secret produces. Cross-checked against an independent
        // computation (scalar x B_montgomery) rather than restating the
        // implementation.
        for seed in [[0u8; 32], [1u8; 32], [42u8; 32], [255u8; 32]] {
            let key = DeviceKey::from_seed(seed);
            let stat = key.noise_static();
            assert_eq!(
                stat.public(),
                noise_public_key(&key.public()),
                "derived public half disagrees with the identity key's image"
            );
            // Independent path: clamped scalar times the Montgomery basepoint.
            let scalar = curve25519_dalek::scalar::Scalar::from_bytes_mod_order(stat.secret());
            let from_scalar = curve25519_dalek::edwards::EdwardsPoint::mul_base(&scalar)
                .to_montgomery()
                .to_bytes();
            assert_eq!(
                from_scalar,
                stat.public(),
                "secret and public are not a pair"
            );
        }
    }

    #[test]
    #[cfg(feature = "transport")]
    fn two_identities_have_two_different_noise_statics() {
        let a = DeviceKey::from_seed([3u8; 32]).noise_static();
        let b = DeviceKey::from_seed([4u8; 32]).noise_static();
        assert_ne!(a.public(), b.public());
        assert_ne!(a.secret(), b.secret());
    }

    #[test]
    #[cfg(feature = "transport")]
    fn the_noise_static_never_prints_its_secret() {
        let key = DeviceKey::from_seed([9u8; 32]);
        let stat = key.noise_static();
        let shown = format!("{stat:?}");
        assert!(
            !shown.contains(&hex(&stat.secret())),
            "Debug leaked the Noise secret"
        );
        assert!(
            shown.contains(&hex(&stat.public())),
            "the public half is useful in logs"
        );
    }

    #[test]
    #[cfg(feature = "transport")]
    fn the_root_key_has_a_noise_static_too() {
        // The server authenticates with the same identity that signs device
        // certificates, so a client pins one key and gets both.
        let root = RootKey::from_seed([7u8; 32]);
        let stat = root.noise_static();
        assert_eq!(stat.public(), noise_public_key(&root.public()));
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arreo-identity-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn a_generated_key_signs_and_verifies() {
        let key = DeviceKey::generate().expect("entropy");
        let signature = key.sign(b"hello arreo");
        assert!(verify(&key.public(), b"hello arreo", &signature));
        // A different payload must not verify with the same signature.
        assert!(!verify(&key.public(), b"hello arreo!", &signature));
    }

    #[test]
    fn two_generated_keys_differ() {
        let a = DeviceKey::generate().expect("entropy");
        let b = DeviceKey::generate().expect("entropy");
        assert_ne!(a.public_hex(), b.public_hex(), "entropy is not re-seeded");
    }

    #[test]
    fn a_device_key_never_prints_its_secret() {
        let key = DeviceKey::generate().expect("entropy");
        let shown = format!("{key:?}");
        let secret = hex(&key.signing.to_bytes());
        assert!(!shown.contains(&secret), "Debug leaked the private key");
        assert!(
            shown.contains(&key.public_hex()),
            "public half is useful in logs"
        );
    }

    #[test]
    fn round_trips_through_a_private_file() {
        let dir = scratch("roundtrip").join("identity");
        let path = dir.join("device.key");
        let key = DeviceKey::generate().expect("entropy");
        key.save(&path).expect("save");
        let loaded = DeviceKey::load(&path).expect("load");
        assert_eq!(loaded.public_hex(), key.public_hex());
        // And the loaded key really signs as the same device.
        let signature = loaded.sign(b"payload");
        assert!(verify(&key.public(), b"payload", &signature));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, FILE_MODE, "key file must be owner-only");
            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, DIR_MODE, "identity dir must be owner-only");
        }
        std::fs::remove_dir_all(dir.parent().unwrap()).ok();
    }

    #[test]
    fn load_or_generate_creates_once_and_reuses_after() {
        let root = scratch("lazy");
        let path = root.join("identity").join("root.key");
        let first = RootKey::load_or_generate(&path).expect("bootstrap");
        let second = RootKey::load_or_generate(&path).expect("reuse");
        assert_eq!(
            first.public_hex(),
            second.public_hex(),
            "root key must be stable"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_malformed_or_missing_key_is_a_typed_error() {
        let root = scratch("malformed");
        let dir = root.join("identity");
        create_private_dir(&dir).expect("dir");
        let path = dir.join("bad.key");
        std::fs::write(&path, "not a key").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(FILE_MODE)).unwrap();
        }
        assert!(matches!(
            DeviceKey::load(&path),
            Err(KeyError::Malformed { .. })
        ));
        assert!(matches!(
            DeviceKey::load(&dir.join("nope.key")),
            Err(KeyError::Missing { .. })
        ));
        // A file with 64 non-hex characters is malformed, not a panic.
        std::fs::write(&path, "z".repeat(64)).unwrap();
        assert!(matches!(
            DeviceKey::load(&path),
            Err(KeyError::Malformed { .. })
        ));
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_world_readable_key_file_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("perms");
        let dir = root.join("identity");
        create_private_dir(&dir).expect("dir");
        let path = dir.join("loose.key");
        DeviceKey::generate().unwrap().save(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        match DeviceKey::load(&path) {
            Err(KeyError::Permissions { mode, .. }) => assert_eq!(mode, 0o644),
            other => panic!("expected a permission refusal, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }
}
