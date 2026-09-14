//! The local half of harness config sync (T-0083): presets decide whether a
//! file may travel, the store remembers every version, and the exchange itself
//! is one function the transport will call.
//!
//! One sentence: `push` validates a file and counts a revision, `payload`
//! produces the object a peer receives, `receive` applies it (or keeps both, or
//! refuses by name), and `revert` puts a previous revision back.
//!
//! ## Where the network is
//!
//! Not here. T-0086 owns the transport — the mesh, the relay, the two live
//! machines — and this module deliberately stops at the shape it carries:
//! [`SyncPayload`] is a serialisable object and [`SyncEngine::receive`] takes
//! one. The daemon's `Message::Sync` arm decodes the payload and calls
//! `receive`; the `xtask sync --check` slice calls the same pair twice inside
//! one scratch directory on two isolated roots. One implementation, two ways in.
//!
//! ## The order of the refusals, which is itself a decision
//!
//! 1. **Class.** A LOCAL file is refused because of what it *is*, before it is
//!    read — so `auth.json` is "not syncable", never "dirty", and its contents
//!    are not even opened by this code path.
//! 2. **Deny-list**, for a path no preset knows.
//! 3. **Content-level class rules** (`hooks.state.*.trusted_hash`), still before
//!    the scan, because they are untransferable rather than suspicious.
//! 4. **The scan** (`fixtures::scan_secrets`, reused — never a second scanner).
//! 5. **Fields**: a machine-local field carrying a literal is refused by name.
//! 6. **References**, on the receiving machine only: a name this machine cannot
//!    resolve stops the file and says which name (T-0075 measured that an unset
//!    key otherwise surfaces as a live provider 401).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fixtures::scan_secrets;
use crate::store::{SessionError, SessionStore, SyncRevision};
use crate::sync::keychain::{self, InjectionPlan, KeychainError, SecretStore};
use crate::sync::merge::{self, MergeRefusal, MergeSpec};
use crate::sync::paths::{MachineEnv, PathError};
use crate::sync::presets::{self, Class, DenyRule, Dialect, Harness};
use crate::sync::vectors::{Relation, Vector};

/// A refusal or failure from the sync engine, phrased for an operator.
#[derive(Debug)]
pub enum SyncError {
    /// Neither a preset file nor an absolute path this machine can sync.
    UnknownFile(String),
    /// The file is in a class that never travels, refused before it is read.
    NotSyncable {
        name: String,
        class: Class,
        detail: String,
    },
    /// A user-declared path tripped the LOCAL deny-list.
    Denied {
        name: String,
        rule: DenyRule,
    },
    /// A machine-local field inside a syncable file carries a literal.
    LocalField {
        name: String,
        field: String,
    },
    /// A syncable file names an absolute path, which is one machine's layout.
    AbsolutePath {
        name: String,
        value: String,
    },
    /// A referenced variable is not set on *this* machine.
    Unresolved {
        name: String,
        variable: String,
        machine: String,
    },
    /// The harness would merge a sibling extension, so writing here would put
    /// two configs live at once.
    Sibling {
        name: String,
        sibling: PathBuf,
    },
    /// The scan found secret-shaped content.
    Secrets {
        name: String,
        findings: Vec<String>,
    },
    /// The merge would lose or invent something.
    Merge(MergeRefusal),
    /// The file does not exist where the preset says it does.
    Missing {
        name: String,
        path: PathBuf,
    },
    /// Nothing to merge: no conflict copy and no sibling.
    NothingToMerge {
        name: String,
    },
    /// There is no earlier revision to go back to.
    NoHistory {
        name: String,
    },
    /// The named revision does not exist.
    NoSuchRevision {
        id: i64,
    },
    /// A payload whose bytes do not match the digest it carries.
    Digest {
        name: String,
    },
    /// The payload's bytes are not a document of the file's own format
    /// (review F5): truncated JSON, a YAML file with no mapping. Refused rather
    /// than written, so a bad payload cannot leave the harness unable to start.
    Malformed {
        name: String,
        detail: String,
    },
    /// A payload claims a counter this machine never issued (review F2). A
    /// machine is the sole authority on its own count, so a claim about *this*
    /// machine above what the local store holds is impossible for an honest
    /// sender — it cannot have seen revisions of ours it never received — and
    /// trusting it is how a forged vector becomes a silent overwrite.
    ForgedVector {
        name: String,
        machine: String,
        claimed: u64,
        local: u64,
    },
    Path(PathError),
    Store(SessionError),
    Keychain(KeychainError),
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    Payload(serde_json::Error),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::UnknownFile(name) => write!(
                f,
                "{name}: not a preset file and not an absolute path — presets exist for the \
                 harnesses whose paths were measured (opencode, pi, omp); anything else needs a \
                 path of your own, and a machine will not receive a file it has no preset for"
            ),
            SyncError::NotSyncable {
                name,
                class,
                detail,
            } => write!(f, "{name} is not syncable ({}): {detail}", class.as_str()),
            SyncError::Malformed { name, detail } => write!(
                f,
                "{name}: the payload was refused before anything was written — {detail}; the \
                 live file is unchanged"
            ),
            SyncError::ForgedVector {
                name,
                machine,
                claimed,
                local,
            } => write!(
                f,
                "{name}: the payload claims {machine} is at revision {claimed}, but this machine \
                 has only issued {local} — a machine is the only authority on its own counter, so \
                 the payload is refused rather than trusted"
            ),
            SyncError::Denied { name, rule } => write!(
                f,
                "{name} is not syncable: it is a {} (LOCAL — refused by class, before the scan)",
                rule.as_str()
            ),
            SyncError::LocalField { name, field } => write!(
                f,
                "{name} sets the machine-local field {field} to a literal — that value belongs to \
                 this machine and would overwrite the peer's; make it a reference or remove it"
            ),
            SyncError::AbsolutePath { name, value } => write!(
                f,
                "{name} contains the absolute path {value} — it names one machine's layout, which \
                 is what syncing it would replicate"
            ),
            SyncError::Unresolved {
                name,
                variable,
                machine,
            } => write!(
                f,
                "refused {name}: {variable} is not set on {machine}, and the file references it — \
                 the harness would answer the provider's 401 instead of a config error; set it \
                 with `arreo sync secret set {variable}`"
            ),
            SyncError::Sibling { name, sibling } => write!(
                f,
                "refused {name}: {} exists beside it and this harness merges both, so writing \
                 would put two configs live at once; reconcile with `arreo sync merge {name}`",
                sibling.display()
            ),
            SyncError::Secrets { name, findings } => {
                write!(f, "refused {name}: possible secrets detected:")?;
                for finding in findings {
                    write!(f, "\n  {finding}")?;
                }
                Ok(())
            }
            SyncError::Merge(refusal) => write!(f, "merge refused: {refusal}"),
            SyncError::Missing { name, path } => {
                write!(f, "{name}: no such file ({})", path.display())
            }
            SyncError::NothingToMerge { name } => write!(
                f,
                "{name}: no conflict copy and no sibling extension — nothing to merge"
            ),
            SyncError::NoHistory { name } => {
                write!(f, "{name}: no previous revision in the local store")
            }
            SyncError::NoSuchRevision { id } => write!(f, "no revision {id}"),
            SyncError::Digest { name } => write!(
                f,
                "refused {name}: the payload's bytes do not match the digest it carries"
            ),
            SyncError::Path(e) => write!(f, "{e}"),
            SyncError::Store(e) => write!(f, "{e}"),
            SyncError::Keychain(e) => write!(f, "{e}"),
            SyncError::Io { path, source } => write!(f, "{}: {source}", path.display()),
            SyncError::Payload(e) => write!(f, "payload is not readable JSON: {e}"),
        }
    }
}

impl std::error::Error for SyncError {}

impl From<PathError> for SyncError {
    fn from(e: PathError) -> Self {
        SyncError::Path(e)
    }
}

impl From<SessionError> for SyncError {
    fn from(e: SessionError) -> Self {
        SyncError::Store(e)
    }
}

impl From<KeychainError> for SyncError {
    fn from(e: KeychainError) -> Self {
        SyncError::Keychain(e)
    }
}

impl From<MergeRefusal> for SyncError {
    fn from(e: MergeRefusal) -> Self {
        SyncError::Merge(e)
    }
}

/// A preset file (or a user-declared path) resolved on *this* machine.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The logical name the vectors, the history and the CLI use.
    pub file: String,
    pub harness: Option<Harness>,
    pub dialect: Option<Dialect>,
    pub class: Class,
    pub path: PathBuf,
    pub portable: &'static [&'static str],
    pub local_fields: &'static [&'static str],
    pub union_arrays: &'static [&'static str],
    pub sibling_merges: bool,
}

impl Resolved {
    /// The merge rules this file's preset declares.
    #[must_use]
    pub fn merge_spec(&self) -> MergeSpec {
        MergeSpec::new(self.union_arrays)
    }

    /// The sibling extension this harness would merge alongside the file.
    #[must_use]
    pub fn sibling(&self) -> Option<PathBuf> {
        presets::sibling_path_for(self.sibling_merges, &self.path)
    }
}

/// The object that travels between machines (T-0086 carries it; this defines
/// it).
///
/// The content is in the **neutral** reference form (`${ARREO_ENV:NAME}`), so
/// the bytes on the wire are not any one harness's spelling and the receiving
/// machine writes its own (ROADMAP §3.8, `docs/harness-centralization.md` §3.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncPayload {
    /// The logical file name both machines agree on.
    pub file: String,
    /// The harness the file belongs to: a receiver whose preset disagrees
    /// refuses, because two harnesses' configs are not interchangeable.
    pub harness: String,
    /// The machine that produced this revision.
    pub machine: String,
    /// That machine's vector for the file, after its push.
    pub vector: Vector,
    /// The file's bytes, references in the neutral form.
    pub content: String,
    /// SHA-256 of `content`, for a cheap "same bytes" check on the far side.
    pub digest: String,
}

/// What `push` did.
#[derive(Debug, Clone)]
pub struct PushOutcome {
    pub file: String,
    pub path: PathBuf,
    /// The revision the machine is now at.
    pub counter: u64,
    pub revision: i64,
    /// False when the file was already the newest revision: a second push of an
    /// unchanged file must not create a phantom version every peer would then
    /// fetch.
    pub changed: bool,
    /// Names the file references, in file order.
    pub references: Vec<String>,
    /// Names this machine cannot resolve (a warning on push: the file is
    /// intent, and the machine that must refuse is the one that cannot use it).
    pub missing: Vec<String>,
}

/// What `receive` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiveOutcome {
    /// The incoming revision was newer: the file now holds it.
    Applied {
        file: String,
        path: PathBuf,
        from: String,
        counter: u64,
        revision: i64,
    },
    /// Already the same bytes (or a newer revision): nothing written.
    UpToDate { file: String },
    /// Both machines edited without seeing each other: nothing overwritten, the
    /// incoming copy kept beside the file under the losing machine's name.
    Conflict {
        file: String,
        live: PathBuf,
        copy: PathBuf,
        from: String,
        revision: i64,
    },
}

/// What `revert` did.
#[derive(Debug, Clone)]
pub struct RevertOutcome {
    pub file: String,
    pub path: PathBuf,
    pub revision: i64,
    pub counter: u64,
    /// `true` when the revision was named explicitly (`--to`), `false` when it
    /// was the previous version of the file.
    pub explicit: bool,
}

/// What `merge` did.
#[derive(Debug, Clone)]
pub struct MergeOutcome {
    pub file: String,
    pub path: PathBuf,
    /// The conflict copies and/or sibling that went into the result.
    pub sources: Vec<PathBuf>,
    pub revision: i64,
    pub counter: u64,
}

/// One file's state, for `arreo sync list`/`status`.
#[derive(Debug, Clone)]
pub struct FileStatus {
    pub file: String,
    pub harness: &'static str,
    pub class: Class,
    /// The fields that carry intent (`docs/harness-centralization.md` §2). The
    /// *enforced* rules are [`SyncEngine::validate`]'s; this is the same data
    /// the doc's table holds, printed so an operator can see what travels
    /// without reading the registry.
    pub portable: &'static [&'static str],
    /// `None` when this machine cannot resolve the preset's symbolic path.
    pub path: Option<PathBuf>,
    pub unresolved: Option<String>,
    pub present: bool,
    pub vector: String,
    pub missing: Vec<String>,
    pub conflicts: usize,
}

/// The engine: one machine's store, its paths and its secrets.
///
/// Borrowed rather than owned so a slice can hold two of them at once — the two
/// isolated roots that stand in for two machines — which is the only way a
/// local test can exercise a two-machine exchange.
pub struct SyncEngine<'a> {
    store: &'a SessionStore,
    env: &'a MachineEnv,
    secrets: &'a SecretStore,
    /// **The identity this machine counts its revisions under** (T-0086): its
    /// authenticated device id (`dev_…`), which is what a version vector is
    /// keyed by and what names a conflict copy's loser.
    ///
    /// Not `env.name()`, and that is the decision this field carries. The
    /// machine's *name* is a display label the operator can change
    /// (`arreo machines rename`, T-0043), so keying a counter by it would fork
    /// "old name" from "new name" and make every peer see a concurrent edit from
    /// a machine that did nothing. The device id is stable, unique, `dev_<hex>`
    /// (filesystem-safe, which matters because it lands in a file name), and —
    /// over the mesh — the identity the session *authenticated*, never a string
    /// the peer chose.
    identity: &'a str,
}

impl<'a> SyncEngine<'a> {
    /// `identity` is this machine's device id; see [`SyncEngine::identity`] for
    /// why it is passed rather than read from `env`.
    #[must_use]
    pub fn new(
        store: &'a SessionStore,
        env: &'a MachineEnv,
        secrets: &'a SecretStore,
        identity: &'a str,
    ) -> Self {
        Self {
            store,
            env,
            secrets,
            identity,
        }
    }

    /// The counter key: the identity every vector entry and conflict copy for
    /// this machine is written under.
    #[must_use]
    pub fn machine(&self) -> &str {
        self.identity
    }

    /// The machine's human name, for the sentences an operator reads ("`NAME`
    /// is not set on `workbox`"). Never a counter key.
    #[must_use]
    pub fn display_name(&self) -> &str {
        self.env.name()
    }

    /// Resolve a logical name (or a path of the operator's own) on this machine.
    ///
    /// The class check comes first and the path second: a LOCAL file must be
    /// refused even when the machine cannot resolve where it would have been,
    /// and `auth.json` must be refused as "not syncable" rather than as "dirty".
    pub fn resolve(&self, target: &str) -> Result<Resolved, SyncError> {
        if let Some((preset, entry)) = presets::file(target) {
            if entry.class != Class::Sync {
                return Err(SyncError::NotSyncable {
                    name: entry.name.to_string(),
                    class: entry.class,
                    detail: match entry.class {
                        Class::Local => {
                            "credentials, session state, caches and logs are this machine's; the \
                             sync engine never reads it"
                                .to_string()
                        }
                        Class::Project => {
                            "it belongs to a repository and travels with git; syncing it would \
                             fight the repo"
                                .to_string()
                        }
                        Class::Sync => String::new(),
                    },
                });
            }
            let path = self.env.resolve(entry.path)?;
            return Ok(Resolved {
                file: entry.name.to_string(),
                harness: Some(preset.harness),
                dialect: Some(preset.dialect),
                class: entry.class,
                path,
                portable: entry.portable,
                local_fields: entry.local_fields,
                union_arrays: entry.union_arrays,
                sibling_merges: entry.sibling_merges,
            });
        }
        // A path of the operator's own: still registered nowhere, so the
        // deny-list is the only class it has, and the scan is the only
        // protection after it (`docs/harness-centralization.md` §2, last
        // section).
        let path = PathBuf::from(target);
        if !path.is_absolute() {
            return Err(SyncError::UnknownFile(target.to_string()));
        }
        if path.is_dir() {
            return Err(SyncError::NotSyncable {
                name: target.to_string(),
                class: Class::Local,
                detail: "a directory: §3.8 rejects folder sync, because a surprise overwrite of a \
                         whole tree is not an opt-in per file"
                    .to_string(),
            });
        }
        if let Some(rule) = presets::deny_path(&path) {
            return Err(SyncError::Denied {
                name: target.to_string(),
                rule,
            });
        }
        let file = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(target)
            .to_string();
        Ok(Resolved {
            file,
            harness: None,
            dialect: None,
            class: Class::Sync,
            path,
            portable: &[],
            local_fields: &[],
            union_arrays: &[],
            sibling_merges: false,
        })
    }

    /// Read a file's content, naming the file when it is missing.
    fn read(&self, resolved: &Resolved) -> Result<String, SyncError> {
        match std::fs::read_to_string(&resolved.path) {
            Ok(text) => Ok(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(SyncError::Missing {
                name: resolved.file.clone(),
                path: resolved.path.clone(),
            }),
            Err(e) => Err(SyncError::Io {
                path: resolved.path.clone(),
                source: e,
            }),
        }
    }

    /// Every check between "this file exists" and "these bytes may travel".
    fn validate(&self, resolved: &Resolved, content: &str) -> Result<(), SyncError> {
        if let Some(rule) = presets::deny_content(content) {
            return Err(SyncError::Denied {
                name: resolved.file.clone(),
                rule,
            });
        }
        if let Some(value) = absolute_path_in(content) {
            return Err(SyncError::AbsolutePath {
                name: resolved.file.clone(),
                value,
            });
        }
        // The scan is `fixtures::scan_secrets`, reused and never re-implemented.
        // The *view* it reads is the file with any neutral reference written in
        // a harness spelling, because the scanner's reference predicate is what
        // an operator's `${ARREO_ENV:X}` has to be recognised as — the file on
        // disk is untouched, and for a file already in its own dialect this is
        // the identity.
        let scannable = keychain::denormalize(
            content,
            resolved.dialect.unwrap_or(Dialect::OpencodeEnvBrace),
        );
        let findings = scan_secrets(&scannable);
        if !findings.is_empty() {
            return Err(SyncError::Secrets {
                name: resolved.file.clone(),
                findings,
            });
        }
        for field in resolved.local_fields {
            if let Some(field) = literal_local_field(content, field) {
                return Err(SyncError::LocalField {
                    name: resolved.file.clone(),
                    field,
                });
            }
        }
        Ok(())
    }

    /// Validate a file and count a revision for it — the local half of a
    /// publish. The transport (T-0086) sends what [`SyncEngine::payload`]
    /// builds from this.
    pub fn push(&self, target: &str) -> Result<PushOutcome, SyncError> {
        let resolved = self.resolve(target)?;
        let content = self.read(&resolved)?;
        self.validate(&resolved, &content)?;
        let references = keychain::references(&content);
        let plan = keychain::plan(&content, self.secrets, self.env.name());
        let mut vector = self.store.sync_vector(&resolved.file)?;
        let digest = digest_of(content.as_bytes());
        let newest = self.newest_digest(&resolved.file)?;
        if newest.as_deref() == Some(digest.as_str()) {
            let revision = self
                .store
                .sync_revisions(&resolved.file)?
                .first()
                .map_or(0, |r| r.id);
            return Ok(PushOutcome {
                file: resolved.file,
                path: resolved.path,
                counter: vector.get(self.identity),
                revision,
                changed: false,
                references,
                missing: plan.missing().to_vec(),
            });
        }
        let counter = vector.bump(self.identity);
        self.store
            .sync_set_counter(&resolved.file, self.identity, counter)?;
        let revision = self.store.sync_record_revision(
            &resolved.file,
            self.identity,
            counter,
            "push",
            content.as_bytes(),
            now_ms(),
        )?;
        Ok(PushOutcome {
            file: resolved.file,
            path: resolved.path,
            counter,
            revision,
            changed: true,
            references,
            missing: plan.missing().to_vec(),
        })
    }

    /// The payload a peer receives: the newest revision, references neutralised
    /// so the bytes on the wire are no harness's spelling.
    pub fn payload(&self, target: &str) -> Result<SyncPayload, SyncError> {
        self.push(target)?;
        let resolved = self.resolve(target)?;
        let content = self.read(&resolved)?;
        let neutral = keychain::normalize(&content);
        Ok(SyncPayload {
            file: resolved.file.clone(),
            harness: resolved
                .harness
                .map(Harness::id)
                .unwrap_or("custom")
                .to_string(),
            machine: self.identity.to_string(),
            vector: self.store.sync_vector(&resolved.file)?,
            digest: digest_of(neutral.as_bytes()),
            content: neutral,
        })
    }

    /// Apply a peer's payload: the one door a payload may come through, whether
    /// it arrived as a file (`arreo sync apply`) or over the mesh (T-0086's
    /// `Message::Sync`, which calls this function rather than re-implementing
    /// any part of it).
    ///
    /// `payload.machine` is the **sender's authenticated identity** — the daemon
    /// sets it from the Noise handshake before calling here, and refuses a
    /// payload whose own claim disagrees — so everything below that treats it as
    /// the counter's authority is treating an authenticated fact as one.
    pub fn receive(&self, payload: &SyncPayload) -> Result<ReceiveOutcome, SyncError> {
        // The caller authenticated the sender; this check is about the bytes in
        // hand, and it is the last place they exist before they are written.
        if digest_of(payload.content.as_bytes()) != payload.digest {
            return Err(SyncError::Digest {
                name: payload.file.clone(),
            });
        }
        let resolved = self.resolve(&payload.file)?;
        // A machine never receives a file it cannot use: the preset is what
        // makes the bytes mean something here, and two harnesses' configs are
        // not interchangeable, so a disagreement about the harness is a refusal.
        match resolved.harness {
            Some(harness) if harness.id() == payload.harness => {}
            Some(harness) => {
                return Err(SyncError::NotSyncable {
                    name: payload.file.clone(),
                    class: resolved.class,
                    detail: format!(
                        "the payload is for {} and this machine's preset for {} is {}",
                        payload.harness,
                        payload.file,
                        harness.id()
                    ),
                })
            }
            None => {
                return Err(SyncError::NotSyncable {
                    name: payload.file.clone(),
                    class: resolved.class,
                    detail: "this machine has no preset for it".to_string(),
                })
            }
        }
        let Some(dialect) = resolved.dialect else {
            return Err(SyncError::NotSyncable {
                name: payload.file.clone(),
                class: resolved.class,
                detail: "this machine has no preset for it, so no dialect to write".to_string(),
            });
        };
        let content = keychain::denormalize(&payload.content, dialect);
        self.validate(&resolved, &content)?;
        // The refusal that closes the scanner's residual: a name this machine
        // cannot resolve stops the file and says which name. Without it the file
        // lands and the harness fails with the provider's own error.
        let plan = keychain::plan(&content, self.secrets, self.env.name());
        if let Some(variable) = plan.missing().first() {
            return Err(SyncError::Unresolved {
                name: resolved.file.clone(),
                variable: variable.clone(),
                machine: self.env.name().to_string(),
            });
        }
        // **Content validity is a refusal, not a repair** (review F5). The digest
        // proves the bytes are the ones the sender sent; it says nothing about
        // whether they are a config. A truncated payload (or a sender bug) would
        // otherwise land verbatim and leave the harness failing at startup — the
        // outcome `write_atomic`'s own note calls "worse than an old one" — with
        // the previous good bytes surviving only in this machine's history.
        // Refusing keeps the live file as it was, which is the whole point of a
        // sync that never loses an edit.
        validate_format(&resolved.file, &content)?;
        let current = self.read(&resolved).ok();
        let mut vector = self.store.sync_vector(&resolved.file)?;
        // **Only the sender's own counter is trusted** (review F2). A version
        // vector is per-machine, and a machine is the sole authority on its own
        // count — so `payload.vector`'s entries for third machines are hearsay
        // and its entry for *this* machine is a claim we can check against our
        // own store. Without this, one forged field turned keep-both into a
        // silent overwrite (a payload claiming this machine was at counter N made
        // the live file look stale) and a second pinned a real machine's counter
        // so its genuine payloads were refused as "already up to date" for ever.
        let ours = self.identity.to_string();
        // `get` is 0 for a machine the vector does not mention, and a counter is
        // bumped from 0, so "absent" can never exceed what this machine issued.
        //
        // A payload from *this* machine is its own bytes coming back (a replay, a
        // round trip, an `arreo sync payload` piped into `apply` on the same
        // box), so there is no third party to forge and the claim is checked
        // against nothing: the local store may legitimately have been reset while
        // the payload was in flight.
        if payload.machine != ours {
            let claimed = payload.vector.get(&ours);
            let local = vector.get(&ours);
            if claimed > local {
                return Err(SyncError::ForgedVector {
                    name: resolved.file.clone(),
                    machine: ours.clone(),
                    claimed,
                    local,
                });
            }
        }
        if current.as_deref() == Some(content.as_str()) {
            self.absorb(
                &resolved.file,
                &payload.machine,
                &payload.vector,
                &mut vector,
            )?;
            return Ok(ReceiveOutcome::UpToDate {
                file: resolved.file,
            });
        }
        // **The bytes on disk are checked against the history before anything
        // is written.** A version vector counts *published* revisions, so an
        // edit made here and not yet pushed is invisible to it — and a payload
        // that is merely newer would overwrite that edit with no copy anywhere.
        // Nothing about this sync is last-writer-wins, so a live file that is
        // not the newest recorded revision takes the keep-both path regardless
        // of which vector is ahead.
        let recorded = self.newest_bytes(&resolved.file)?;
        let live_is_recorded = match (&current, &recorded) {
            (Some(live), Some(newest)) => live.as_bytes() == newest.as_slice(),
            // No live file: applying creates it, which destroys nothing.
            (None, _) => true,
            // A live file the history has never seen (edited before the first
            // push, or a hand edit): treat it as an edit worth keeping.
            (Some(_), None) => false,
        };
        // The decision still reads the sender's whole vector — that is its
        // honest view of the world, and dropping its third-machine entries here
        // would lose the concurrency signal (a sender that has *not* seen our
        // latest revision is what makes a payload concurrent). What the check
        // above forbids is a claim about our own counter that we never issued;
        // what `absorb` refuses is recording hearsay about third machines. A
        // sender may still lie about its *own* counter, which suppresses only its
        // own future updates — self-harm, not a cross-machine attack.
        match vector.relation(&payload.vector) {
            // The sender is behind: this machine holds newer or equal published
            // revisions, so the payload is not news.
            Relation::Newer | Relation::Same if live_is_recorded => {
                self.absorb(
                    &resolved.file,
                    &payload.machine,
                    &payload.vector,
                    &mut vector,
                )?;
                Ok(ReceiveOutcome::UpToDate {
                    file: resolved.file,
                })
            }
            Relation::Older if live_is_recorded => {
                self.check_sibling(&resolved)?;
                write_atomic(&resolved.path, content.as_bytes())?;
                let counter = payload.vector.get(&payload.machine);
                let revision = self.store.sync_record_revision(
                    &resolved.file,
                    &payload.machine,
                    counter,
                    "sync",
                    content.as_bytes(),
                    now_ms(),
                )?;
                self.absorb(
                    &resolved.file,
                    &payload.machine,
                    &payload.vector,
                    &mut vector,
                )?;
                Ok(ReceiveOutcome::Applied {
                    file: resolved.file,
                    path: resolved.path,
                    from: payload.machine.clone(),
                    counter,
                    revision,
                })
            }
            // Keep both. The live file is not touched (nothing here is
            // last-writer-wins), and the incoming copy is kept under the losing
            // machine's name and the instant, so "which one is which" is in the
            // file name.
            _ => {
                self.check_sibling(&resolved)?;
                let copy = merge::conflict_path(&resolved.path, &payload.machine, now_ms());
                if let Some(parent) = copy.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| SyncError::Io {
                        path: parent.to_path_buf(),
                        source: e,
                    })?;
                }
                write_atomic(&copy, content.as_bytes())?;
                let counter = payload.vector.get(&payload.machine);
                let revision = self.store.sync_record_revision(
                    &resolved.file,
                    &payload.machine,
                    counter,
                    "conflict",
                    content.as_bytes(),
                    now_ms(),
                )?;
                Ok(ReceiveOutcome::Conflict {
                    file: resolved.file,
                    live: resolved.path,
                    copy,
                    from: payload.machine.clone(),
                    revision,
                })
            }
        }
    }

    /// Take **the sender's own counter** into this machine's vector (review F2).
    ///
    /// Only `from`'s entry moves: it is the one counter the sender is the
    /// authority on. A payload's claims about other machines are dropped rather
    /// than recorded — recording them let one payload pin a third machine's
    /// counter and suppress its later, genuine revisions.
    fn absorb(
        &self,
        file: &str,
        from: &str,
        incoming: &Vector,
        vector: &mut Vector,
    ) -> Result<(), SyncError> {
        let before = vector.get(from);
        vector.set(from, before.max(incoming.get(from)));
        if vector.get(from) != before {
            self.store.sync_set_counter(file, from, vector.get(from))?;
        }
        Ok(())
    }

    /// Refuse to write when the harness would merge a sibling extension.
    fn check_sibling(&self, resolved: &Resolved) -> Result<(), SyncError> {
        if let Some(sibling) = resolved.sibling() {
            if sibling.exists() {
                return Err(SyncError::Sibling {
                    name: resolved.file.clone(),
                    sibling,
                });
            }
        }
        Ok(())
    }

    /// A file's revisions, newest first.
    pub fn history(&self, target: &str) -> Result<Vec<SyncRevision>, SyncError> {
        let resolved = self.resolve(target)?;
        Ok(self.store.sync_revisions(&resolved.file)?)
    }

    /// Put a previous revision back on disk.
    ///
    /// Without `to`, the newest revision that differs from what is on disk —
    /// "undo the last sync". The revert is itself a revision and it bumps the
    /// counter, so it propagates like any other edit and a second revert undoes
    /// the undo.
    pub fn revert(&self, target: &str, to: Option<i64>) -> Result<RevertOutcome, SyncError> {
        let resolved = self.resolve(target)?;
        let revisions = self.store.sync_revisions(&resolved.file)?;
        if revisions.is_empty() {
            return Err(SyncError::NoHistory {
                name: resolved.file,
            });
        }
        let current = self.read(&resolved).ok();
        let current_digest = current.as_ref().map(|text| digest_of(text.as_bytes()));
        let revision = match to {
            Some(id) => revisions
                .iter()
                .find(|r| r.id == id)
                .ok_or(SyncError::NoSuchRevision { id })?,
            None => {
                let mut found = None;
                for candidate in &revisions {
                    let content = self.store.sync_content(candidate.id)?;
                    let same = content
                        .as_deref()
                        .map(|bytes| current_digest.as_deref() == Some(digest_of(bytes).as_str()))
                        .unwrap_or(false);
                    if !same {
                        found = Some(candidate);
                        break;
                    }
                }
                found.ok_or(SyncError::NoHistory {
                    name: resolved.file.clone(),
                })?
            }
        };
        let bytes = self
            .store
            .sync_content(revision.id)?
            .ok_or(SyncError::NoSuchRevision { id: revision.id })?;
        // No sibling gate on an undo: the file already exists and opencode is
        // already merging whatever sits beside it — that state predates the
        // revert, and the gate exists for writes that *introduce* a second live
        // config (a receive), not for putting back what was there.
        write_atomic(&resolved.path, &bytes)?;
        let mut vector = self.store.sync_vector(&resolved.file)?;
        let counter = vector.bump(self.identity);
        self.store
            .sync_set_counter(&resolved.file, self.identity, counter)?;
        let revision = self.store.sync_record_revision(
            &resolved.file,
            self.identity,
            counter,
            "revert",
            &bytes,
            now_ms(),
        )?;
        Ok(RevertOutcome {
            file: resolved.file,
            path: resolved.path,
            revision,
            counter,
            explicit: to.is_some(),
        })
    }

    /// The conflict copies kept beside a file, oldest first.
    pub fn conflicts(&self, target: &str) -> Result<Vec<PathBuf>, SyncError> {
        let resolved = self.resolve(target)?;
        let Some(parent) = resolved.path.parent() else {
            return Ok(Vec::new());
        };
        let mut copies: Vec<PathBuf> = Vec::new();
        let entries = match std::fs::read_dir(parent) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(SyncError::Io {
                    path: parent.to_path_buf(),
                    source: e,
                })
            }
        };
        for entry in entries {
            let entry = entry.map_err(|e| SyncError::Io {
                path: parent.to_path_buf(),
                source: e,
            })?;
            let path = entry.path();
            if merge::is_conflict_copy(&resolved.path, &path) {
                copies.push(path);
            }
        }
        copies.sort();
        Ok(copies)
    }

    /// Reconcile a file with its conflict copies and its sibling extension.
    ///
    /// This is the one operation that *writes* a merge, because it is the only
    /// one an operator asked for by name. Conflict copies are consumed on
    /// success — their bytes are already in the history (recorded under reason
    /// `conflict` when they were written), so nothing is lost by removing the
    /// file, and leaving them behind would make the next merge re-apply them.
    pub fn merge(&self, target: &str) -> Result<MergeOutcome, SyncError> {
        let resolved = self.resolve(target)?;
        let spec = resolved.merge_spec();
        let live = self.read(&resolved)?;
        let mut merged = live;
        let mut sources: Vec<PathBuf> = Vec::new();
        let copies = self.conflicts(target)?;
        for copy in &copies {
            let incoming = read_file(copy)?;
            merged = merge::merge_json(&merged, &incoming, &spec)?;
            sources.push(copy.clone());
        }
        let sibling = resolved.sibling().filter(|path| path.exists());
        if let Some(sibling) = &sibling {
            let incoming = read_file(sibling)?;
            merged = merge::merge_json(&merged, &incoming, &spec)?;
            sources.push(sibling.clone());
        }
        if sources.is_empty() {
            return Err(SyncError::NothingToMerge {
                name: resolved.file,
            });
        }
        // The sibling is renamed aside **before** the merged file is written, so
        // the two configs are never both live: a crash between the two steps
        // leaves the operator's sibling under a name they can move back, which
        // is recoverable, while the reverse order would leave two live configs
        // and no error to explain the harness's behaviour.
        if let Some(sibling) = &sibling {
            let aside = sibling.with_file_name(format!(
                "{}.reconciled-{}",
                sibling
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("sibling"),
                merge::compact_utc_ms(now_ms())
            ));
            std::fs::rename(sibling, &aside).map_err(|e| SyncError::Io {
                path: sibling.clone(),
                source: e,
            })?;
        }
        write_atomic(&resolved.path, merged.as_bytes())?;
        let mut vector = self.store.sync_vector(&resolved.file)?;
        let counter = vector.bump(self.identity);
        self.store
            .sync_set_counter(&resolved.file, self.identity, counter)?;
        let revision = self.store.sync_record_revision(
            &resolved.file,
            self.identity,
            counter,
            "merge",
            merged.as_bytes(),
            now_ms(),
        )?;
        for copy in &copies {
            let _ = std::fs::remove_file(copy);
        }
        Ok(MergeOutcome {
            file: resolved.file,
            path: resolved.path,
            sources,
            revision,
            counter,
        })
    }

    /// What this machine needs for a file and which of those names it lacks.
    pub fn injection(&self, target: &str) -> Result<(Resolved, InjectionPlan), SyncError> {
        let resolved = self.resolve(target)?;
        let content = self.read(&resolved)?;
        self.validate(&resolved, &content)?;
        let plan = keychain::plan(&content, self.secrets, self.env.name());
        Ok((resolved, plan))
    }

    /// Every syncable preset file, with this machine's view of each.
    ///
    /// A listing rather than a decision: a file whose root this machine cannot
    /// resolve is reported as unresolved instead of failing the whole call, so
    /// one missing variable does not hide the other five files.
    pub fn status(&self) -> Result<Vec<FileStatus>, SyncError> {
        let mut out = Vec::new();
        for (preset, entry) in presets::files() {
            if entry.class != Class::Sync {
                continue;
            }
            let vector = self.store.sync_vector(entry.name)?;
            let (path, unresolved) = match self.env.resolve(entry.path) {
                Ok(path) => (Some(path), None),
                Err(e) => (None, Some(e.to_string())),
            };
            let mut present = false;
            let mut missing = Vec::new();
            let mut conflicts = 0;
            if let Some(path) = &path {
                if let Ok(content) = std::fs::read_to_string(path) {
                    present = true;
                    let plan = keychain::plan(&content, self.secrets, self.env.name());
                    missing = plan.missing().to_vec();
                }
                if let Some(parent) = path.parent() {
                    if let Ok(entries) = std::fs::read_dir(parent) {
                        conflicts = entries
                            .filter_map(Result::ok)
                            .filter(|e| merge::is_conflict_copy(path, &e.path()))
                            .count();
                    }
                }
            }
            out.push(FileStatus {
                file: entry.name.to_string(),
                harness: preset.harness.id(),
                portable: entry.portable,
                class: entry.class,
                path,
                unresolved,
                present,
                vector: vector.summary(),
                missing,
                conflicts,
            });
        }
        Ok(out)
    }

    /// The digest of the newest revision on record, if any.
    fn newest_digest(&self, file: &str) -> Result<Option<String>, SyncError> {
        Ok(self.newest_bytes(file)?.map(|bytes| digest_of(&bytes)))
    }

    /// The bytes of the newest revision on record, if any.
    ///
    /// The comparison `receive` makes is against *this*, not against the digest:
    /// "is the file on disk one of the revisions we know about" is the question
    /// that decides whether a write could destroy something.
    fn newest_bytes(&self, file: &str) -> Result<Option<Vec<u8>>, SyncError> {
        let revisions = self.store.sync_revisions(file)?;
        let Some(newest) = revisions.first() else {
            return Ok(None);
        };
        self.store.sync_content(newest.id).map_err(SyncError::from)
    }
}

/// Read a file this crate wrote (a conflict copy or a sibling), naming it.
/// Does `content` parse as the format `file` claims to be?
///
/// Deliberately shallow — a JSON/JSONC document must parse as JSON once comments
/// are removed, a YAML document must have at least one `key:` line — because the
/// alternative is a full schema for three harnesses, and the failure this
/// prevents is *truncation*, not a wrong-but-well-formed config. A well-formed
/// config that says the wrong thing is the operator's business (and the merge
/// rules handle the fields that matter); a half-written file is nobody's.
fn validate_format(file: &str, content: &str) -> Result<(), SyncError> {
    let extension = std::path::Path::new(file)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let refused = |detail: String| SyncError::Malformed {
        name: file.to_string(),
        detail,
    };
    match extension.as_str() {
        "json" | "jsonc" => {
            let stripped = crate::sync::merge::strip_jsonc_comments(content);
            serde_json::from_str::<serde_json::Value>(&stripped)
                .map(|_| ())
                .map_err(|e| refused(format!("it does not parse as JSON: {e}")))
        }
        "yml" | "yaml" => {
            let has_mapping = content.lines().any(|line| {
                let line = line.trim_start();
                !line.is_empty()
                    && !line.starts_with('#')
                    && !line.starts_with('-')
                    && line.contains(':')
            });
            if has_mapping {
                Ok(())
            } else {
                Err(refused(
                    "it has no `key: value` line, so it is not a YAML mapping".to_string(),
                ))
            }
        }
        // An unknown extension is not this function's to judge: the preset
        // registry only knows these three, and a user-declared path has no
        // declared format.
        _ => Ok(()),
    }
}

fn read_file(path: &Path) -> Result<String, SyncError> {
    std::fs::read_to_string(path).map_err(|e| SyncError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Write through a temporary file in the same directory, then rename.
///
/// A half-written config is worse than an old one: the harness reads the file
/// at startup and would fail on truncated JSON, and the sync's own retry would
/// then race the operator's editor. `rename` in one directory is atomic on
/// every filesystem this product ships on.
fn write_atomic(path: &Path, content: &[u8]) -> Result<(), SyncError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| SyncError::Io {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let mut name = path
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .unwrap_or_default();
    name.push(format!(".sync-tmp-{}", std::process::id()));
    let temporary = path.with_file_name(name);
    std::fs::write(&temporary, content).map_err(|e| SyncError::Io {
        path: temporary.clone(),
        source: e,
    })?;
    std::fs::rename(&temporary, path).map_err(|e| SyncError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

/// The field name when `field` is assigned a literal rather than a reference.
fn literal_local_field(content: &str, field: &str) -> Option<String> {
    for line in content.lines() {
        let Some(separator) = line.find([':', '=']) else {
            continue;
        };
        let key = line[..separator].trim().trim_matches(['"', '\'']);
        if key != field {
            continue;
        }
        let value = line[separator + 1..]
            .trim()
            .trim_end_matches(',')
            .trim_matches(['"', '\'']);
        if value.is_empty() {
            continue;
        }
        if keychain::reference_name(value).is_none() {
            return Some(field.to_string());
        }
    }
    None
}

/// An absolute path in a value, which is one machine's layout.
///
/// The rule is `docs/harness-centralization.md` §1's third clause: portable
/// intent may not carry a path that only resolves on the machine that wrote it.
/// Deliberately narrow — a value that starts with `/` or `~/` and has another
/// separator after it, or a Windows drive form — because a single `/` is a URL
/// path (`https://x/v1`) and refusing those would refuse the configs this
/// feature exists for.
fn absolute_path_in(content: &str) -> Option<String> {
    for (start, end) in keychain::value_spans(content) {
        let value = &content[start..end];
        let windows = value.len() > 2
            && value.as_bytes()[0].is_ascii_alphabetic()
            && value.as_bytes()[1] == b':'
            && (value.as_bytes()[2] == b'\\' || value.as_bytes()[2] == b'/');
        let unix = value.starts_with("~/") || (value.starts_with('/') && value[1..].contains('/'));
        if windows || unix {
            return Some(value.to_string());
        }
    }
    None
}

/// SHA-256 of the bytes, hex — the cheap identity of a revision.
fn digest_of(bytes: &[u8]) -> String {
    crate::identity::keys::hex(&Sha256::digest(bytes))
}

/// Milliseconds since the epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::paths::Os;
    use crate::sync::presets;

    /// The variable name every test uses. Deliberately one no environment can
    /// hold: the keychain falls back to the process environment, and a test that
    /// silently resolved a variable from the developer's shell would pass for
    /// the wrong reason.
    const KEY: &str = "ARREO_T83_KEY";

    /// One machine: its own roots, its own store, its own secrets, its own
    /// **device identity**.
    ///
    /// Two of these in one process is the whole point — a two-machine exchange
    /// with no network — which is why [`SyncEngine`] borrows all of them rather
    /// than owning a global. The identity is a key derived from a fixed seed
    /// (T-0086): the counter key is a device id, so a test that hardcoded one
    /// would be asserting against a number it could not predict, and a test that
    /// reused one machine's id for both roots would be testing one machine.
    struct Root {
        dir: PathBuf,
        env: MachineEnv,
        store: SessionStore,
        secrets: SecretStore,
        identity: String,
    }

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "arreo-sync-{tag}-{}-{}",
            std::process::id(),
            now_ms()
        ));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    impl Root {
        fn new(tag: &str, name: &str) -> Self {
            let dir = scratch(tag);
            let env = MachineEnv::new(name, Os::Linux, dir.join("home"))
                .with_config_home(dir.join("cfg"))
                .with_pi_agent_dir(dir.join("agent"));
            let secrets = SecretStore::open(dir.join("secrets.json")).expect("secrets");
            let store = SessionStore::open_memory().expect("store");
            // The device identity, derived from the fixture's own tag so the id
            // is stable across a run and different between two roots. **The
            // derivation is the fixture's, not the product's**: a real machine
            // reads its id from `identity/device.key` (T-0086) and never from a
            // name — deriving an identity from a renameable label is the exact
            // mistake the counter-key decision exists to prevent.
            let seed = tag
                .bytes()
                .fold(0u8, |acc, byte| acc.wrapping_mul(31).wrapping_add(byte))
                | 1;
            let key = crate::identity::DeviceKey::from_seed([seed; 32]);
            let identity = crate::identity::DeviceId::from_key(&key.public()).display_id();
            Self {
                dir,
                env,
                store,
                secrets,
                identity,
            }
        }

        fn engine(&self) -> SyncEngine<'_> {
            SyncEngine::new(&self.store, &self.env, &self.secrets, &self.identity)
        }

        /// The id this machine's revisions are counted under — the device id,
        /// never the display name (T-0086).
        fn id(&self) -> &str {
            &self.identity
        }

        /// The resolved destination of a preset file, on this machine.
        fn path(&self, file: &str) -> PathBuf {
            let (_, entry) = presets::file(file).unwrap_or_else(|| panic!("preset {file}"));
            self.env.resolve(entry.path).expect("resolved")
        }

        fn write(&self, file: &str, content: &str) -> PathBuf {
            let path = self.path(file);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("dir");
            }
            std::fs::write(&path, content).expect("write");
            path
        }

        fn read(&self, file: &str) -> String {
            std::fs::read_to_string(self.path(file)).expect("read")
        }

        fn set_secret(&mut self, name: &str, value: &str) {
            self.secrets.set(name, value).expect("set secret");
        }
    }

    impl Drop for Root {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A device id for a machine this fixture never builds: the sender of a
    /// hand-built payload.
    ///
    /// **Even-seeded, so it can never collide with a [`Root`]'s** (those fold a
    /// tag into an odd seed). That matters: a payload whose `machine` is the
    /// *receiver's* own id is the replay case, which legitimately skips the
    /// vector check — a test that meant to exercise the forgery path would
    /// silently stop exercising it.
    fn other_id() -> String {
        let key = crate::identity::DeviceKey::from_seed([0xE6; 32]);
        crate::identity::DeviceId::from_key(&key.public()).display_id()
    }

    fn worked_case() -> String {
        // The owner's file, with a test-only variable name so a shell that
        // happens to export the real one cannot make a test pass.
        crate::sync::EXAMPLE_PROVIDER.replace("VBK_PROD_KEY", KEY)
    }

    /// Two machines, one of which has the key and one of which does not — the
    /// §3.8 story from `docs/harness-centralization.md` §4, told locally.
    #[test]
    fn the_owner_case_converges_across_two_roots_that_each_resolve_the_reference_itself() {
        let mut alpha = Root::new("case-alpha", "alpha-machine");
        let mut beta = Root::new("case-beta", "beta-machine");
        alpha.set_secret(KEY, "alpha-value");
        alpha.write("opencode.jsonc", &worked_case());

        // Step 1: the edit, on alpha only.
        let pushed = alpha.engine().push("opencode.jsonc").expect("push");
        assert!(pushed.changed);
        assert_eq!(pushed.counter, 1, "alpha's first revision");
        assert_eq!(pushed.references, vec![KEY.to_string()]);
        assert!(pushed.missing.is_empty(), "alpha has its own key");

        // Step 2: the payload carries the neutral form — no key, no
        // harness-specific spelling.
        let payload = alpha.engine().payload("opencode.jsonc").expect("payload");
        assert!(payload.content.contains("${ARREO_ENV:ARREO_T83_KEY}"));
        assert!(!payload.content.contains("alpha-value"));
        assert_eq!(payload.machine, alpha.id());

        // Step 3: beta has the file's *reference* but not the variable, and says
        // so by name instead of letting opencode answer the provider's 401.
        let refusal = beta
            .engine()
            .receive(&payload)
            .expect_err("beta has no key");
        let message = refusal.to_string();
        assert!(
            message.contains(&format!("{KEY} is not set on beta-machine")),
            "{message}"
        );
        assert!(message.contains("arreo sync secret set"), "{message}");
        assert!(
            !beta.path("opencode.jsonc").exists(),
            "a refused payload must not land"
        );

        // Step 4: beta sets its own value, and the same payload now applies.
        beta.set_secret(KEY, "beta-value");
        let outcome = beta.engine().receive(&payload).expect("applies");
        assert!(
            matches!(outcome, ReceiveOutcome::Applied { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            beta.read("opencode.jsonc"),
            alpha.read("opencode.jsonc"),
            "the bytes are identical on both machines"
        );

        // Step 5: each machine resolves the reference from its own store.
        let (_, alpha_plan) = alpha.engine().injection("opencode.jsonc").expect("plan");
        let (_, beta_plan) = beta.engine().injection("opencode.jsonc").expect("plan");
        assert_eq!(
            alpha_plan.environment(),
            vec![(KEY.to_string(), "alpha-value".to_string())]
        );
        assert_eq!(
            beta_plan.environment(),
            vec![(KEY.to_string(), "beta-value".to_string())]
        );
        assert!(alpha_plan.is_complete() && beta_plan.is_complete());

        // The exchange is idempotent: sending it again writes nothing.
        assert_eq!(
            beta.engine().receive(&payload).expect("again"),
            ReceiveOutcome::UpToDate {
                file: "opencode.jsonc".to_string()
            }
        );
    }

    #[test]
    fn alpha_can_receive_back_so_both_roots_hold_the_same_file_and_history() {
        let mut alpha = Root::new("both-alpha", "alpha-machine");
        let mut beta = Root::new("both-beta", "beta-machine");
        alpha.set_secret(KEY, "alpha-value");
        beta.set_secret(KEY, "beta-value");
        alpha.write("opencode.jsonc", &worked_case());

        let payload = alpha.engine().payload("opencode.jsonc").expect("payload");
        beta.engine().receive(&payload).expect("applies");
        // The return trip: beta's vector now includes alpha's counter, and alpha
        // absorbs nothing new — the exchange is a function that can be called
        // twice, which is what the mesh's `Message::Sync` pair wraps in sockets.
        let back = beta.engine().payload("opencode.jsonc").expect("payload");
        assert_eq!(back.vector.get(alpha.id()), 1);
        assert_eq!(
            back.vector.get(beta.id()),
            0,
            "beta has edited nothing, so it has no revision of its own"
        );
        assert_eq!(
            alpha.engine().receive(&back).expect("applies"),
            ReceiveOutcome::UpToDate {
                file: "opencode.jsonc".to_string()
            }
        );
        assert_eq!(alpha.read("opencode.jsonc"), beta.read("opencode.jsonc"));
        assert_eq!(
            alpha
                .engine()
                .history("opencode.jsonc")
                .expect("history")
                .len(),
            1,
            "one revision, pushed once"
        );
        assert_eq!(
            beta.engine()
                .history("opencode.jsonc")
                .expect("history")
                .len(),
            1,
            "one revision, received once"
        );
    }

    /// The class decides before anything is read: `auth.json` is refused as
    /// "not syncable" even though it does not exist, and even when its contents
    /// would have been a secret finding. A refusal that said "possible secret"
    /// would tell the operator their credentials file is dirty when the truth is
    /// that it never travels.
    #[test]
    fn a_local_file_is_refused_by_class_before_it_is_read_or_scanned() {
        let root = Root::new("class", "alpha-machine");
        let missing = root.engine().push("auth.json").expect_err("refused");
        assert!(
            matches!(
                missing,
                SyncError::NotSyncable {
                    class: Class::Local,
                    ..
                }
            ),
            "{missing:?}"
        );
        assert!(
            !missing.to_string().contains("no such file"),
            "the class refusal runs before the read: {missing}"
        );

        root.write("auth.json", "{\"apiKey\": \"sk-0123456789abcdef\"}\n");
        let dirty = root.engine().push("auth.json").expect_err("refused");
        assert!(matches!(dirty, SyncError::NotSyncable { .. }), "{dirty:?}");
        assert!(
            !dirty.to_string().contains("possible"),
            "a credential file is not dirty, it is not syncable: {dirty}"
        );

        // A syncable file on the same machine gets past the class check and
        // fails on the read instead, which is the ordering stated in one line.
        let absent = root.engine().push("models.json").expect_err("no file yet");
        assert!(matches!(absent, SyncError::Missing { .. }), "{absent:?}");
    }

    /// The deny-list, for a path no preset knows. The content here is *clean*:
    /// the refusal is about what the file is, which is the whole reason the list
    /// is checked before the scan.
    #[test]
    fn a_credential_store_no_preset_knows_is_refused_by_the_deny_list() {
        let root = Root::new("deny", "alpha-machine");
        let path = root.dir.join("cfg/arreo/credentials.json");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(&path, "{\"apiKey\": \"{env:ARREO_T83_KEY}\"}\n").expect("write");
        let refusal = root
            .engine()
            .push(path.to_str().expect("utf8"))
            .expect_err("refused");
        assert!(
            matches!(
                refusal,
                SyncError::Denied {
                    rule: DenyRule::Credentials,
                    ..
                }
            ),
            "{refusal:?}"
        );
        assert!(refusal.to_string().contains("LOCAL"), "{refusal}");

        let database = root.dir.join("cfg/arreo/sessions.db");
        std::fs::write(&database, "not a key at all\n").expect("write");
        let refusal = root
            .engine()
            .push(database.to_str().expect("utf8"))
            .expect_err("refused");
        assert!(
            matches!(
                refusal,
                SyncError::Denied {
                    rule: DenyRule::Database,
                    ..
                }
            ),
            "{refusal:?}"
        );
    }

    #[test]
    fn a_codex_hook_trust_block_is_refused_before_the_scan() {
        let root = Root::new("codex", "alpha-machine");
        let path = root.dir.join("cfg/arreo/codex.toml");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        std::fs::write(
            &path,
            "[hooks.state.abc.trusted_hash]\nhash = \"0123456789abcdef\"\n",
        )
        .expect("write");
        let refusal = root
            .engine()
            .push(path.to_str().expect("utf8"))
            .expect_err("refused");
        assert!(
            matches!(
                refusal,
                SyncError::Denied {
                    rule: DenyRule::HookTrustHash,
                    ..
                }
            ),
            "{refusal:?}"
        );
    }

    #[test]
    fn a_machine_local_field_with_a_literal_is_refused_by_name() {
        let root = Root::new("fields", "alpha-machine");
        root.write(
            "settings.json",
            "{\n  \"theme\": \"dark\",\n  \"lastChangelogVersion\": \"0.84.4\"\n}\n",
        );
        let refusal = root.engine().push("settings.json").expect_err("refused");
        assert!(
            matches!(refusal, SyncError::LocalField { ref field, .. } if field == "lastChangelogVersion"),
            "{refusal:?}"
        );
        assert!(
            refusal.to_string().contains("lastChangelogVersion"),
            "{refusal}"
        );

        // The same file without the per-machine marker is intent, and travels.
        root.write("settings.json", "{\n  \"theme\": \"dark\"\n}\n");
        assert!(root.engine().push("settings.json").expect("pushes").changed);

        // A provider-prefixed literal in the same field is caught earlier, by
        // the scanner — which is the point of keeping both nets: the field rule
        // names the *kind* of value, the scan reads its shape.
        root.write(
            "models.json",
            "{\n  \"apiKey\": \"vbk_pro_0123456789abcdef\"\n}\n",
        );
        let refusal = root.engine().push("models.json").expect_err("refused");
        assert!(matches!(refusal, SyncError::Secrets { .. }), "{refusal:?}");
    }

    #[test]
    fn a_path_only_one_machine_can_resolve_is_refused() {
        let root = Root::new("paths", "alpha-machine");
        root.write(
            "opencode.jsonc",
            "{\n  \"plugin\": [\"/home/dev/plugins/mine.js\"]\n}\n",
        );
        let refusal = root.engine().push("opencode.jsonc").expect_err("refused");
        assert!(
            matches!(refusal, SyncError::AbsolutePath { .. }),
            "{refusal:?}"
        );
        assert!(refusal.to_string().contains("/home/dev/plugins/mine.js"));
        // A URL is not an absolute path, which is why the rule is narrow.
        root.write(
            "opencode.jsonc",
            "{\n  \"provider\": { \"p\": { \"options\": { \"baseURL\": \"https://code.verboo.ai/router/v1\" } } }\n}\n",
        );
        assert!(
            root.engine()
                .push("opencode.jsonc")
                .expect("pushes")
                .changed
        );
    }

    /// The keep-both rule, and the mutation the acceptance names: make the
    /// conflict path last-writer-wins and this test goes red (the live file
    /// would hold the peer's bytes and no copy would exist).
    #[test]
    fn concurrent_edits_keep_both_copies_and_the_live_file_is_untouched() {
        let mut alpha = Root::new("conflict-alpha", "alpha-machine");
        let mut beta = Root::new("conflict-beta", "beta-machine");
        alpha.set_secret(KEY, "alpha-value");
        beta.set_secret(KEY, "beta-value");
        alpha.write("opencode.jsonc", &worked_case());
        let first = alpha.engine().payload("opencode.jsonc").expect("payload");
        beta.engine().receive(&first).expect("applies");

        // Both edit without seeing each other.
        alpha.write(
            "opencode.jsonc",
            &worked_case().replace(
                "\"provider\": {",
                "\"provider\": {\n    \"alphaOnly\": { \"name\": \"Alpha\" },",
            ),
        );
        let alpha_side = alpha.engine().payload("opencode.jsonc").expect("payload");
        beta.write(
            "opencode.jsonc",
            &worked_case().replace(
                "\"provider\": {",
                "\"provider\": {\n    \"betaOnly\": { \"name\": \"Beta\" },",
            ),
        );
        beta.engine().push("opencode.jsonc").expect("push");

        let beta_before = beta.read("opencode.jsonc");
        let outcome = beta.engine().receive(&alpha_side).expect("conflict");
        let ReceiveOutcome::Conflict {
            copy, live, from, ..
        } = outcome
        else {
            panic!("expected a conflict, got {outcome:?}");
        };
        assert_eq!(from, alpha.id());
        assert_eq!(live, beta.path("opencode.jsonc"));
        assert_eq!(
            beta.read("opencode.jsonc"),
            beta_before,
            "the live file is never overwritten by a concurrent payload"
        );
        let name = copy.file_name().expect("name").to_str().expect("utf8");
        // The loser is named by its **device id**, not by its display name: the
        // id is stable across a rename and filesystem-safe (`dev_<hex>`), and a
        // conflict copy named after a machine that has since been renamed would
        // point at a machine nobody can find.
        assert!(
            name.starts_with(&format!("opencode.conflict-{}-", alpha.id()))
                && name.ends_with(".jsonc"),
            "{name}"
        );
        let copy_text = std::fs::read_to_string(&copy).expect("copy");
        assert!(copy_text.contains("alphaOnly"));
        assert!(!copy_text.contains("betaOnly"));
        // The losing copy is in the history, so consuming it later loses nothing.
        let history = beta.engine().history("opencode.jsonc").expect("history");
        assert_eq!(history.first().map(|r| r.reason.as_str()), Some("conflict"));
        assert_eq!(
            beta.engine()
                .conflicts("opencode.jsonc")
                .expect("list")
                .len(),
            1
        );
    }

    /// A vector counts *published* revisions, so an edit made here and not yet
    /// pushed is invisible to it. A payload that is merely newer must not
    /// overwrite that edit with no copy anywhere — the sync is never
    /// last-writer-wins, and this is the case a vector alone gets wrong.
    #[test]
    fn an_unpublished_local_edit_is_kept_when_a_newer_payload_arrives() {
        let mut alpha = Root::new("unpub-alpha", "alpha-machine");
        let mut beta = Root::new("unpub-beta", "beta-machine");
        alpha.set_secret(KEY, "alpha-value");
        beta.set_secret(KEY, "beta-value");
        alpha.write("opencode.jsonc", &worked_case());
        let first = alpha.engine().payload("opencode.jsonc").expect("payload");
        beta.engine().receive(&first).expect("applies");

        // Beta hand-edits its copy and does not push: the store has no revision
        // for this state, and beta's vector is unchanged.
        let edit = beta.read("opencode.jsonc").replace(
            "\"name\": \"Verboo Code\"",
            "\"name\": \"Beta's unpublished edit\"",
        );
        beta.write("opencode.jsonc", &edit);

        alpha.write(
            "opencode.jsonc",
            &worked_case().replace("\"name\": \"Verboo Code\"", "\"name\": \"Alpha v2\""),
        );
        let second = alpha.engine().payload("opencode.jsonc").expect("payload");
        let outcome = beta.engine().receive(&second).expect("kept");
        let ReceiveOutcome::Conflict { copy, .. } = outcome else {
            panic!("an unpublished edit must be kept, got {outcome:?}");
        };
        assert_eq!(
            beta.read("opencode.jsonc"),
            edit,
            "the local edit survived a payload that was strictly newer"
        );
        assert!(std::fs::read_to_string(&copy)
            .expect("copy")
            .contains("Alpha v2"));
    }

    /// A machine that has never pushed still keeps a hand-written config: the
    /// first payload it receives does not destroy it.
    #[test]
    fn a_hand_written_file_the_history_never_saw_is_kept_too() {
        let mut alpha = Root::new("hand-alpha", "alpha-machine");
        let mut beta = Root::new("hand-beta", "beta-machine");
        alpha.set_secret(KEY, "alpha-value");
        beta.set_secret(KEY, "beta-value");
        alpha.write("opencode.jsonc", &worked_case());
        let payload = alpha.engine().payload("opencode.jsonc").expect("payload");
        let hand_written =
            "{\n  \"provider\": {\n    \"mine\": { \"name\": \"Hand-written\" }\n  }\n}\n";
        beta.write("opencode.jsonc", hand_written);

        let outcome = beta.engine().receive(&payload).expect("kept");
        assert!(
            matches!(outcome, ReceiveOutcome::Conflict { .. }),
            "{outcome:?}"
        );
        assert_eq!(beta.read("opencode.jsonc"), hand_written);

        // Once beta pushes its own file, the vectors distinguish the two and the
        // next payload resolves by comparison instead of by protection.
        beta.engine().push("opencode.jsonc").expect("push");
        let third = beta.engine().payload("opencode.jsonc").expect("payload");
        assert_eq!(third.vector.get(beta.id()), 1);
        assert!(matches!(
            alpha.engine().receive(&third).expect("conflict"),
            ReceiveOutcome::Conflict { .. }
        ));
    }

    #[test]
    fn the_sibling_extension_refuses_a_write_and_the_merge_reconciles_it() {
        let mut alpha = Root::new("sib-alpha", "alpha-machine");
        let mut beta = Root::new("sib-beta", "beta-machine");
        alpha.set_secret(KEY, "alpha-value");
        beta.set_secret(KEY, "beta-value");
        alpha.write("opencode.jsonc", &worked_case());
        let first = alpha.engine().payload("opencode.jsonc").expect("payload");
        beta.engine().receive(&first).expect("applies");

        // opencode merges both extensions (measured), so a second file here is
        // two live provider lists.
        let sibling = beta.path("opencode.json");
        std::fs::write(
            &sibling,
            "{\n  \"$schema\": \"https://opencode.ai/config.json\",\n  \"provider\": {\n    \"siblingOnly\": { \"name\": \"Sibling\" }\n  }\n}\n",
        )
        .expect("sibling");

        alpha.write(
            "opencode.jsonc",
            &worked_case().replace("\"name\": \"Verboo Code\"", "\"name\": \"Verboo Code v2\""),
        );
        let second = alpha.engine().payload("opencode.jsonc").expect("payload");
        let refusal = beta.engine().receive(&second).expect_err("refused");
        assert!(matches!(refusal, SyncError::Sibling { .. }), "{refusal:?}");
        assert!(refusal.to_string().contains("opencode.json"), "{refusal}");
        assert!(
            refusal.to_string().contains("arreo sync merge"),
            "{refusal}"
        );
        assert!(
            !beta.read("opencode.jsonc").contains("v2"),
            "the refused payload did not land"
        );

        // Reconcile: the sibling's provider is folded in and the sibling is
        // renamed aside, never deleted.
        let outcome = beta.engine().merge("opencode.jsonc").expect("merge");
        assert!(outcome.sources.iter().any(|s| s.ends_with("opencode.json")));
        assert!(
            !sibling.exists(),
            "the sibling is no longer a second live config"
        );
        let renamed: Vec<String> = std::fs::read_dir(sibling.parent().expect("parent"))
            .expect("dir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains("reconciled"))
            .collect();
        assert_eq!(renamed.len(), 1, "{renamed:?}");
        let merged = beta.read("opencode.jsonc");
        assert!(merged.contains("siblingOnly"), "{merged}");
        assert!(merged.contains("verboo"), "{merged}");
    }

    #[test]
    fn a_jsonc_comment_refuses_the_merge_rather_than_being_reformatted_away() {
        let mut alpha = Root::new("cmt-alpha", "alpha-machine");
        let mut beta = Root::new("cmt-beta", "beta-machine");
        alpha.set_secret(KEY, "alpha-value");
        beta.set_secret(KEY, "beta-value");
        // A commented file: legal in the real format, and this merge re-emits
        // what it parsed, so it refuses instead of dropping the operator's note.
        let commented = format!("// the owner's note\n{}", worked_case());
        alpha.write("opencode.jsonc", &commented);
        let payload = alpha.engine().payload("opencode.jsonc").expect("payload");
        beta.engine().receive(&payload).expect("applies");
        // Both edit, so the exchange conflicts and the copy is what a merge
        // would take in.
        beta.write(
            "opencode.jsonc",
            &commented.replace("\"name\": \"Verboo Code\"", "\"name\": \"Beta\""),
        );
        beta.engine().push("opencode.jsonc").expect("push");
        alpha.write(
            "opencode.jsonc",
            &commented.replace("\"name\": \"Verboo Code\"", "\"name\": \"Alpha\""),
        );
        let second = alpha.engine().payload("opencode.jsonc").expect("payload");
        assert!(matches!(
            beta.engine().receive(&second).expect("conflict"),
            ReceiveOutcome::Conflict { .. }
        ));
        let refusal = beta.engine().merge("opencode.jsonc").expect_err("refused");
        assert!(
            matches!(refusal, SyncError::Merge(MergeRefusal::Comments { .. })),
            "{refusal:?}"
        );
        assert!(refusal.to_string().contains("comments"), "{refusal}");
        assert!(
            beta.read("opencode.jsonc").contains("the owner's note"),
            "the comment is still there — the merge did not rewrite the file"
        );
    }

    #[test]
    fn a_plugin_array_merges_as_a_union_between_two_copies() {
        let mut alpha = Root::new("arr-alpha", "alpha-machine");
        let mut beta = Root::new("arr-beta", "beta-machine");
        alpha.set_secret(KEY, "alpha-value");
        beta.set_secret(KEY, "beta-value");
        let alpha_file = worked_case().replace(
            "\"provider\": {",
            "\"plugin\": [\"alpha.js\"],\n  \"provider\": {",
        );
        alpha.write("opencode.jsonc", &alpha_file);
        let first = alpha.engine().payload("opencode.jsonc").expect("payload");
        beta.engine().receive(&first).expect("applies");
        beta.write(
            "opencode.jsonc",
            &beta.read("opencode.jsonc").replace(
                "\"plugin\": [\"alpha.js\"]",
                "\"plugin\": [\"alpha.js\", \"beta.js\"]",
            ),
        );
        beta.engine().push("opencode.jsonc").expect("push");
        alpha.write(
            "opencode.jsonc",
            &alpha_file.replace(
                "\"plugin\": [\"alpha.js\"]",
                "\"plugin\": [\"alpha.js\", \"alpha2.js\"]",
            ),
        );
        let second = alpha.engine().payload("opencode.jsonc").expect("payload");
        assert!(matches!(
            beta.engine().receive(&second).expect("conflict"),
            ReceiveOutcome::Conflict { .. }
        ));
        beta.engine().merge("opencode.jsonc").expect("merge");
        let merged: serde_json::Value =
            serde_json::from_str(&beta.read("opencode.jsonc")).expect("json");
        assert_eq!(
            merged["plugin"],
            serde_json::json!(["alpha.js", "beta.js", "alpha2.js"]),
            "the union keeps every machine's plugins and duplicates none"
        );
    }

    #[test]
    fn revert_puts_the_previous_revision_back_and_propagates_it() {
        let mut alpha = Root::new("rev-alpha", "alpha-machine");
        let mut beta = Root::new("rev-beta", "beta-machine");
        alpha.set_secret(KEY, "alpha-value");
        beta.set_secret(KEY, "beta-value");
        let first = worked_case();
        alpha.write("opencode.jsonc", &first);
        alpha.engine().push("opencode.jsonc").expect("push");
        let second = first.replace("\"name\": \"Verboo Code\"", "\"name\": \"Broken\"");
        alpha.write("opencode.jsonc", &second);
        alpha.engine().push("opencode.jsonc").expect("push");
        let payload = alpha.engine().payload("opencode.jsonc").expect("payload");
        assert!(payload.content.contains("Broken"));
        beta.engine().receive(&payload).expect("applies");
        assert!(beta.read("opencode.jsonc").contains("Broken"));

        // The 3 a.m. undo: one command, the previous version back.
        let outcome = alpha
            .engine()
            .revert("opencode.jsonc", None)
            .expect("reverts");
        assert_eq!(outcome.counter, 3);
        assert_eq!(alpha.read("opencode.jsonc"), first);
        let history = alpha.engine().history("opencode.jsonc").expect("history");
        assert_eq!(history.first().map(|r| r.reason.as_str()), Some("revert"));

        // The revert is a revision like any other, so it travels.
        let payload = alpha.engine().payload("opencode.jsonc").expect("payload");
        assert_eq!(payload.vector.get(alpha.id()), 3);
        beta.engine().receive(&payload).expect("applies");
        assert_eq!(beta.read("opencode.jsonc"), first);

        // A file that never travels has no undo either — the class refusal
        // comes first, exactly as it does for push.
        let refusal = alpha
            .engine()
            .revert("auth.json", None)
            .expect_err("refused");
        assert!(
            matches!(refusal, SyncError::NotSyncable { .. }),
            "{refusal:?}"
        );
    }

    #[test]
    fn a_second_push_of_an_unchanged_file_invents_no_revision() {
        let mut alpha = Root::new("idem-alpha", "alpha-machine");
        alpha.set_secret(KEY, "alpha-value");
        alpha.write("opencode.jsonc", &worked_case());
        let first = alpha.engine().push("opencode.jsonc").expect("push");
        let again = alpha.engine().push("opencode.jsonc").expect("push");
        assert!(first.changed);
        assert!(!again.changed, "same bytes, same revision");
        assert_eq!(again.counter, first.counter);
        assert_eq!(again.revision, first.revision);
        assert_eq!(
            alpha
                .engine()
                .history("opencode.jsonc")
                .expect("history")
                .len(),
            1
        );
    }

    #[test]
    fn the_receiver_rescans_and_refuses_a_payload_the_sender_never_vetted() {
        let mut beta = Root::new("rescan", "beta-machine");
        beta.set_secret(KEY, "beta-value");
        let payload = SyncPayload {
            file: "opencode.jsonc".to_string(),
            harness: "opencode".to_string(),
            machine: other_id(),
            vector: Vector::from_pairs([(other_id(), 1)]),
            digest: String::new(),
            content: "{\n  \"provider\": { \"p\": { \"options\": { \"apiKey\": \"{env:ARREO_T83_KEY}\" } } },\n  \"extra\": \"sk-0123456789abcdef\"\n}\n".to_string(),
        };
        let payload = SyncPayload {
            digest: digest_of(payload.content.as_bytes()),
            ..payload
        };
        let refusal = beta.engine().receive(&payload).expect_err("refused");
        assert!(matches!(refusal, SyncError::Secrets { .. }), "{refusal:?}");
        assert!(!beta.path("opencode.jsonc").exists());

        // A payload whose bytes do not match its digest is refused too: the
        // transport will authenticate, but a truncated payload must still be
        // caught by the check that has the bytes in hand.
        let tampered = SyncPayload {
            digest: "00".repeat(32),
            ..payload.clone()
        };
        let refusal = beta.engine().receive(&tampered).expect_err("refused");
        assert!(matches!(refusal, SyncError::Digest { .. }), "{refusal:?}");

        // And a payload for a harness this machine has no preset for.
        let foreign = SyncPayload {
            file: "grok-config.toml".to_string(),
            ..payload
        };
        let refusal = beta.engine().receive(&foreign).expect_err("refused");
        assert!(matches!(refusal, SyncError::UnknownFile(_)), "{refusal:?}");
    }

    /// **A truncated payload is refused, not written** (review F5). The digest
    /// proves the bytes are the sender's; it says nothing about whether they are
    /// a config. Without the format check a half-written document landed and left
    /// the harness unable to start, with the previous good bytes surviving only in
    /// the local history — the outcome `write_atomic`'s own note calls worse than
    /// an old file.
    #[test]
    fn a_payload_that_is_not_a_document_is_refused_before_anything_is_written() {
        let mut alpha = Root::new("malformed-alpha", "alpha-machine");
        alpha.set_secret(KEY, "alpha-value");
        alpha.write("opencode.jsonc", &worked_case());
        alpha.engine().push("opencode.jsonc").expect("push");
        let live_before = alpha.read("opencode.jsonc");

        let truncated = "{\n  \"provider\": {\n";
        let payload = SyncPayload {
            file: "opencode.jsonc".to_string(),
            harness: "opencode".to_string(),
            machine: other_id(),
            vector: Vector::from_pairs([(other_id(), 2)]),
            digest: digest_of(truncated.as_bytes()),
            content: truncated.to_string(),
        };
        let refusal = alpha.engine().receive(&payload).expect_err("refused");
        assert!(
            matches!(refusal, SyncError::Malformed { .. }),
            "{refusal:?}"
        );
        assert!(
            refusal.to_string().contains("does not parse as JSON"),
            "and says what is wrong: {refusal}"
        );
        assert_eq!(
            alpha.read("opencode.jsonc"),
            live_before,
            "the live file is exactly as it was"
        );

        // A YAML file with no mapping is refused the same way.
        let yaml = SyncPayload {
            file: "models.json".to_string(),
            harness: "pi".to_string(),
            machine: other_id(),
            vector: Vector::from_pairs([(other_id(), 2)]),
            digest: digest_of(b"not: [a mapping\n"),
            content: "not: [a mapping\n".to_string(),
        };
        assert!(
            matches!(
                alpha.engine().receive(&yaml).expect_err("refused"),
                SyncError::Malformed { .. }
            ),
            "a broken JSON payload is refused too"
        );
    }

    /// **A forged vector cannot buy a silent overwrite** (review F2). A payload
    /// is free to describe its own counter; it is not free to describe *ours*.
    /// Claiming this machine is further ahead than its own store says made the
    /// live file look stale, so the keep-both rule was skipped and the peer's
    /// bytes replaced an unpushed local edit with no copy anywhere. The claim is
    /// now checked against the store, and a lie about a third machine is never
    /// recorded at all.
    #[test]
    fn a_payload_claiming_our_own_counter_is_refused() {
        let mut alpha = Root::new("forged-alpha", "alpha-machine");
        let mut beta = Root::new("forged-beta", "beta-machine");
        alpha.set_secret(KEY, "alpha-value");
        beta.set_secret(KEY, "beta-value");
        alpha.write("opencode.jsonc", &worked_case());
        let first = alpha.engine().payload("opencode.jsonc").expect("payload");
        beta.engine().receive(&first).expect("applies");
        // Beta edits and publishes, so its store says beta is at 1.
        beta.write(
            "opencode.jsonc",
            &worked_case().replace("Verboo Code", "Verboo Code (beta)"),
        );
        beta.engine().push("opencode.jsonc").expect("push");
        let live_before = beta.read("opencode.jsonc");

        // Alpha publishes its own revision too, so the honest payload is a
        // genuine concurrent edit — the shape whose decision the forgery flips.
        alpha.write(
            "opencode.jsonc",
            &worked_case().replace("Verboo Code", "Verboo Code (alpha)"),
        );
        alpha.engine().push("opencode.jsonc").expect("alpha pushes");

        // A hostile payload: alpha's bytes, but a vector claiming *beta* (the
        // receiver) is at 9 — which beta never issued.
        let mut forged = alpha.engine().payload("opencode.jsonc").expect("payload");
        forged.vector.set(beta.id(), 9);
        let refusal = beta.engine().receive(&forged).expect_err("refused");
        assert!(
            matches!(
                refusal,
                SyncError::ForgedVector {
                    ref machine,
                    claimed: 9,
                    ..
                } if machine.as_str() == beta.id()
            ),
            "{refusal:?}"
        );
        assert_eq!(
            beta.read("opencode.jsonc"),
            live_before,
            "and nothing was written"
        );

        // The honest version of the same payload still takes keep-both: the
        // forged field was the whole difference.
        let honest = alpha.engine().payload("opencode.jsonc").expect("payload");
        let outcome = beta.engine().receive(&honest).expect("conflict");
        assert!(
            matches!(outcome, ReceiveOutcome::Conflict { .. }),
            "keep-both, not an overwrite: {outcome:?}"
        );
        assert_eq!(
            beta.read("opencode.jsonc"),
            live_before,
            "the live file is still this machine's edit"
        );
    }

    /// A payload cannot pin a **third** machine's counter either: recording a
    /// peer's hearsay about another peer is how one message suppresses that
    /// machine's future revisions (they arrive as "already up to date" for ever).
    #[test]
    fn a_payload_cannot_pin_a_third_machines_counter() {
        let mut alpha = Root::new("pin-alpha", "alpha-machine");
        let mut beta = Root::new("pin-beta", "beta-machine");
        let mut gamma = Root::new("pin-gamma", "gamma-machine");
        for root in [&mut alpha, &mut beta, &mut gamma] {
            root.set_secret(KEY, "shared-value");
        }
        alpha.write("opencode.jsonc", &worked_case());
        let a1 = alpha.engine().payload("opencode.jsonc").expect("payload");
        beta.engine().receive(&a1).expect("beta applies");
        gamma.engine().receive(&a1).expect("gamma applies");

        // Gamma edits and publishes its own revision.
        gamma.write(
            "opencode.jsonc",
            &worked_case().replace("Verboo Code", "Verboo Code (gamma)"),
        );
        gamma.engine().push("opencode.jsonc").expect("gamma pushes");
        let gamma_payload = gamma.engine().payload("opencode.jsonc").expect("payload");

        // Alpha edits and publishes, then a hostile party takes alpha's payload
        // and adds a claim that gamma is at 99.
        alpha.write(
            "opencode.jsonc",
            &worked_case().replace("Verboo Code", "Verboo Code (alpha)"),
        );
        alpha.engine().push("opencode.jsonc").expect("alpha pushes");
        let mut forged = alpha.engine().payload("opencode.jsonc").expect("payload");
        forged.vector.set(gamma.id(), 99);
        beta.engine()
            .receive(&forged)
            .expect("beta takes alpha's revision");
        assert!(
            gamma_payload.vector.get(gamma.id()) >= 1,
            "gamma's own revision is in its payload: {:?}",
            gamma_payload.vector.summary()
        );

        // Gamma's genuine revision still lands: its counter was never pinned.
        let outcome = beta
            .engine()
            .receive(&gamma_payload)
            .expect("gamma's revision is still news");
        assert!(
            !matches!(outcome, ReceiveOutcome::UpToDate { .. }),
            "a third machine's counter cannot be pinned by hearsay: {outcome:?}"
        );
    }

    #[test]
    fn a_neutral_reference_lands_in_the_receiving_harnesss_own_dialect() {
        let mut alpha = Root::new("neutral-alpha", "alpha-machine");
        alpha.set_secret(KEY, "alpha-value");
        // pi's dialect is `${NAME}`, and the payload arrives in the neutral form
        // because that is what the wire carries.
        let content = format!(
            "{{\n  \"providers\": {{\n    \"p\": {{\n      \"apiKey\": \"${{ARREO_ENV:{KEY}}}\"\n    }}\n  }}\n}}\n"
        );
        // The sender is *another* machine, which is the only shape a real
        // payload has: a vector claiming a counter for the receiving machine is
        // exactly the forgery `ForgedVector` refuses (review F2).
        let payload = SyncPayload {
            file: "models.json".to_string(),
            harness: "pi".to_string(),
            machine: other_id(),
            vector: Vector::from_pairs([(other_id(), 1)]),
            digest: digest_of(content.as_bytes()),
            content,
        };
        assert!(matches!(
            alpha.engine().receive(&payload).expect("applies"),
            ReceiveOutcome::Applied { .. }
        ));
        let landed = alpha.read("models.json");
        assert!(landed.contains(&format!("\"${{{KEY}}}\"")), "{landed}");
        assert!(
            !landed.contains("ARREO_ENV"),
            "the neutral form never lands"
        );
    }

    #[test]
    fn status_names_every_syncable_file_and_the_names_this_machine_lacks() {
        let mut root = Root::new("status", "alpha-machine");
        root.set_secret(KEY, "alpha-value");
        root.write("opencode.jsonc", &worked_case());
        root.engine().push("opencode.jsonc").expect("push");
        let status = root.engine().status().expect("status");
        let files: Vec<&str> = status.iter().map(|s| s.file.as_str()).collect();
        for expected in [
            "opencode.jsonc",
            "tui.jsonc",
            "models.json",
            "settings.json",
            "models.yml",
            "config.yml",
        ] {
            assert!(files.contains(&expected), "{files:?}");
        }
        let opencode = status
            .iter()
            .find(|s| s.file == "opencode.jsonc")
            .expect("present");
        assert!(opencode.present);
        assert_eq!(opencode.vector, format!("{}:1", root.id()));
        assert!(opencode.missing.is_empty());
        assert_eq!(opencode.harness, "opencode");
        assert_eq!(opencode.class, Class::Sync);
        assert!(
            opencode.portable.contains(&"provider.<id>.options.baseURL"),
            "{:?}",
            opencode.portable
        );
        // Nothing local is listed as a candidate.
        assert!(!files.contains(&"auth.json"));
        assert!(!files.contains(&"opencode.db"));
        // A machine that cannot resolve a root reports it rather than failing
        // the whole listing.
        let bare = MachineEnv::new("bare", Os::Linux, root.dir.join("nohome"));
        let bare_store = SessionStore::open_memory().expect("store");
        let bare_secrets = SecretStore::open(root.dir.join("none.json")).expect("secrets");
        let bare_status = SyncEngine::new(&bare_store, &bare, &bare_secrets, root.id())
            .status()
            .expect("status");
        let models = bare_status
            .iter()
            .find(|s| s.file == "models.json")
            .expect("present");
        assert!(models.unresolved.is_some(), "{models:?}");
    }

    #[test]
    fn nothing_of_this_ever_puts_a_secret_in_a_synced_file_or_a_history_row() {
        let mut alpha = Root::new("nosecret-alpha", "alpha-machine");
        let mut beta = Root::new("nosecret-beta", "beta-machine");
        alpha.set_secret(KEY, "super-secret-value");
        beta.set_secret(KEY, "other-secret-value");
        alpha.write("opencode.jsonc", &worked_case());
        let payload = alpha.engine().payload("opencode.jsonc").expect("payload");
        assert!(!payload.content.contains("super-secret-value"));
        beta.engine().receive(&payload).expect("applies");
        let revision = beta
            .engine()
            .history("opencode.jsonc")
            .expect("history")
            .first()
            .expect("a revision")
            .id;
        let bytes = beta
            .store
            .sync_content(revision)
            .expect("content")
            .expect("present");
        let text = String::from_utf8(bytes).expect("utf8");
        assert!(!text.contains("other-secret-value"));
        assert!(!text.contains("super-secret-value"));
        assert!(text.contains(KEY), "the name is what travels");
    }
}
