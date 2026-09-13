//! The release channel: a URL, a signed index, and the verifier (T-0037,
//! ROADMAP §3.13 channels-and-trust).
//!
//! One sentence: `arreo update` learns what to install by fetching a small signed
//! index from a URL, and the only thing that ever leaves this module is an
//! artifact [`verify`] has already accepted.
//!
//! ## What a channel is
//!
//! A channel is a **URL prefix** — this repository's GitHub Releases `latest` URL
//! unless configuration says otherwise — plus two files that live under it:
//!
//! ```text
//! <channel>/arreo-index.json           the index: the version, and the artifact per target
//! <channel>/arreo-index.json.minisig   its signature, made by the release job
//! <channel>/<artifact>                 the binary the index names
//! <channel>/<artifact>.minisig         its signature
//! ```
//!
//! Nothing else about the channel is known to this code: no API endpoint, no query
//! string, no version negotiation. That is deliberate — a channel that is a
//! *directory of files* can be mirrored by anyone (`file://`, a company web
//! server, a GitHub release), and the trust decision does not change when the
//! transport does.
//!
//! ## Why the index is signed, and why it carries no digests
//!
//! The index is **verified before a single byte of it is believed** (fail closed:
//! there is no branch that reads a version out of an unverified index). Its
//! signature is checked against the same pinned key every other artifact is — see
//! [`verify`] — so an index that is unsigned, signed by a stranger, or altered
//! after signing is refused with the verifier's own sentence.
//!
//! It carries **no digests**, and that is a decision rather than a gap: the
//! trusted digest source in this project is the signed `SHA256SUMS` manifest (see
//! `docs/release.md`), and a second list of digests in a second file would be a
//! second answer to "which bytes are this release?". What the index adds is the
//! two facts a manifest of digests cannot carry — **which version** this is, and
//! **which artifact is for this machine** — and the artifact's own signature is
//! what binds its bytes.
//!
//! ## Why one fetch, two transports
//!
//! `file://` and `https://` differ in exactly one place: how the bytes of a named
//! URL are obtained. Everything above [`Fetcher`] — the file names, the signature
//! check, the parse, the artifact download, the refusal sentences — is the same
//! code for both, and the tests prove it by checking one channel's content through
//! both transports (see `file_and_https_channels_differ_only_in_their_fetcher`).
//! The `file://` transport is not a test double: a self-hosted install mirroring
//! releases inside a firewall is a real deployment, and it is also what lets the
//! whole update story be exercised with no network at all.
//!
//! ## Why `https` is a TLS client here and not a subprocess
//!
//! The tempting shortcut is to shell out to `curl`. It is rejected on the same
//! grounds the verifier's key is compiled in: the bytes that arrive decide which
//! binary this machine runs, so the program that fetches them must not be resolved
//! through `$PATH` and must not be replaceable by a wrapper. The transport is
//! therefore the TLS stack the daemon already ships (see [`super::https`]), with
//! the platform certificate store as its roots.
//!
//! ## An empty channel is an answer, not an error
//!
//! Before the first release, `latest/download/arreo-index.json` does not exist.
//! That is the state this repository is in today, and `--check` reports it as "no
//! releases yet" with exit 0 rather than as a failure: an operator asking "is
//! there an update?" deserves the honest answer. A *present* index with no
//! signature is a different fact — a broken release job — and is refused.

use super::verify::{self, TrustSet, Verified};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The channel a build points at when neither `--channel` nor
/// `ARREO_CHANNEL_URL` says otherwise.
///
/// GitHub's `latest` alias, so one URL keeps working as releases are published;
/// the artifacts behind it are the ones the T-0036 release job signs.
pub const DEFAULT_URL: &str = "https://github.com/ivan-cavero/Arreo/releases/latest/download/";

/// The index's file name inside the channel.
pub const INDEX_FILE: &str = "arreo-index.json";

/// What the channel could not do, each variant naming the URL or the file.
///
/// [`Error::Verify`] carries the verifier's own refusal unchanged — one sentence,
/// the same one `arreo update verify` prints — because an operator who sees a
/// refusal must be able to act on it without translating it first.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The channel URL is not one this build can fetch.
    #[error("{url}: {detail}")]
    Url { url: String, detail: String },
    /// Nothing is published at that URL. On the *index* it means an empty
    /// channel; anywhere else it is a broken release.
    #[error("{url}: nothing is published at this path")]
    NotFound { url: String },
    /// The fetch itself failed: a socket, a TLS handshake, a non-2xx status.
    #[error("{url}: {detail}")]
    Fetch { url: String, detail: String },
    /// A local file the fetch needed to write or read.
    #[error("{path}: {detail}")]
    Io { path: String, detail: String },
    /// The verifier's refusal, verbatim (T-0036).
    #[error("{0}")]
    Verify(#[from] verify::Error),
    /// The index verified, but is not an index.
    #[error("{url}: the index is not a release index ({detail})")]
    Index { url: String, detail: String },
    /// The index names no build for this machine.
    #[error("release {version} does not ship a build for {target} (it carries {carries})")]
    NoArtifact {
        version: String,
        target: String,
        carries: String,
    },
}

impl Error {
    fn url(url: &str, detail: impl std::fmt::Display) -> Self {
        Self::Url {
            url: url.to_string(),
            detail: detail.to_string(),
        }
    }

    fn io(path: &Path, detail: impl std::fmt::Display) -> Self {
        Self::Io {
            path: path.display().to_string(),
            detail: detail.to_string(),
        }
    }
}

/// A channel: the URL prefix its files live under.
///
/// Normalized to end in exactly one `/`, so joining a file name to it is a
/// concatenation and not a set of special cases about slashes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    url: String,
}

impl Channel {
    /// A channel at `url`, refused here if its scheme is one this build cannot
    /// fetch.
    ///
    /// Checked at construction rather than at the first fetch so a typo in
    /// `--channel` is a usage error before anything is read, and so this type
    /// cannot hold a URL that would fail later for a reason the operator cannot
    /// see.
    pub fn new(url: impl Into<String>) -> Result<Self, Error> {
        let url = url.into();
        let trimmed = url.trim();
        if trimmed.is_empty() {
            return Err(Error::url(&url, "an empty channel URL names nothing"));
        }
        if let Transport::Https = Transport::of(trimmed)? {
            // Named here, once, so a build without TLS refuses the URL at the
            // point the operator typed it.
            #[cfg(not(feature = "transport"))]
            return Err(Error::url(
                trimmed,
                "this build has no TLS transport (built without the `transport` feature), so it \
                 cannot fetch an https channel",
            ));
        }
        let mut normalized = trimmed.to_string();
        if !normalized.ends_with('/') {
            normalized.push('/');
        }
        Ok(Self { url: normalized })
    }

    /// The URL as configured, with its trailing `/`.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// The index URL: `<channel>/arreo-index.json`.
    #[must_use]
    pub fn index_url(&self) -> String {
        self.join(INDEX_FILE)
    }

    /// A file's URL inside this channel.
    #[must_use]
    pub fn join(&self, name: &str) -> String {
        format!("{}{name}", self.url)
    }

    /// A file's signature URL: `<channel>/<name>.minisig`.
    #[must_use]
    pub fn signature_url(&self, name: &str) -> String {
        self.join(&format!("{name}.{}", verify::SIGNATURE_SUFFIX))
    }
}

impl std::fmt::Display for Channel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.url)
    }
}

impl std::str::FromStr for Channel {
    type Err = Error;

    fn from_str(url: &str) -> Result<Self, Self::Err> {
        Self::new(url)
    }
}

/// What the channel says is the newest release, as the verified index reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The release's version, without a leading `v` — the same spelling
    /// `arreo --version` prints.
    pub version: String,
    /// The target triple this artifact is for.
    pub target: String,
    /// The artifact's file name inside the channel.
    pub artifact: String,
    /// The key id that signed the index, in minisign's spelling.
    pub key_id: String,
    /// The index's own SHA-256 — auditable, and the thing an operator can write
    /// down when reporting a channel that served something odd.
    pub digest: String,
}

/// This machine's target triple, in the spelling the release job uses.
///
/// The release publishes one artifact per target (`arreo-<target>`) and the index
/// is keyed by exactly those names — the three `[workspace.metadata.dist]`
/// builds, plus the two variants a mirror may build for itself. A platform
/// outside the table reports `"unknown"` and the channel refuses with "does not
/// ship a build for unknown" rather than guessing: picking the nearest artifact
/// would be installing a binary for a different machine.
#[must_use]
pub fn host_target() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        ("windows", "aarch64") => "aarch64-pc-windows-msvc",
        _ => "unknown",
    }
}

/// Where fetched bytes live between the fetch and the install: `<state>/channel`.
///
/// In the state directory rather than a system temp directory for two reasons:
/// the fetched artifact is written somewhere an operator can look at what was
/// downloaded, and a directory shared with other users' processes is not where a
/// program puts something it is about to execute. `$ARREO_STATE_DIR` moves it,
/// which is how the tests keep off the developer's own state.
///
/// Two invocations that share the directory are safe by construction, not by
/// luck: each run deletes the files it fetched last time before fetching again,
/// and the verifier accepts or refuses the bytes as they are on disk — so a run
/// that overlaps another can see a spurious refusal, never a false acceptance.
#[must_use]
pub fn work_dir() -> PathBuf {
    super::resume::dir().join("channel")
}

/// Fetch the channel's index, verify it, and report what it names.
///
/// `None` means **the channel is empty** — there is no index at all, which is the
/// honest state before a first release. Every other failure is an `Err`: a
/// present index with no signature is a broken release job, not an empty channel,
/// and it is refused with the verifier's `MissingSignature`.
///
/// Nothing is installed, staged or moved: this is the whole of `--check`.
pub fn check(trust: &TrustSet, channel: &Channel, work: &Path) -> Result<Option<Release>, Error> {
    check_with(&Network, trust, channel, work)
}

/// Fetch the artifact `release` names, prove it against `trust`, and return it.
///
/// The bytes land in `work` (see [`work_dir`]), the signature is fetched beside
/// them, and the verified file is made executable — so it passes the same gate a
/// `--from` candidate passes, and the install path that follows is the same code
/// with the same rules. The file's *mode* is still the installed binary's
/// business: [`super::stage`] gives the staged copy the current binary's mode.
pub fn fetch(
    trust: &TrustSet,
    channel: &Channel,
    release: &Release,
    work: &Path,
) -> Result<Verified, Error> {
    fetch_with(&Network, trust, channel, release, work)
}

/// How a URL's bytes are obtained. **The only difference between transports.**
///
/// Everything above this trait is written once and runs unchanged for a local
/// mirror and for GitHub. The trait exists so that equivalence can be *tested* —
/// both schemes driven through one check, with the bytes supplied from one place —
/// rather than asserted in a comment.
trait Fetcher {
    fn get(&self, url: &str, dest: &mut dyn Write) -> Result<(), Error>;
}

/// The real transports: `file://` and `https://`.
struct Network;

impl Fetcher for Network {
    fn get(&self, url: &str, dest: &mut dyn Write) -> Result<(), Error> {
        match Transport::of(url)? {
            Transport::File => file_get(url, dest),
            #[cfg(feature = "transport")]
            Transport::Https => super::https::get(url, dest),
            #[cfg(not(feature = "transport"))]
            Transport::Https => Err(Error::url(
                url,
                "this build has no TLS transport (built without the `transport` feature), so it \
                 cannot fetch over https",
            )),
        }
    }
}

/// Which transport a URL names. The scheme is the whole decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    File,
    Https,
}

impl Transport {
    fn of(url: &str) -> Result<Self, Error> {
        if url.starts_with("file://") {
            Ok(Self::File)
        } else if url.starts_with("https://") {
            Ok(Self::Https)
        } else {
            let scheme = url.split_once("://").map_or(url, |(scheme, _)| scheme);
            Err(Error::url(
                url,
                format!("unsupported channel scheme {scheme:?} — a channel is file:// or https://"),
            ))
        }
    }
}

fn check_with(
    fetcher: &dyn Fetcher,
    trust: &TrustSet,
    channel: &Channel,
    work: &Path,
) -> Result<Option<Release>, Error> {
    let dir = ensure_dir(work)?;
    let index_path = dir.join(INDEX_FILE);
    let signature_path = verify::signature_path(&index_path);
    // Nothing left by an earlier run may be mistaken for this run's bytes.
    let _ = std::fs::remove_file(&index_path);
    let _ = std::fs::remove_file(&signature_path);

    let index_url = channel.index_url();
    match fetcher.get(&index_url, &mut create(&index_path)?) {
        Ok(()) => {}
        // **An absent index is an empty channel.** Before the first release there
        // is nothing at `latest/download/`, and "no releases yet" is the honest
        // answer — not a failure, and not something to install.
        Err(Error::NotFound { .. }) => return Ok(None),
        Err(e) => return Err(e),
    }

    // The signature travels beside the index, exactly as the release job uploads
    // it. An absent one is *not* an empty channel: the index is there, so this is
    // a release that should have been signed and was not, and the verifier is
    // allowed to say so in its own words (`MissingSignature`) — which is why the
    // empty file this fetch would have created is removed again.
    let signature_url = channel.signature_url(INDEX_FILE);
    match fetcher.get(&signature_url, &mut create(&signature_path)?) {
        Ok(()) => {}
        Err(Error::NotFound { .. }) => {
            let _ = std::fs::remove_file(&signature_path);
        }
        Err(e) => return Err(e),
    }

    // **The door.** From here on the index is trusted bytes, or this returns.
    let verified = verify::verify_with(trust, &index_path, None)?;
    let text = std::fs::read_to_string(&index_path).map_err(|e| Error::io(&index_path, e))?;
    let index: Index = serde_json::from_str(&text).map_err(|e| Error::Index {
        url: index_url.clone(),
        detail: e.to_string(),
    })?;

    let target = host_target();
    let Some(artifact) = index.artifacts.get(target) else {
        return Err(Error::NoArtifact {
            version: index.version,
            target: target.to_string(),
            carries: index
                .artifacts
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
        });
    };
    artifact_name(artifact, &index_url)?;

    Ok(Some(Release {
        version: index.version,
        target: target.to_string(),
        artifact: artifact.clone(),
        key_id: verified.key_id,
        digest: verified.digest,
    }))
}

fn fetch_with(
    fetcher: &dyn Fetcher,
    trust: &TrustSet,
    channel: &Channel,
    release: &Release,
    work: &Path,
) -> Result<Verified, Error> {
    let dir = ensure_dir(work)?;
    artifact_name(&release.artifact, &channel.index_url())?;
    let artifact_path = dir.join(&release.artifact);
    let signature_path = verify::signature_path(&artifact_path);
    let _ = std::fs::remove_file(&artifact_path);
    let _ = std::fs::remove_file(&signature_path);

    fetcher.get(
        &channel.join(&release.artifact),
        &mut create(&artifact_path)?,
    )?;
    match fetcher.get(
        &channel.signature_url(&release.artifact),
        &mut create(&signature_path)?,
    ) {
        Ok(()) => {}
        Err(Error::NotFound { .. }) => {
            // As for the index: an absent signature must read as *absent*, not as
            // an empty file that is not a signature.
            let _ = std::fs::remove_file(&signature_path);
        }
        Err(e) => return Err(e),
    }

    let verified = verify::verify_with(trust, &artifact_path, None)?;
    // A downloaded file has no execute bit, and `stage` refuses a candidate that
    // cannot run — the same rule a `--from` path passes. The *installed* file's
    // mode is still derived from the binary it replaces, so this grants exactly
    // the one bit that gate asks about.
    super::mark_executable(&artifact_path).map_err(|e| Error::io(&artifact_path, e))?;
    Ok(verified)
}

/// The index, as the release job writes it.
///
/// Unknown fields are ignored rather than refused: a future release may add one,
/// and an old client that failed on it would turn a forward-compatible change
/// into an outage. The two fields that exist are the two facts a client cannot
/// derive for itself.
#[derive(Debug, serde::Deserialize)]
struct Index {
    /// The release's version, without a leading `v`.
    version: String,
    /// Artifact file name per target triple.
    artifacts: std::collections::BTreeMap<String, String>,
}

/// An artifact name has to be a name: one component, no separators.
///
/// The index is signed, so a name like `../../etc/passwd` would take a signing key
/// to produce — but the fetched file is written *under* the work directory and
/// then executed, and a path that escapes it is the kind of bug that is only ever
/// found after it is written. Cheap to refuse here instead.
fn artifact_name(name: &str, url: &str) -> Result<(), Error> {
    let ok = !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\');
    if ok {
        Ok(())
    } else {
        Err(Error::Index {
            url: url.to_string(),
            detail: format!("{name:?} is not an artifact file name"),
        })
    }
}

fn ensure_dir(work: &Path) -> Result<&Path, Error> {
    std::fs::create_dir_all(work).map_err(|e| Error::io(work, e))?;
    Ok(work)
}

fn create(path: &Path) -> Result<std::fs::File, Error> {
    std::fs::File::create(path).map_err(|e| Error::io(path, e))
}

/// Read a `file://` URL into `dest`.
fn file_get(url: &str, dest: &mut dyn Write) -> Result<(), Error> {
    let path = file_path(url)?;
    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Error::NotFound {
                url: url.to_string(),
            })
        }
        Err(e) => {
            return Err(Error::Fetch {
                url: url.to_string(),
                detail: e.to_string(),
            })
        }
    };
    std::io::copy(&mut file, dest).map_err(|e| Error::io(&path, e))?;
    Ok(())
}

/// The path a `file://` URL names.
///
/// `file:///home/you/channel/` is the shape: an absolute path, an empty (or
/// `localhost`) authority, and `%XX` escapes decoded — a channel URL that came
/// from an environment variable may well contain a space, and a URL that cannot
/// name the file it points at is not a URL.
fn file_path(url: &str) -> Result<PathBuf, Error> {
    let rest = url
        .strip_prefix("file://")
        .expect("called only for file:// URLs");
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return Err(Error::url(
            url,
            "a file:// channel must name an absolute path (file:///home/you/channel/)",
        ));
    }
    // On Windows the drive letter follows the leading slash: `file:///C:/x` is
    // `C:/x`.
    #[cfg(windows)]
    let rest = rest
        .strip_prefix('/')
        .filter(|r| r.as_bytes().get(1) == Some(&b':'))
        .unwrap_or(rest);

    let bytes = percent_decode(rest, url)?;
    Ok(path_from_bytes(bytes))
}

/// Turn decoded URL bytes into a path, byte-for-byte where the platform allows
/// it.
fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        PathBuf::from(std::ffi::OsString::from_vec(bytes))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from(String::from_utf8_lossy(&bytes).into_owned())
    }
}

/// Decode `%XX` escapes, refusing a malformed one rather than reading a different
/// file than the URL names.
fn percent_decode(text: &str, url: &str) -> Result<Vec<u8>, Error> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let value = bytes
                .get(i + 1..i + 3)
                .and_then(|pair| std::str::from_utf8(pair).ok())
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .ok_or_else(|| {
                    Error::url(url, "a % escape in the URL is not two hexadecimal digits")
                })?;
            out.push(value);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine as _;
    use blake2::Blake2b512;
    use ed25519_dalek::{Signer, SigningKey};
    use sha2::{Digest as _, Sha256};

    /// A throwaway minisign keypair — the same writer as
    /// `tests/update_verify.rs`, which cannot be shared with a unit-test module
    /// without shipping a signer in the library. An acceptance (and a refusal)
    /// can only be *proved* against keys whose secrets are in hand, and the
    /// release key's secret is in a CI secret store and nowhere else.
    struct Throwaway {
        signing: SigningKey,
        key_id: [u8; 8],
    }

    impl Throwaway {
        fn new() -> Self {
            let mut seed = [0u8; 32];
            let mut key_id = [0u8; 8];
            getrandom::fill(&mut seed).expect("OS entropy");
            getrandom::fill(&mut key_id).expect("OS entropy");
            Self {
                signing: SigningKey::from_bytes(&seed),
                key_id,
            }
        }

        fn id(&self) -> String {
            self.key_id
                .iter()
                .rev()
                .map(|byte| format!("{byte:02X}"))
                .collect()
        }

        fn public_key_text(&self) -> String {
            let mut blob = Vec::new();
            blob.extend_from_slice(b"Ed");
            blob.extend_from_slice(&self.key_id);
            blob.extend_from_slice(self.signing.verifying_key().as_bytes());
            format!(
                "untrusted comment: minisign public key {}\n{}\n",
                self.id(),
                BASE64.encode(blob)
            )
        }

        fn trust(&self) -> TrustSet {
            TrustSet::parse(&self.public_key_text()).expect("the generated key parses")
        }

        /// A prehashed signature, the shape `minisign -S` writes by default and
        /// therefore the shape the release job uploads.
        fn sign(&self, content: &[u8], name: &str) -> String {
            let prehash = Blake2b512::digest(content);
            let signature = self.signing.sign(&prehash);
            let trusted = format!("timestamp:1700000000\tfile:{name}\thashed");
            let mut global = signature.to_bytes().to_vec();
            global.extend_from_slice(trusted.as_bytes());
            let global_signature = self.signing.sign(&global);

            let mut blob = Vec::new();
            blob.extend_from_slice(b"ED");
            blob.extend_from_slice(&self.key_id);
            blob.extend_from_slice(&signature.to_bytes());
            format!(
                "untrusted comment: signature from minisign secret key\n{}\ntrusted comment: \
                 {trusted}\n{}\n",
                BASE64.encode(blob),
                BASE64.encode(global_signature.to_bytes())
            )
        }
    }

    /// One test's own directory, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("arreo-channel-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch directory");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        /// A `file://` channel at this directory.
        fn channel(&self) -> Channel {
            Channel::new(format!("file://{}/", self.0.display())).expect("a file channel")
        }

        fn write(&self, name: &str, bytes: impl AsRef<[u8]>) -> PathBuf {
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

    /// The index a release job would write: this platform's artifact, plus one
    /// other target so the map is not a single-entry special case.
    fn index_bytes(version: &str) -> Vec<u8> {
        serde_json::json!({
            "version": version,
            "artifacts": {
                host_target(): "arreo-candidate",
                "aarch64-apple-darwin": "arreo-aarch64-apple-darwin",
            }
        })
        .to_string()
        .into_bytes()
    }

    /// A channel with a signed index and a signed artifact, as the release job
    /// would publish it.
    fn published(scratch: &Scratch, key: &Throwaway, version: &str) -> (Channel, Vec<u8>) {
        let index = index_bytes(version);
        scratch.write(INDEX_FILE, &index);
        scratch.write(
            &format!("{INDEX_FILE}.minisig"),
            key.sign(&index, INDEX_FILE),
        );
        let artifact = b"an arreo binary, in this test only\n".to_vec();
        scratch.write("arreo-candidate", &artifact);
        scratch.write(
            "arreo-candidate.minisig",
            key.sign(&artifact, "arreo-candidate"),
        );
        (scratch.channel(), artifact)
    }

    fn work(scratch: &Scratch) -> PathBuf {
        scratch.path().join("work")
    }

    /// **The accept path.** A signed index is checked, and the version and the
    /// artifact it names come back — which is what `--check` prints.
    #[test]
    fn a_signed_index_reports_the_newest_version_and_its_artifact() {
        let scratch = Scratch::new("accept");
        let key = Throwaway::new();
        let (channel, _) = published(&scratch, &key, "0.1.0");

        let release = check(&key.trust(), &channel, &work(&scratch))
            .expect("a signed index is accepted")
            .expect("and it names a release");
        assert_eq!(release.version, "0.1.0");
        assert_eq!(release.artifact, "arreo-candidate");
        assert_eq!(release.target, host_target());
        assert_eq!(release.key_id, key.id(), "the signing key is reported");
        assert_eq!(
            release.digest,
            format!("{:x}", Sha256::digest(index_bytes("0.1.0"))),
            "the index's own digest is reported, so a channel can be audited"
        );
    }

    /// **The tamper test.** One byte of the index changed, the signature
    /// untouched: the refusal is the verifier's `BadSignature` and it names the
    /// file, so an operator knows what to fetch again.
    #[test]
    fn a_tampered_index_is_refused_as_a_bad_signature_naming_the_file() {
        let scratch = Scratch::new("tamper-index");
        let key = Throwaway::new();
        let (channel, _) = published(&scratch, &key, "0.1.0");

        let mut bytes = index_bytes("0.1.0");
        bytes[10] ^= 0x01;
        scratch.write(INDEX_FILE, &bytes);

        let error = check(&key.trust(), &channel, &work(&scratch)).expect_err("refused");
        assert!(
            matches!(&error, Error::Verify(verify::Error::BadSignature { artifact, .. })
                if artifact.ends_with(INDEX_FILE)),
            "expected BadSignature naming the index, got {error:?}"
        );
        assert!(
            error.to_string().contains("does not authenticate"),
            "the verifier's sentence, verbatim: {error}"
        );
    }

    /// The key id decides, not the signature's validity: a perfectly good
    /// signature from a key this build does not trust is refused as a stranger.
    #[test]
    fn an_index_signed_by_a_stranger_is_refused_by_key_id() {
        let scratch = Scratch::new("stranger");
        let key = Throwaway::new();
        let stranger = Throwaway::new();
        let (channel, _) = published(&scratch, &stranger, "0.1.0");

        let error = check(&key.trust(), &channel, &work(&scratch)).expect_err("refused");
        match &error {
            Error::Verify(verify::Error::UnknownKeyId { found, trusted, .. }) => {
                assert_eq!(found, &stranger.id());
                assert_eq!(trusted, &key.id());
            }
            other => panic!("expected UnknownKeyId, got {other:?}"),
        }
    }

    /// An index with no signature is a **broken release**, not an empty channel:
    /// the two must not be confused, and this is the assertion that keeps them
    /// apart.
    #[test]
    fn an_index_with_no_signature_is_refused_and_names_what_it_looked_for() {
        let scratch = Scratch::new("unsigned-index");
        let key = Throwaway::new();
        scratch.write(INDEX_FILE, index_bytes("0.1.0"));
        let channel = scratch.channel();

        let error = check(&key.trust(), &channel, &work(&scratch)).expect_err("refused");
        assert!(
            matches!(&error, Error::Verify(verify::Error::MissingSignature { path, .. })
                if path.ends_with("arreo-index.json.minisig")),
            "expected MissingSignature naming the index's signature, got {error:?}"
        );
    }

    /// **An empty channel is exit 0's answer**: no index at all means nothing has
    /// been published, which is not an error and not something to install.
    #[test]
    fn an_empty_channel_is_no_releases_not_an_error() {
        let scratch = Scratch::new("empty");
        let key = Throwaway::new();
        let channel = scratch.channel();

        assert_eq!(
            check(&key.trust(), &channel, &work(&scratch)).expect("not an error"),
            None,
            "an empty channel names no release"
        );
    }

    /// The artifact half: a signed artifact is fetched, verified, and left
    /// executable so the same install path `--from` uses will take it.
    #[test]
    fn a_signed_artifact_is_fetched_verified_and_made_runnable() {
        let scratch = Scratch::new("artifact");
        let key = Throwaway::new();
        let (channel, artifact) = published(&scratch, &key, "0.1.0");
        let release = check(&key.trust(), &channel, &work(&scratch))
            .expect("accepted")
            .expect("a release");

        let verified = fetch(&key.trust(), &channel, &release, &work(&scratch))
            .expect("a signed artifact verifies");
        assert_eq!(
            std::fs::read(&verified.artifact).expect("the fetched bytes"),
            artifact,
            "the fetched file is what the channel served"
        );
        assert_eq!(verified.digest, format!("{:x}", Sha256::digest(&artifact)));
        assert!(
            super::super::is_runnable(&verified.artifact),
            "the fetched artifact passes the same gate a --from candidate passes"
        );
    }

    /// A tampered artifact — one flipped byte, the signature carried over — is
    /// refused with the same sentence the verifier gives anywhere else.
    #[test]
    fn a_tampered_artifact_is_refused_as_a_bad_signature() {
        let scratch = Scratch::new("tamper-artifact");
        let key = Throwaway::new();
        let (channel, mut artifact) = published(&scratch, &key, "0.1.0");
        let release = check(&key.trust(), &channel, &work(&scratch))
            .expect("accepted")
            .expect("a release");
        artifact[3] ^= 0x01;
        scratch.write("arreo-candidate", &artifact);

        let error = fetch(&key.trust(), &channel, &release, &work(&scratch)).expect_err("refused");
        assert!(
            matches!(&error, Error::Verify(verify::Error::BadSignature { artifact, .. })
                if artifact.ends_with("arreo-candidate")),
            "expected BadSignature naming the artifact, got {error:?}"
        );
    }

    /// An artifact with no signature at all is refused before anything could be
    /// staged.
    #[test]
    fn an_artifact_with_no_signature_is_refused() {
        let scratch = Scratch::new("unsigned-artifact");
        let key = Throwaway::new();
        let (channel, _) = published(&scratch, &key, "0.1.0");
        let release = check(&key.trust(), &channel, &work(&scratch))
            .expect("accepted")
            .expect("a release");
        let _ = std::fs::remove_file(scratch.path().join("arreo-candidate.minisig"));

        let error = fetch(&key.trust(), &channel, &release, &work(&scratch)).expect_err("refused");
        assert!(
            matches!(&error, Error::Verify(verify::Error::MissingSignature { path, .. })
                if path.ends_with("arreo-candidate.minisig")),
            "expected MissingSignature naming the artifact's signature, got {error:?}"
        );
    }

    /// A release with no build for this machine is refused by name rather than
    /// installing the nearest thing, which would be a binary for another machine.
    #[test]
    fn an_index_without_a_build_for_this_platform_is_refused_by_name() {
        let scratch = Scratch::new("no-target");
        let key = Throwaway::new();
        let index = br#"{"version":"0.1.0","artifacts":{"mips-unknown-none":"arreo-mips"}}"#;
        scratch.write(INDEX_FILE, index);
        scratch.write("arreo-index.json.minisig", key.sign(index, INDEX_FILE));
        let channel = scratch.channel();

        let error = check(&key.trust(), &channel, &work(&scratch)).expect_err("refused");
        assert!(
            matches!(&error, Error::NoArtifact { target, carries, .. }
                if target == host_target() && carries.contains("mips-unknown-none")),
            "the refusal names the target and what the release does carry: {error}"
        );
    }

    /// A verified index that is not an index is refused as such — not silently
    /// treated as an empty channel.
    #[test]
    fn a_verified_index_that_is_not_json_is_refused() {
        let scratch = Scratch::new("not-json");
        let key = Throwaway::new();
        let index = b"<html>this is a release page, not an index</html>";
        scratch.write(INDEX_FILE, index);
        scratch.write("arreo-index.json.minisig", key.sign(index, INDEX_FILE));
        let channel = scratch.channel();

        let error = check(&key.trust(), &channel, &work(&scratch)).expect_err("refused");
        assert!(
            matches!(&error, Error::Index { url, .. } if url.ends_with(INDEX_FILE)),
            "expected an Index refusal naming the index, got {error:?}"
        );
    }

    /// The index is signed, so a traversal in an artifact name would take a key to
    /// produce — but the file is executed after it is downloaded, and a name that
    /// escapes the work directory is refused rather than written.
    #[test]
    fn an_artifact_name_that_is_a_path_is_refused() {
        let scratch = Scratch::new("traversal");
        let key = Throwaway::new();
        let index = format!(
            r#"{{"version":"0.1.0","artifacts":{{"{}":"../../evil"}}}}"#,
            host_target()
        );
        scratch.write(INDEX_FILE, index.as_bytes());
        scratch.write(
            "arreo-index.json.minisig",
            key.sign(index.as_bytes(), INDEX_FILE),
        );
        let channel = scratch.channel();

        let error = check(&key.trust(), &channel, &work(&scratch)).expect_err("refused");
        assert!(
            matches!(&error, Error::Index { detail, .. } if detail.contains("not an artifact")),
            "expected the index to be refused for a path-shaped name, got {error:?}"
        );
    }

    /// A fetcher that serves one set of files whatever URL asks, and records what
    /// it was asked for. Used only by the transport-equivalence test, which needs
    /// an `https://` channel and therefore the TLS feature.
    #[cfg(feature = "transport")]
    struct Memory {
        files: std::collections::BTreeMap<String, Vec<u8>>,
        asked: std::cell::RefCell<Vec<String>>,
    }

    #[cfg(feature = "transport")]
    impl Fetcher for Memory {
        fn get(&self, url: &str, dest: &mut dyn Write) -> Result<(), Error> {
            self.asked.borrow_mut().push(url.to_string());
            let name = url.rsplit('/').next().expect("a file name");
            match self.files.get(name) {
                Some(bytes) => dest
                    .write_all(bytes)
                    .map_err(|e| Error::io(Path::new(url), e)),
                None => Err(Error::NotFound {
                    url: url.to_string(),
                }),
            }
        }
    }

    /// **The transport-agnostic proof.** One channel's content, checked through
    /// the same code under `file://` and under `https://`: the same file names are
    /// requested and the same release comes back. The scheme never reaches
    /// anything but the fetcher.
    #[cfg(feature = "transport")]
    #[test]
    fn file_and_https_channels_differ_only_in_their_fetcher() {
        let scratch = Scratch::new("transports");
        let key = Throwaway::new();
        let (file_channel, _) = published(&scratch, &key, "0.1.0");
        let https_channel =
            Channel::new("https://github.com/ivan-cavero/Arreo/releases/latest/download/")
                .expect("an https channel");

        // The bytes come from the directory the `file://` channel serves,
        // supplied through the fetcher: exactly what a mirror of the same release
        // would do, which is why the two cannot drift.
        let memory = Memory {
            files: std::fs::read_dir(scratch.path())
                .expect("read the published channel")
                .map(|entry| {
                    let entry = entry.expect("entry");
                    (
                        entry.file_name().to_string_lossy().into_owned(),
                        std::fs::read(entry.path()).expect("bytes"),
                    )
                })
                .collect(),
            asked: std::cell::RefCell::new(Vec::new()),
        };

        let from_file = check_with(
            &memory,
            &key.trust(),
            &file_channel,
            &work(&scratch).join("file"),
        )
        .expect("the file channel is checked");
        let asked_file = memory.asked.borrow().clone();
        let from_https = check_with(
            &memory,
            &key.trust(),
            &https_channel,
            &work(&scratch).join("https"),
        )
        .expect("the https channel is checked");

        let names_file: Vec<String> = asked_file
            .iter()
            .map(|url| {
                url.strip_prefix(file_channel.url())
                    .expect("a file URL")
                    .to_string()
            })
            .collect();
        let asked_https = memory.asked.borrow().clone();
        let names_https: Vec<String> = asked_https
            .iter()
            .filter_map(|url| url.strip_prefix(https_channel.url()))
            .map(str::to_string)
            .collect();
        assert_eq!(
            names_file,
            vec![INDEX_FILE, "arreo-index.json.minisig"],
            "the index and its signature are what a check asks for"
        );
        assert_eq!(
            names_file, names_https,
            "the same files are requested from both channels"
        );
        assert_eq!(
            from_file, from_https,
            "one check, two transports, the same answer"
        );
        assert_eq!(
            from_https.expect("a release").artifact,
            "arreo-candidate",
            "and the artifact named is the same one"
        );
    }

    /// The scheme is the whole transport decision, and anything else is refused
    /// by name before a byte is fetched.
    #[test]
    fn the_transport_is_chosen_by_scheme_alone() {
        assert_eq!(Transport::of("file:///tmp/x/").unwrap(), Transport::File);
        assert_eq!(
            Transport::of("https://example.invalid/").unwrap(),
            Transport::Https
        );
        for refused in ["http://example.invalid/", "ftp://host/", "not a url"] {
            let error = Transport::of(refused).expect_err("refused");
            assert!(
                error.to_string().contains("unsupported channel scheme"),
                "{refused}: {error}"
            );
        }
        // A channel is validated when it is built, so a typo is a usage error
        // rather than a surprise at fetch time.
        assert!(Channel::new("http://example.invalid/").is_err());
        assert!(Channel::new("").is_err());
        assert_eq!(
            Channel::new("file:///tmp/rel").unwrap().url(),
            "file:///tmp/rel/",
            "a trailing slash is added, not doubled"
        );
        assert_eq!(
            Channel::new("file:///tmp/rel/").unwrap().index_url(),
            "file:///tmp/rel/arreo-index.json"
        );
    }

    /// The default is this repository's own releases URL: no build ships pointing
    /// at a channel nobody owns.
    #[cfg(feature = "transport")]
    #[test]
    fn the_default_channel_is_this_repositorys_releases_url() {
        assert!(DEFAULT_URL.starts_with("https://github.com/"));
        assert!(DEFAULT_URL.ends_with("/releases/latest/download/"));
        let channel = Channel::new(DEFAULT_URL).expect("the default is a usable channel");
        assert_eq!(channel.index_url(), format!("{DEFAULT_URL}{INDEX_FILE}"));
        assert_eq!(
            channel.signature_url(INDEX_FILE),
            format!("{DEFAULT_URL}{INDEX_FILE}.minisig")
        );
    }

    /// `file://` paths are decoded, not taken literally: a channel directory with
    /// a space in it is a real directory.
    #[test]
    fn a_file_url_is_decoded_and_must_be_absolute() {
        assert_eq!(
            file_path("file:///tmp/a%20b/").expect("decoded"),
            PathBuf::from("/tmp/a b/")
        );
        assert_eq!(
            file_path("file://localhost/tmp/x").expect("decoded"),
            PathBuf::from("/tmp/x")
        );
        assert!(file_path("file://relative/").is_err(), "must be absolute");
        assert!(file_path("file:///tmp/%zz").is_err(), "a bad escape");
    }
}
