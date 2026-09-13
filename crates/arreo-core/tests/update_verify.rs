//! T-0036: the verifier refuses what it must — proven by artifact, not by
//! argument.
//!
//! Every fixture here is signed by a **throwaway keypair generated in this
//! process**, written into this test's own temp directory and deleted with it.
//! Nothing is signed with the real key, no secret is committed, and no fixture is
//! reused: a test that needed the release key would be a test that cannot run on
//! a contributor's machine or in a fork, which is exactly the property that makes
//! the release key worth having.
//!
//! ## Why this file writes minisign signatures
//!
//! A refusal can only be *proved* by producing something valid and then breaking
//! it — so the test needs a signer, while the workspace deliberately ships none
//! (`minisign-verify` is verification only; the signing key exists solely as the
//! `MINISIGN_SECRET_KEY` CI secret). Writing the format here, from
//! `ed25519-dalek` and `blake2` — both already in the dependency graph — keeps
//! the product free of signing code while still letting the negative tests mean
//! something. The last test in this file checks the writer against the **real**
//! `minisign` binary when it is installed, so the fixtures cannot pass by being
//! wrong in the same way as the verifier.
//!
//! ## What each test turns red if removed
//!
//! * the positive case: the negative tests below would pass against a verifier
//!   that refuses everything;
//! * the flipped byte: `BadSignature` naming the file — the tamper-refusal claim
//!   this task exists for;
//! * the stranger's key: `UnknownKeyId`, with the same bytes verifying against
//!   the stranger's own key, which is what proves the *key id* is checked and not
//!   merely the signature's validity;
//! * the manifest digest: `DigestMismatch`, including for a file the manifest
//!   never listed.

use std::path::{Path, PathBuf};

use arreo_core::update::verify::{
    check_manifest_digest, sha256, verify_with, Error, TrustSet, Verified,
};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use blake2::Blake2b512;
use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest as _, Sha256};

/// A minisign keypair that exists only for one test.
struct Throwaway {
    signing: SigningKey,
    key_id: [u8; 8],
}

impl Throwaway {
    fn new() -> Throwaway {
        let mut seed = [0u8; 32];
        let mut key_id = [0u8; 8];
        getrandom::fill(&mut seed).expect("OS entropy");
        getrandom::fill(&mut key_id).expect("OS entropy");
        Throwaway {
            signing: SigningKey::from_bytes(&seed),
            key_id,
        }
    }

    /// The key id in minisign's spelling: the bytes reversed, uppercase hex.
    fn id(&self) -> String {
        self.key_id
            .iter()
            .rev()
            .map(|byte| format!("{byte:02X}"))
            .collect()
    }

    /// The two-line `minisign.pub` text, exactly the shape
    /// `supply-chain/arreo.pub` has.
    fn public_key_text(&self) -> String {
        let mut blob = Vec::new();
        blob.extend_from_slice(b"Ed"); // minisign's key algorithm, as its own -G writes
        blob.extend_from_slice(&self.key_id);
        blob.extend_from_slice(self.signing.verifying_key().as_bytes());
        format!(
            "untrusted comment: minisign public key {}\n{}\n",
            self.id(),
            BASE64.encode(blob)
        )
    }

    fn key(&self) -> TrustSet {
        TrustSet::parse(&self.public_key_text()).expect("the generated key parses")
    }

    /// A prehashed signature — `alg || key id || ed25519(blake2b512(content))`,
    /// plus the global signature over `signature || trusted comment` — which is
    /// what `minisign -S` produces by default and therefore what the release job
    /// uploads.
    fn sign(&self, content: &[u8], name: &str) -> String {
        let prehash = Blake2b512::digest(content);
        let signature = self.signing.sign(&prehash);
        let trusted = format!("timestamp:1700000000\tfile:{name}\thashed");
        let mut global = signature.to_bytes().to_vec();
        global.extend_from_slice(trusted.as_bytes());
        let global_signature = self.signing.sign(&global);

        let mut blob = Vec::new();
        blob.extend_from_slice(b"ED"); // prehashed
        blob.extend_from_slice(&self.key_id);
        blob.extend_from_slice(&signature.to_bytes());
        format!(
            "untrusted comment: signature from minisign secret key\n{}\ntrusted comment: {trusted}\n{}\n",
            BASE64.encode(blob),
            BASE64.encode(global_signature.to_bytes())
        )
    }

    /// The same, in minisign's legacy mode (`Ed`): ed25519 over the raw bytes.
    /// Nothing in this repository produces one; the verifier must refuse it.
    fn sign_legacy(&self, content: &[u8], name: &str) -> String {
        let signature = self.signing.sign(content);
        let trusted = format!("timestamp:1700000000\tfile:{name}");
        let mut global = signature.to_bytes().to_vec();
        global.extend_from_slice(trusted.as_bytes());
        let global_signature = self.signing.sign(&global);
        let mut blob = Vec::new();
        blob.extend_from_slice(b"Ed");
        blob.extend_from_slice(&self.key_id);
        blob.extend_from_slice(&signature.to_bytes());
        format!(
            "untrusted comment: signature from minisign secret key\n{}\ntrusted comment: {trusted}\n{}\n",
            BASE64.encode(blob),
            BASE64.encode(global_signature.to_bytes())
        )
    }
}

/// One test's own directory, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("arreo-verify-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch directory");
        Scratch(dir)
    }

    /// Write a file and return its path.
    fn write(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).expect("write fixture");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The positive case. Without it the refusals below would be satisfied by a
/// verifier that refuses everything — including the release it is meant to
/// accept.
#[test]
fn a_signed_fixture_verifies_and_reports_its_key_id_and_digest() {
    let scratch = Scratch::new("positive");
    let key = Throwaway::new();
    let content = b"an arreo release artifact\n";
    let artifact = scratch.write("arreo-0.1.0-x86_64", content);
    scratch.write(
        "arreo-0.1.0-x86_64.minisig",
        key.sign(content, "arreo-0.1.0-x86_64").as_bytes(),
    );

    // The signature is found beside the artifact, the way the release job ships it.
    let Verified {
        artifact: verified_path,
        key_id,
        digest,
    } = verify_with(&key.key(), &artifact, None).expect("a valid signature verifies");
    assert_eq!(verified_path, artifact);
    assert_eq!(key_id, key.id(), "the reported key id is the signing key's");
    assert_eq!(digest, hex(&Sha256::digest(content)), "the reported digest");
}

/// **The tamper test.** One flipped byte, the signature untouched: the artifact
/// is no longer what was signed, and the refusal must say so — as `BadSignature`,
/// naming the file, rather than a boolean a caller could ignore.
#[test]
fn one_flipped_byte_is_refused_as_a_bad_signature_naming_the_file() {
    let scratch = Scratch::new("tamper");
    let key = Throwaway::new();
    let content = b"the bytes that were signed\n";
    let artifact = scratch.write("arreo-0.1.0-aarch64", content);
    let signature = scratch.write(
        "arreo-0.1.0-aarch64.minisig",
        key.sign(content, "arreo-0.1.0-aarch64").as_bytes(),
    );
    // Sanity: this exact pair verifies, so what follows is about the flip.
    verify_with(&key.key(), &artifact, Some(&signature)).expect("the untampered pair verifies");

    let mut tampered = content.to_vec();
    let index = tampered.len() / 2;
    tampered[index] ^= 0x01;
    scratch.write("arreo-0.1.0-aarch64", &tampered);

    let refused = verify_with(&key.key(), &artifact, Some(&signature))
        .expect_err("a flipped byte must be refused");
    match &refused {
        Error::BadSignature {
            artifact: named, ..
        } => {
            assert!(
                named.ends_with("arreo-0.1.0-aarch64"),
                "the refusal must name the file, got {named}"
            );
        }
        other => panic!("expected BadSignature, got {other:?}"),
    }
    assert!(
        refused.to_string().contains("arreo-0.1.0-aarch64"),
        "the printed refusal must name the file: {refused}"
    );
}

/// A valid signature carried over to a *different* file is refused too: the
/// signature covers bytes, not names, so `--sig` cannot be used to bless an
/// artifact that was never signed.
#[test]
fn a_valid_signature_over_a_different_file_is_refused() {
    let scratch = Scratch::new("swapped");
    let key = Throwaway::new();
    let signed = scratch.write("arreo-0.1.0-linux", b"the signed artifact\n");
    let other = scratch.write("arreo-0.1.0-windows", b"a different artifact\n");
    let signature = scratch.write(
        "arreo-0.1.0-linux.minisig",
        key.sign(b"the signed artifact\n", "arreo-0.1.0-linux")
            .as_bytes(),
    );
    verify_with(&key.key(), &signed, Some(&signature)).expect("the signed pair verifies");

    match verify_with(&key.key(), &other, Some(&signature)) {
        Err(Error::BadSignature {
            artifact: named, ..
        }) => {
            assert!(named.ends_with("arreo-0.1.0-windows"), "got {named}");
        }
        other => panic!("expected BadSignature for the unsigned artifact, got {other:?}"),
    }
}

/// The key id decides, not the signature's validity: the *same bytes* verify
/// against the stranger's own key and are refused against ours. A verifier that
/// only checked "is this a valid signature" would accept an attacker's release.
#[test]
fn a_signature_from_another_key_is_refused_by_key_id() {
    let scratch = Scratch::new("stranger");
    let ours = Throwaway::new();
    let stranger = Throwaway::new();
    let content = b"an artifact signed by somebody else\n";
    let artifact = scratch.write("arreo-0.1.0-macos", content);
    scratch.write(
        "arreo-0.1.0-macos.minisig",
        stranger.sign(content, "arreo-0.1.0-macos").as_bytes(),
    );

    // The stranger's signature is *valid* — for the stranger's key.
    verify_with(&stranger.key(), &artifact, None)
        .expect("the stranger's own key accepts the stranger's signature");

    match verify_with(&ours.key(), &artifact, None) {
        Err(Error::UnknownKeyId {
            found,
            trusted,
            artifact: named,
        }) => {
            assert_eq!(found, stranger.id(), "the refusal names the signing key");
            assert_eq!(trusted, ours.id(), "and the key this build trusts");
            assert!(named.ends_with("arreo-0.1.0-macos"), "got {named}");
        }
        other => panic!("expected UnknownKeyId, got {other:?}"),
    }

    // The same refusal through the *pinned* door the product actually uses.
    match verify_with(TrustSet::pinned(), &artifact, None) {
        Err(Error::UnknownKeyId { trusted, .. }) => {
            assert_eq!(trusted, "076F2F7CEBE0AF51");
        }
        other => panic!("expected UnknownKeyId against the pinned key, got {other:?}"),
    }
}

/// An artifact with no signature at all is the failure a broken release job
/// produces, and it is a *different* refusal from a bad signature.
#[test]
fn a_missing_signature_is_refused_and_names_what_it_looked_for() {
    let scratch = Scratch::new("unsigned");
    let key = Throwaway::new();
    let artifact = scratch.write("arreo-0.1.0-unsigned", b"shipped without a signature\n");

    match verify_with(&key.key(), &artifact, None) {
        Err(Error::MissingSignature {
            artifact: named,
            path,
        }) => {
            assert!(named.ends_with("arreo-0.1.0-unsigned"), "got {named}");
            assert!(path.ends_with("arreo-0.1.0-unsigned.minisig"), "got {path}");
        }
        other => panic!("expected MissingSignature, got {other:?}"),
    }

    // An explicit --sig that names nothing is the same refusal, not a panic and
    // not a pass.
    match verify_with(&key.key(), &artifact, Some(Path::new("/nonexistent.sig"))) {
        Err(Error::MissingSignature { .. }) => {}
        other => panic!("expected MissingSignature for an absent --sig, got {other:?}"),
    }
}

/// A signature whose artifact cannot be read is an I/O refusal naming the path —
/// never a pass.
#[test]
fn an_unreadable_artifact_is_an_io_refusal() {
    let scratch = Scratch::new("io");
    let key = Throwaway::new();
    let signature = scratch.write("absent.minisig", key.sign(b"x", "absent").as_bytes());

    match verify_with(&key.key(), &scratch.0.join("absent"), Some(&signature)) {
        Err(Error::Io { path, .. }) => assert!(path.ends_with("absent"), "got {path}"),
        other => panic!("expected Io, got {other:?}"),
    }
}

/// A legacy (non-prehashed) signature is refused rather than quietly accepted:
/// `minisign -S` has produced prehashed signatures by default since 0.9, so
/// accepting a second format would widen what "verified" means for nothing.
#[test]
fn a_legacy_signature_is_refused() {
    let scratch = Scratch::new("legacy");
    let key = Throwaway::new();
    let content = b"a legacy-mode signature\n";
    let artifact = scratch.write("arreo-0.1.0-legacy", content);
    scratch.write(
        "arreo-0.1.0-legacy.minisig",
        key.sign_legacy(content, "arreo-0.1.0-legacy").as_bytes(),
    );

    match verify_with(&key.key(), &artifact, None) {
        Err(Error::BadSignature {
            artifact: named, ..
        }) => {
            assert!(named.ends_with("arreo-0.1.0-legacy"), "got {named}");
        }
        other => panic!("expected BadSignature for a legacy signature, got {other:?}"),
    }
}

/// The digest check: the manifest is the only trusted digest source, so a file
/// whose bytes are not the ones it lists is refused — including a file it never
/// listed.
#[test]
fn an_artifact_the_manifest_does_not_describe_is_refused() {
    let scratch = Scratch::new("manifest");
    let content = b"the artifact the manifest lists\n";
    let artifact = scratch.write("arreo-0.1.0-linux", content);
    let digest = sha256(&artifact).expect("digest");

    let good = scratch.write(
        "SHA256SUMS",
        format!("{digest}  arreo-0.1.0-linux\n").as_bytes(),
    );
    assert_eq!(
        check_manifest_digest(&artifact, &good).expect("the listed artifact matches"),
        digest
    );

    let wrong = scratch.write(
        "SHA256SUMS.wrong",
        format!("{}  arreo-0.1.0-linux\n", "0".repeat(64)).as_bytes(),
    );
    match check_manifest_digest(&artifact, &wrong) {
        Err(Error::DigestMismatch {
            found, expected, ..
        }) => {
            assert_eq!(found, digest);
            assert_eq!(expected, "0".repeat(64));
        }
        other => panic!("expected DigestMismatch, got {other:?}"),
    }

    let unlisted = scratch.write("SHA256SUMS.unlisted", b"aa11  arreo-other\n");
    match check_manifest_digest(&artifact, &unlisted) {
        Err(Error::DigestMismatch { expected, .. }) => {
            assert!(expected.contains("no entry"), "got {expected}");
        }
        other => panic!("expected DigestMismatch for an unlisted file, got {other:?}"),
    }

    // And the same check on tampered bytes: the manifest did not move, so the
    // digest is what catches a file that was changed after it was listed.
    let mut tampered = content.to_vec();
    tampered[0] ^= 0x01;
    scratch.write("arreo-0.1.0-linux", &tampered);
    match check_manifest_digest(&artifact, &good) {
        Err(Error::DigestMismatch { .. }) => {}
        other => panic!("expected DigestMismatch after tampering, got {other:?}"),
    }
}

/// The fixture writer is only evidence if it writes what `minisign` writes.
///
/// This is the one test that can compare against the real implementation, and it
/// runs in both directions: the signature this file's throwaway keypair produced
/// must be accepted by the `minisign` binary, and a signature the `minisign`
/// binary produced must be accepted here. `minisign` is deliberately not a build
/// dependency (the workspace ships no signer), so when the binary is absent this
/// self-skips with a loud note — the T-0019 no-delegation precedent: a skip is
/// reported, never counted as a pass.
#[test]
fn real_minisign_agrees_with_this_files_fixtures() {
    let Ok(version) = std::process::Command::new("minisign").arg("-v").output() else {
        eprintln!(
            "SKIPPED: no `minisign` binary on PATH — the throwaway fixtures were not \
             cross-checked against the real implementation (the release job runs this check \
             with minisign installed)"
        );
        return;
    };
    assert!(version.status.success(), "`minisign -v` failed");
    eprintln!(
        "cross-checking against {}",
        String::from_utf8_lossy(&version.stdout).trim()
    );

    let scratch = Scratch::new("crosscheck");
    let key = Throwaway::new();
    let content = b"cross-checked against the real implementation\n";
    let artifact = scratch.write("arreo-0.1.0-crosscheck", content);
    let pub_path = scratch.write("crosscheck.pub", key.public_key_text().as_bytes());

    // Ours → theirs: the real verifier accepts the fixture this file writes.
    let ours = scratch.write(
        "crosscheck.minisig",
        key.sign(content, "arreo-0.1.0-crosscheck").as_bytes(),
    );
    let checked = std::process::Command::new("minisign")
        .args(["-V", "-p"])
        .arg(&pub_path)
        .arg("-m")
        .arg(&artifact)
        .arg("-x")
        .arg(&ours)
        .output()
        .expect("run minisign -V");
    assert!(
        checked.status.success(),
        "the real minisign refused this file's signature: {}{}",
        String::from_utf8_lossy(&checked.stdout),
        String::from_utf8_lossy(&checked.stderr)
    );

    // Theirs → ours: a signature the real signer made verifies here.
    let secret = scratch.0.join("real.key");
    let real_pub = scratch.0.join("real.pub");
    let generated = std::process::Command::new("minisign")
        .args(["-G", "-W", "-p"])
        .arg(&real_pub)
        .arg("-s")
        .arg(&secret)
        .output()
        .expect("run minisign -G");
    assert!(
        generated.status.success(),
        "minisign -G failed: {}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let signed = std::process::Command::new("minisign")
        .args(["-S", "-s"])
        .arg(&secret)
        .arg("-m")
        .arg(&artifact)
        .output()
        .expect("run minisign -S");
    assert!(
        signed.status.success(),
        "minisign -S failed: {}",
        String::from_utf8_lossy(&signed.stderr)
    );
    let real_text = std::fs::read_to_string(&real_pub).expect("the generated public key");
    let real_key = TrustSet::parse(&real_text).expect("the generated key parses here");
    let verified = verify_with(&real_key, &artifact, None)
        .expect("a signature made by the real minisign must verify here");
    assert_eq!(verified.key_id, real_key.ids());
    assert_eq!(verified.digest, hex(&Sha256::digest(content)));
}

/// Rotation, proven rather than described: the trust set the module verifies
/// against is a *set*, so a release signed by the next key verifies on a build
/// that also pins the current one, and a key outside the set is still refused.
///
/// This is the property `docs/release.md`'s rotation procedure depends on: the
/// four steps there involve no code change because adding the next key's public
/// half to `supply-chain/arreo.pub` is the whole mechanism. Without this test the
/// document would be describing behavior nothing checks.
#[test]
fn a_two_key_trust_set_accepts_either_key_and_refuses_a_stranger() {
    let scratch = Scratch::new("rotation");
    let current = Throwaway::new();
    let next = Throwaway::new();
    let stranger = Throwaway::new();
    let content = b"a release signed by the next key\n";

    let set = TrustSet::parse(&format!(
        "{}{}",
        current.public_key_text(),
        next.public_key_text()
    ))
    .expect("both keys parse into one set");

    // Either key in the set signs, and the artifact verifies.
    for signer in [&current, &next] {
        let name = format!("arreo-0.1.0-{}", signer.id());
        let artifact = scratch.write(&name, content);
        scratch.write(
            &format!("{name}.minisig"),
            signer.sign(content, &name).as_bytes(),
        );
        let verified =
            verify_with(&set, &artifact, None).expect("a key in the set verifies the artifact");
        assert_eq!(verified.key_id, signer.id(), "the signing key is reported");
    }

    // A third key, outside the set, is refused by id — and the refusal names the
    // whole set, so an operator can see what this build does trust.
    let artifact = scratch.write("arreo-0.1.0-stranger", content);
    scratch.write(
        "arreo-0.1.0-stranger.minisig",
        stranger.sign(content, "arreo-0.1.0-stranger").as_bytes(),
    );
    match verify_with(&set, &artifact, None) {
        Err(Error::UnknownKeyId { trusted, .. }) => {
            assert!(trusted.contains(&current.id()), "got {trusted}");
            assert!(trusted.contains(&next.id()), "got {trusted}");
        }
        other => panic!("expected UnknownKeyId outside the set, got {other:?}"),
    }
}
