//! Verifying a release artifact against the keys pinned in this repository
//! (T-0036, ROADMAP §3.13 channels-and-trust).
//!
//! One sentence: a byte of an update is trusted only when it carries a minisign
//! signature made by a key committed at `supply-chain/arreo.pub`, and the only
//! answers this module gives are *verified* or a typed refusal.
//!
//! ## Why the keys are compiled in
//!
//! [`include_str!`] puts the trust set inside every binary. Keys read from a path,
//! an environment variable or a file beside the artifact could be *pointed at
//! different keys* by whoever controls that path — and an update path whose trust
//! anchor is configurable by the thing being updated is not a trust anchor. The
//! only way to change which keys this build trusts is to rebuild it from a
//! reviewed commit, which is what makes the release job's verification meaningful.
//!
//! ## Why it is a *set* of keys, not one
//!
//! Rotation, and specifically the ugly case: a leaked key. With one pinned key,
//! that case is either "keep signing with a key you no longer trust" or "an
//! emergency release" — and an emergency release is how projects ship mistakes.
//! So the pinned file holds a **trust set**, a signature from any key in it
//! verifies, and rotation is a sequence of ordinary releases: publish the next
//! key alongside the current one, wait out the N−1 compat window while the current
//! key keeps signing, switch, then retire the old key. No code change is involved
//! in any of those four steps, which is what makes them a procedure an operator
//! can follow rather than an engineering project they must schedule. Today the set
//! holds exactly one key, because the next one is generated and held by the same
//! custodian as the current one — see `docs/release.md`.
//!
//! ## Why there is no boolean
//!
//! [`verify`] returns a [`Verified`] or an [`Error`] — never `bool`, never
//! `Option`, never "verified but …". There is no warn-and-continue branch and no
//! bypass, because a caller that *can* proceed on a failed check eventually will;
//! the whole value of this module is that proceeding is not expressible. Every
//! consumer — `arreo update verify`, the channel fetcher (T-0037), the release
//! job, the T-0042 slice — goes through this one door. A second verification
//! path would be a bug, not a feature.
//!
//! ## The five failure modes, and why each is separate
//!
//! * [`Error::MissingSignature`] — there is no signature at all. The most common
//!   failure in practice (a build that shipped the binary and forgot the
//!   `.minisig`), and it must never be confused with a bad signature: one is a
//!   broken release job, the other is an attack or corruption.
//! * [`Error::UnknownKeyId`] — the signature is well-formed but names a key
//!   outside the pinned set. Distinguished *before* the signature is checked,
//!   because "a stranger signed this" and "this file changed" call for different
//!   actions, and because the key id is what makes the pinned set the only keys
//!   this build will accept (a valid signature from an attacker's key verifies
//!   fine — just not here).
//! * [`Error::BadSignature`] — the signature does not authenticate these bytes:
//!   one flipped byte, a signature carried over from another file, a signature
//!   file that is not a signature at all, or a legacy (non-prehashed) signature.
//! * [`Error::DigestMismatch`] — the artifact's digest is not the one the trusted
//!   `SHA256SUMS` manifest records for it. The signature proves authorship; the
//!   digest proves this is the file the manifest listed.
//! * [`Error::Io`] — the bytes could not be read at all, with the path named.
//!
//! ## What a signature does and does not prove
//!
//! It proves *our* authorship of exactly these bytes. It is not Apple
//! notarization and not Windows Authenticode, and it says nothing about the
//! operator's machine. That gap is recorded in `docs/release.md` rather than
//! papered over.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use minisign_verify::{PublicKey, Signature};
use sha2::{Digest, Sha256};

/// The pinned trust set, compiled into every binary — see the module docs.
const PINNED_KEYS: &str = include_str!("../../../../supply-chain/arreo.pub");

/// The suffix a signature asset carries: `<artifact>.minisig`, which is what
/// `minisign -S -m <artifact>` writes and therefore what the release job uploads.
pub const SIGNATURE_SUFFIX: &str = "minisig";

/// How much of an artifact is held in memory at once. A daemon binary is tens of
/// megabytes; the signature check and the digest are computed over one pass of
/// this buffer, so neither the update path nor the release job ever holds a
/// whole binary in RAM.
const CHUNK: usize = 64 * 1024;

/// What went wrong, each variant naming the file it happened to.
///
/// The variants exist so the caller can print the right thing *and* act
/// differently: a missing signature is a broken release job, an unknown key is a
/// stranger, a bad signature is corruption or an attack, a digest mismatch is a
/// file the manifest never listed, and an I/O error is a path problem. There is
/// deliberately no variant meaning "could not check" that a caller might read as
/// "probably fine".
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The bytes could not be read.
    #[error("{path}: {detail}")]
    Io { path: String, detail: String },
    /// The artifact has no signature beside it.
    #[error("{artifact}: no signature at {path} — refusing an unverified artifact")]
    MissingSignature { artifact: String, path: String },
    /// The signature was made by a key this build does not trust.
    #[error("{artifact}: signed by key {found}, this build trusts {trusted}")]
    UnknownKeyId {
        artifact: String,
        found: String,
        trusted: String,
    },
    /// The signature does not authenticate this file.
    #[error("{artifact}: signature does not authenticate this file ({detail})")]
    BadSignature { artifact: String, detail: String },
    /// The artifact is not the file the trusted manifest describes.
    #[error("{artifact}: sha256 is {found}, the trusted manifest says {expected}")]
    DigestMismatch {
        artifact: String,
        found: String,
        expected: String,
    },
}

/// One public key this build trusts.
///
/// Wraps the parsed minisign key with the key id in the spelling minisign uses,
/// so a refusal can be compared against `supply-chain/arreo.pub` by eye.
#[derive(Debug)]
pub struct Key {
    inner: PublicKey,
    id: String,
}

impl Key {
    /// The key id, in minisign's own spelling: the 8 bytes reversed, uppercase
    /// hex — `076F2F7CEBE0AF51` for the key in `supply-chain/arreo.pub`, exactly
    /// as that file's comment prints it.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// The keys a build trusts: one or more, all equally.
///
/// A set rather than a key because rotation has to be possible *without* a code
/// change — see the module docs. A signature verifies if its key id matches any
/// key in the set, so adding the next key is a two-line commit to
/// `supply-chain/arreo.pub` and removing a retired key is deleting those lines.
#[derive(Debug)]
pub struct TrustSet {
    keys: Vec<Key>,
}

impl TrustSet {
    /// Parse a trust set: one or more minisign public keys, each in the two-line
    /// form `minisign -G` writes (a comment line, then the base64 blob).
    ///
    /// `None` means "this text holds no minisign public key at all" — the one
    /// condition under which a build has no trust anchor and must refuse
    /// everything. A malformed *second* key is refused too rather than skipped:
    /// silently ignoring half of a trust set would turn a typo in the next key's
    /// commit into a rotation that quietly did not happen.
    #[must_use]
    pub fn parse(text: &str) -> Option<TrustSet> {
        let mut keys = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with("untrusted comment:") {
                continue;
            }
            let inner = PublicKey::from_base64(line).ok()?;
            let id = key_id_from_blob(line)?;
            keys.push(Key { inner, id });
        }
        (!keys.is_empty()).then_some(TrustSet { keys })
    }

    /// The keys pinned in this build.
    ///
    /// # Panics
    ///
    /// Never in a build that passed CI: `supply-chain/arreo.pub` is committed,
    /// and `cargo test -p arreo-core --test update_verify` asserts it parses and
    /// carries the documented key id. A build that somehow shipped a broken trust
    /// set must not fall back to anything — refusing every artifact loudly is the
    /// only honest behavior, and it is strictly better than verifying against a
    /// key nobody holds.
    #[must_use]
    pub fn pinned() -> &'static TrustSet {
        static PINNED: LazyLock<TrustSet> = LazyLock::new(|| {
            TrustSet::parse(PINNED_KEYS).expect(
                "supply-chain/arreo.pub holds no minisign public key — this build has no trust \
                 anchor and cannot verify a release",
            )
        });
        &PINNED
    }

    /// Every key id in the set, for a message that names what is trusted.
    #[must_use]
    pub fn ids(&self) -> String {
        self.keys.iter().map(Key::id).collect::<Vec<_>>().join(", ")
    }

    /// The key a signature names, or `None` when it names a stranger.
    #[must_use]
    fn find(&self, id: &str) -> Option<&Key> {
        self.keys.iter().find(|key| key.id == id)
    }
}

/// A signature that verified: what a caller needs to report it, and nothing that
/// lets a caller skip the check.
#[derive(Debug)]
pub struct Verified {
    /// The artifact whose bytes were checked.
    pub artifact: PathBuf,
    /// The key id that signed it, in minisign's spelling.
    pub key_id: String,
    /// The artifact's SHA-256, lowercase hex — the digest a `SHA256SUMS` line
    /// records, so a caller can compare it against a manifest it trusts.
    pub digest: String,
}

/// Where the signature for `artifact` lives: the sibling `<artifact>.minisig`.
#[must_use]
pub fn signature_path(artifact: &Path) -> PathBuf {
    let mut name = artifact
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "arreo".to_string());
    name.push('.');
    name.push_str(SIGNATURE_SUFFIX);
    artifact.with_file_name(name)
}

/// Verify `artifact` against the key pinned in this build.
///
/// `signature` names the `.minisig` file; `None` means the sibling
/// [`signature_path`], which is where the release job writes it. The bytes are
/// read once — the same pass feeds the signature check and computes the digest —
/// and a `SHA256SUMS` manifest is verified by calling this on the manifest
/// itself.
pub fn verify(artifact: &Path, signature: Option<&Path>) -> Result<Verified, Error> {
    verify_with(TrustSet::pinned(), artifact, signature)
}

/// Verify `artifact` against an explicit trust set.
///
/// The product only ever passes [`TrustSet::pinned`]. This door exists because a
/// *refusal* can only be proved against keys whose secrets are in hand: the
/// tamper test generates a throwaway keypair, signs a fixture with it, and shows
/// that a flipped byte is refused. It is also what the T-0042 release slice
/// drives, and it is the same code path as [`verify`] — one implementation, two
/// ways to name the keys.
pub fn verify_with(
    keys: &TrustSet,
    artifact: &Path,
    signature: Option<&Path>,
) -> Result<Verified, Error> {
    let artifact_name = artifact.display().to_string();
    let sig_path = signature.map_or_else(|| signature_path(artifact), Path::to_path_buf);
    let text = match read_text(&sig_path) {
        Ok(text) => text,
        Err(ReadError::Missing) => {
            return Err(Error::MissingSignature {
                artifact: artifact_name,
                path: sig_path.display().to_string(),
            })
        }
        Err(ReadError::NotText) => {
            return Err(Error::BadSignature {
                artifact: artifact_name,
                detail: format!("{} is not a text signature file", sig_path.display()),
            })
        }
        Err(ReadError::Io(detail)) => {
            return Err(Error::Io {
                path: sig_path.display().to_string(),
                detail,
            })
        }
    };

    // **The key id first.** A signature from a key outside the set is refused as
    // exactly that, before the signature is even checked: the id check is the
    // pinned-set decision, and doing it here keeps the refusal independent of how
    // the verification crate orders its own checks.
    let found = key_id_of(&text).ok_or_else(|| Error::BadSignature {
        artifact: artifact_name.clone(),
        detail: format!("{} is not a minisign signature", sig_path.display()),
    })?;
    let Some(key) = keys.find(&found) else {
        return Err(Error::UnknownKeyId {
            artifact: artifact_name,
            found,
            trusted: keys.ids(),
        });
    };

    let parsed = Signature::decode(&text).map_err(|e| Error::BadSignature {
        artifact: artifact_name.clone(),
        detail: format!(
            "{} is not a readable minisign signature: {e}",
            sig_path.display()
        ),
    })?;

    let mut file = File::open(artifact).map_err(|e| Error::Io {
        path: artifact_name.clone(),
        detail: e.to_string(),
    })?;
    let digest = verify_streaming(key, &parsed, &mut file, &artifact_name)?;
    Ok(Verified {
        artifact: artifact.to_path_buf(),
        key_id: key.id().to_string(),
        digest,
    })
}

/// The artifact's SHA-256, lowercase hex.
///
/// Exposed because the digest is half of what an install script needs — the
/// signature says *who*, the digest says *which file* — and because computing it
/// here keeps one implementation of the streaming read.
pub fn sha256(artifact: &Path) -> Result<String, Error> {
    let name = artifact.display().to_string();
    let mut file = File::open(artifact).map_err(|e| Error::Io {
        path: name.clone(),
        detail: e.to_string(),
    })?;
    let mut hasher = Sha256::new();
    read_chunks(&mut file, &name, |chunk| hasher.update(chunk))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Check `artifact` against the entry for it in a trusted `SHA256SUMS` manifest.
///
/// **The manifest must already have been verified** — [`verify`] on the manifest
/// itself — or its digests mean nothing; the CLI's `--manifest` does both, in
/// that order. The manifest is the only trusted digest source (never a web page,
/// never a filename), and an artifact the manifest does not mention is refused:
/// a file nobody listed is exactly what a digest check exists to catch.
pub fn check_manifest_digest(artifact: &Path, manifest: &Path) -> Result<String, Error> {
    let name = artifact.display().to_string();
    let text = match read_text(manifest) {
        Ok(text) => text,
        Err(ReadError::Missing) => {
            return Err(Error::Io {
                path: manifest.display().to_string(),
                detail: "no such manifest".to_string(),
            })
        }
        Err(ReadError::NotText) => {
            return Err(Error::Io {
                path: manifest.display().to_string(),
                detail: "not a text manifest".to_string(),
            })
        }
        Err(ReadError::Io(detail)) => {
            return Err(Error::Io {
                path: manifest.display().to_string(),
                detail,
            })
        }
    };
    let found = sha256(artifact)?;
    let expected = manifest_digest(&text, artifact);
    match expected {
        Some(expected) if expected.eq_ignore_ascii_case(&found) => Ok(found),
        Some(expected) => Err(Error::DigestMismatch {
            artifact: name,
            found,
            expected,
        }),
        None => Err(Error::DigestMismatch {
            artifact: name,
            found,
            expected: format!("no entry for this artifact in {}", manifest.display()),
        }),
    }
}

/// The digest a `SHA256SUMS` line records for `artifact`.
///
/// `sha256sum`'s own format: `<hex>  <name>`, with `*` instead of the second
/// space for binary mode. Compared by file name — the digest is the trust
/// anchor, so where the line happens to say the file lives is irrelevant.
#[must_use]
pub fn manifest_digest(manifest: &str, artifact: &Path) -> Option<String> {
    let wanted = artifact.file_name()?.to_string_lossy().into_owned();
    manifest.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let digest = fields.next()?;
        let entry = fields.next()?.trim_start_matches('*');
        let entry = entry.rsplit('/').next().unwrap_or(entry);
        (entry == wanted).then(|| digest.to_ascii_lowercase())
    })
}

/// One pass over the file: hash it, and verify the signature over the same bytes.
fn verify_streaming(
    key: &Key,
    signature: &Signature,
    file: &mut File,
    artifact: &str,
) -> Result<String, Error> {
    let bad = |detail: String| Error::BadSignature {
        artifact: artifact.to_string(),
        detail,
    };
    // The crate's streaming path is the prehashed one — BLAKE2b-512 over the
    // content, which is minisign's default since 0.9 and therefore what
    // `cargo dist sign` and `minisign -S` produce. A legacy signature (ed25519
    // over the raw bytes) is refused rather than accepted: nothing in this
    // repository produces one, and a verifier that quietly widens to a second
    // format is how "verified" stops meaning one thing.
    let mut verifier = key
        .inner
        .verify_stream(signature)
        .map_err(|e| bad(e.to_string()))?;
    let mut hasher = Sha256::new();
    read_chunks(file, artifact, |chunk| {
        hasher.update(chunk);
        verifier.update(chunk);
    })?;
    verifier.finalize().map_err(|e| bad(e.to_string()))?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Read `file` to the end in [`CHUNK`]-sized pieces, feeding each one onward.
fn read_chunks(file: &mut File, path: &str, mut feed: impl FnMut(&[u8])) -> Result<(), Error> {
    let mut buffer = vec![0u8; CHUNK];
    loop {
        let read = file.read(&mut buffer).map_err(|e| Error::Io {
            path: path.to_string(),
            detail: e.to_string(),
        })?;
        if read == 0 {
            return Ok(());
        }
        feed(&buffer[..read]);
    }
}

/// Why reading a small text file failed, in the three ways that need different
/// handling: absent (a missing signature), not text (not a signature), or an I/O
/// problem worth reporting verbatim.
enum ReadError {
    Missing,
    NotText,
    Io(String),
}

fn read_text(path: &Path) -> Result<String, ReadError> {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes).map_err(|_| ReadError::NotText),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(ReadError::Missing),
        Err(e) => Err(ReadError::Io(e.to_string())),
    }
}

/// The 8-byte key id a minisign signature carries, in minisign's spelling: the
/// bytes reversed, uppercase hex.
///
/// `minisign-verify` checks the key id but exposes no accessor for it, and the id
/// is what lets a refusal say *which* key signed. Reading the label out of a blob
/// the crate has already decoded is not a second implementation of the format:
/// the signature itself is still the crate's to check.
fn key_id_of(text: &str) -> Option<String> {
    let blob = text.lines().map(str::trim).find(|line| {
        !line.is_empty()
            && !line.starts_with("untrusted comment:")
            && !line.starts_with("trusted comment:")
    })?;
    key_id_from_blob(blob)
}

/// The same, for a base64 blob already in hand — a public key line, or a
/// signature line. Both carry the key id at the same offset (bytes 2..10).
fn key_id_from_blob(blob: &str) -> Option<String> {
    let bytes = BASE64.decode(blob).ok()?;
    let id = bytes.get(2..10)?;
    Some(id.iter().rev().map(|byte| format!("{byte:02X}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trust anchor is a *real* key with a known id, not a placeholder.
    ///
    /// A placeholder public key is worse than none: every artifact would fail
    /// closed against a key nobody holds, which looks like a working gate and can
    /// never pass. This is the check that turns "the operator committed a key"
    /// into something CI can see, and every comment line in the file must agree
    /// with the key id inside the key it introduces — a mismatch means someone
    /// edited one and not the other, which is exactly the mistake a rotation
    /// makes at 3am.
    #[test]
    fn the_pinned_key_is_real_and_every_comment_agrees_with_its_key() {
        let keys = TrustSet::pinned();
        assert_eq!(
            keys.ids(),
            "076F2F7CEBE0AF51",
            "the pinned trust set changed"
        );
        let comments: Vec<&str> = PINNED_KEYS
            .lines()
            .filter(|line| line.starts_with("untrusted comment:"))
            .collect();
        for key in &keys.keys {
            assert!(
                comments.iter().any(|line| line.contains(key.id())),
                "supply-chain/arreo.pub must carry a comment naming key {} — the file is {:?}",
                key.id(),
                PINNED_KEYS
            );
            assert!(
                !matches!(key.id(), "0000000000000000" | "FFFFFFFFFFFFFFFF"),
                "the pinned trust set holds a placeholder key"
            );
        }
    }

    /// Nothing to check is a refusal, not a pass — and it names what it looked
    /// for, because the common cause is a release job that forgot to sign.
    #[test]
    fn an_absent_signature_is_a_typed_refusal() {
        let missing = Path::new("/nonexistent/arreo-for-tests");
        match verify(missing, None) {
            Err(Error::MissingSignature { artifact, path }) => {
                assert_eq!(artifact, missing.display().to_string());
                assert!(path.ends_with("arreo-for-tests.minisig"), "got {path}");
            }
            other => panic!("expected MissingSignature, got {other:?}"),
        }
    }

    /// A `SHA256SUMS` line is read the way `sha256sum` writes it, including the
    /// `*` binary-mode marker and a path in front of the name.
    #[test]
    fn a_manifest_line_is_read_by_name() {
        let text = "aa11  arreo-x86_64\nBB22 *dist/arreo-server-aarch64\n";
        assert_eq!(
            manifest_digest(text, Path::new("/tmp/arreo-x86_64")).as_deref(),
            Some("aa11")
        );
        assert_eq!(
            manifest_digest(text, Path::new("arreo-server-aarch64")).as_deref(),
            Some("bb22")
        );
        assert_eq!(manifest_digest(text, Path::new("arreo-unknown")), None);
    }
}
