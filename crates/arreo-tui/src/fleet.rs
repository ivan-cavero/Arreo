//! Fleet verbs from the TUI (T-0074): the account's machines and this
//! machine's trust grants.
//!
//! **Why a TUI may do this at all.** The TUI is a client of the daemon, and
//! `arreo machines` is not a daemon verb — it dials the relay itself, with this
//! machine's paired identity (`device.key` plus the certificate `arreo pair`
//! saved), and the trust verbs open this machine's own ledger beside the socket.
//! The TUI may not depend on the CLI binary crate (T-0074's constraint), so it
//! does what the CLI does: the same `arreo-core` calls, in the same order, with
//! the same sentences and the same exit-code vocabulary. Every *fact* — the
//! directory rows, the presence rule, the name rule, the ledger rule, the
//! refusals the relay prints — comes from `arreo-core`; what is repeated here is
//! only the CLI line a refusal is wrapped in, and `xtask/src/mesh_slice.rs`
//! asserts those lines against the CLI's own output for the same situation.
//!
//! **The exit codes are the contract** (T-0044, stated in `arreo machines
//! --help`): 0 ok, 1 store/identity failure, 2 usage, 3 no such machine, 4
//! unreachable, 5 the relay refused. They are a type here rather than loose
//! numbers, so a caller cannot invent a sixth.
//!
//! **Trust is local, always.** The trust verbs act on the machine this process
//! runs on, whatever the TUI is looking at: a grant recorded anywhere but the
//! machine that will enforce it is advice, not access (ADR 0019). A TUI attached
//! to another machine by name therefore refuses the trust verbs with the same
//! sentence the CLI's `--machine <other>` prints — never silently acting on the
//! local ledger under a remote name.

use arreo_core::identity::authority::{now_ms, Layout};
use arreo_core::identity::{self, DeviceCert, DeviceId, DeviceKey, Role, RootKey};
use arreo_core::mesh::{default_machine_name, MachineId, MachineRow, Name, Presence, TrustLedger};
use arreo_core::relay::session::RelaySession;
use arreo_core::relay::{join_proof_payload, JoinRequest, RELAY_VERSION};
use arreo_core::store::rfc3339_ms;
use std::path::{Path, PathBuf};

/// The exit-code vocabulary of `arreo machines` (T-0044), as a type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    /// 0 — the verb did what was asked.
    Ok,
    /// 1 — a store or identity this command needed could not be read.
    Failure,
    /// 2 — usage: the operator has to say something else.
    Usage,
    /// 3 — no such machine (or a name that is not a name).
    UnknownMachine,
    /// 4 — the relay is unreachable, or this machine has no paired identity.
    Unreachable,
    /// 5 — the relay refused (name conflict, trust refusal).
    Conflict,
}

impl Code {
    /// The number the CLI exits with for this outcome.
    #[must_use]
    pub fn as_u8(self) -> u8 {
        match self {
            Self::Ok => 0,
            Self::Failure => 1,
            Self::Usage => 2,
            Self::UnknownMachine => 3,
            Self::Unreachable => 4,
            Self::Conflict => 5,
        }
    }

    /// Is this one of the three codes the criterion names as refusals?
    #[must_use]
    pub fn refused(self) -> bool {
        matches!(
            self,
            Self::UnknownMachine | Self::Unreachable | Self::Conflict
        )
    }
}

/// One machine, as the panel shows it: the fields `arreo machines list` prints,
/// from the same [`MachineRow`] the CLI maps.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub name: String,
    /// The relay computed this against *its* clock; recomputing would be a
    /// second presence rule (T-0043 has one).
    pub presence: Presence,
    pub age_secs: i64,
    /// Whether the row carries the key a peer dials (T-0045): a name with one
    /// can be reached by `arreo attach --machine`.
    pub reachable: bool,
    pub name_conflict: bool,
    pub tombstone_until_ms: Option<i64>,
}

impl Machine {
    fn from_relay(row: &MachineRow, now_ms: i64) -> Self {
        Self {
            name: row.name.as_str().to_string(),
            // The relay's own answer, not a re-derivation.
            presence: row.presence,
            age_secs: age_secs(row.last_seen_ms, now_ms),
            reachable: row.daemon_key.is_some(),
            name_conflict: row.name_conflict,
            tombstone_until_ms: row.tombstone_until_ms,
        }
    }

    /// What an operator needs to know beyond the name — the CLI's flags,
    /// minus `unverified`, which is a fact only a cache read can have (the TUI
    /// does not read the cache; see `deviations` in the task report).
    #[must_use]
    pub fn flags(&self, now_ms: i64) -> Vec<&'static str> {
        let mut flags = Vec::new();
        if self.name_conflict {
            flags.push("name-suffixed");
        }
        if self.tombstone_until_ms.is_some_and(|until| until > now_ms) {
            flags.push("name-tombstoned");
        } else if self.presence == Presence::Stale {
            flags.push("name-reclaimable");
        }
        flags
    }
}

/// One grant, as the trust panel shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub device: String,
    pub role: Role,
    pub live: bool,
}

/// What one verb produced: the CLI's code, the CLI's sentence, and the rows a
/// panel renders when the verb listed something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub code: Code,
    /// The CLI's own line, verb prefix included (`machines: …`,
    /// `machines trust: …`, `devices revoke: …`). The status line renders it
    /// verbatim, which is what "no silent divergence" means in practice.
    pub message: String,
    pub machines: Vec<Machine>,
    pub grants: Vec<Grant>,
}

impl Outcome {
    fn ok(message: String) -> Self {
        Self {
            code: Code::Ok,
            message,
            machines: Vec::new(),
            grants: Vec::new(),
        }
    }

    fn failed(code: Code, message: String) -> Self {
        Self {
            code,
            message,
            machines: Vec::new(),
            grants: Vec::new(),
        }
    }

    /// The verb succeeded (exit 0).
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.code == Code::Ok
    }
}

/// The two trust verbs, whose CLI lines differ for the same underlying failure
/// (the grant lives under `machines trust`, the cut under `devices revoke`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrustVerb {
    Grant,
    Revoke,
}

/// A grant the UI is about to write, so the confirmation and the write cannot
/// disagree about what was confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrantPreview {
    pub device: DeviceId,
    pub role: Role,
    /// The line the operator confirms: fingerprint, previous state, role,
    /// machine.
    pub line: String,
    /// The machine the grant is on (this one, always — trust is local), named in
    /// the result line the CLI prints.
    pub machine_name: String,
}

/// Everything a fleet verb needs to know about where it runs.
#[derive(Debug, Clone)]
pub struct Fleet {
    /// The socket this TUI reads. The store beside it is this machine's trust
    /// ledger — `Layout::for_socket`, the same derivation the CLI uses.
    pub socket: PathBuf,
    /// This device's identity directory: `device.key`, `devices/<id>.cert` (the
    /// paired identity the directory verbs need) and `root.key` (this machine's
    /// own key, which the trust ledger is keyed by). Resolved once by `main`
    /// (`$ARREO_IDENTITY_DIR`, else the standard root) and carried here, so a
    /// test can point the ledger at a scratch directory without touching the
    /// process environment.
    pub identity_root: PathBuf,
    /// `--config`, else `$ARREO_CONFIG`: the `[relay]` section the directory
    /// verbs need. Deliberately no default path, for the reason
    /// `docs/machines.md` gives: a second answer to "which file is this
    /// machine's relay configuration" is the defect class this project keeps
    /// finding.
    pub config: Option<PathBuf>,
    /// The machine this TUI is attached to over the relay, when it was resolved
    /// by name. Trust is local, so this is the CLI's `--machine <other>` case.
    pub attached_to: Option<String>,
}

impl Fleet {
    /// The authority layout for this machine: its root key and certificate
    /// directory, and the store beside the socket.
    fn layout(&self) -> Layout {
        Layout {
            root_key: self.identity_root.join("root.key"),
            cert_dir: self.identity_root.join("devices"),
            store: arreo_core::identity::authority::sidecar_db(&self.socket),
        }
    }

    /// This machine's identity and name, derived from the socket it serves —
    /// `local_machine` in `arreo machines trust`, and the reason a ledger keyed
    /// by anything else would be a second answer to "which machine am I".
    ///
    /// `verb` is the CLI line the one failure here is reported under: the two
    /// trust verbs spell it differently (`machines trust` names the key file,
    /// `devices revoke` does not), and a refusal that diverges is the bug this
    /// file exists to avoid.
    fn local_machine(&self, verb: TrustVerb) -> Result<(MachineId, String), Outcome> {
        let root = RootKey::load_or_generate(&self.layout().root_key).map_err(|e| match verb {
            TrustVerb::Grant => Outcome::failed(
                Code::Failure,
                format!(
                    "machines trust: cannot read this machine's key ({}): {e}",
                    self.layout().root_key.display()
                ),
            ),
            TrustVerb::Revoke => Outcome::failed(
                Code::Failure,
                format!("devices revoke: cannot read this machine's key: {e}"),
            ),
        })?;
        Ok((MachineId::from_key(&root.public()), default_machine_name()))
    }

    /// This machine's trust ledger, for a local administrative verb.
    fn local_ledger(&self, name: &str, verb: TrustVerb) -> Result<TrustLedger, Outcome> {
        let layout = self.layout();
        TrustLedger::open(&layout.store, &layout.root_key, name.to_string()).map_err(|e| match verb
        {
            TrustVerb::Grant => Outcome::failed(Code::Failure, format!("machines trust: {e}")),
            TrustVerb::Revoke => Outcome::failed(
                Code::Failure,
                format!("devices revoke: cannot open this machine's trust ledger: {e}"),
            ),
        })
    }

    /// The CLI's `--machine` refusal: a TUI attached to another machine cannot
    /// change trust *here* under that machine's name, and acting quietly on the
    /// local ledger instead would be the worst outcome — a silent no-op on the
    /// machine the operator named.
    fn remote_refusal(
        &self,
        verb: TrustVerb,
        this_machine: &MachineId,
        this_name: &str,
    ) -> Option<Outcome> {
        let named = self.attached_to.as_deref()?;
        if machine_matches(named, this_machine, this_name) {
            return None;
        }
        let message = match verb {
            TrustVerb::Grant => format!(
                "machines trust: {named:?} is not this machine (which is {this_name:?}, {}). \
                 Trust is local: no machine — and not the relay — can grant on another's \
                 behalf. Run this on {named} itself.",
                this_machine.as_str()
            ),
            TrustVerb::Revoke => format!(
                "devices revoke: {named:?} is not this machine (which is {this_name:?}, {}). A \
                 machine's grants live in its own store: run this on {named}.",
                this_machine.as_str()
            ),
        };
        Some(Outcome::failed(Code::Conflict, message))
    }

    /// The configuration file to read, or the CLI's usage refusal.
    fn config_path(&self) -> Result<PathBuf, Outcome> {
        if let Some(path) = &self.config {
            return Ok(path.clone());
        }
        if let Some(path) = std::env::var_os("ARREO_CONFIG") {
            return Ok(PathBuf::from(path));
        }
        Err(Outcome::failed(
            Code::Usage,
            "machines: which configuration? pass --config PATH, or set ARREO_CONFIG (the [relay] \
             section names the relay and the account)"
                .to_string(),
        ))
    }

    /// The relay conversation a write verb needs: the `[relay]` settings and
    /// this machine's paired identity. A write cannot fall back to the cache the
    /// way a read can, so it stops with the reason.
    async fn session(&self) -> Result<RelaySession, Outcome> {
        let path = self.config_path()?;
        let settings = match arreo_core::relay::config::load_config(&path) {
            Ok(Some(settings)) => settings,
            Ok(None) => {
                return Err(Outcome::failed(
                    Code::Unreachable,
                    format!(
                        "machines: no relay is configured ({}), so the directory cannot be written",
                        path.display()
                    ),
                ))
            }
            Err(e) => return Err(Outcome::failed(Code::Usage, format!("machines: {e}"))),
        };
        let (key, cert) = match paired_identity(&self.identity_root) {
            Ok(pair) => pair,
            Err(message) => {
                return Err(Outcome::failed(
                    Code::Unreachable,
                    format!("machines: {message}"),
                ))
            }
        };
        RelaySession::dial(settings.addr, &settings.account, &key, &cert)
            .await
            .map_err(|e| {
                Outcome::failed(
                    Code::Unreachable,
                    format!("machines: cannot reach the relay at {}: {e}", settings.addr),
                )
            })
    }

    /// `arreo machines list`: the directory, with presence.
    ///
    /// The relay's answer or the relay's refusal, with the CLI's own sentence
    /// and the CLI's code (3/4/5). Deliberately **not** the CLI's cache
    /// fallback: reading `identity/machines.cache` needs `serde_json`, and a
    /// new dependency is not this task's to add — so the TUI says the relay is
    /// unreachable rather than printing remembered rows. Same refusal, no
    /// remembered rows; see the task report.
    pub async fn machines_list(&self) -> Outcome {
        let path = match self.config_path() {
            Ok(path) => path,
            Err(outcome) => return outcome,
        };
        let settings = match arreo_core::relay::config::load_config(&path) {
            Ok(Some(settings)) => settings,
            Ok(None) => {
                return Outcome::failed(
                    Code::Unreachable,
                    format!(
                        "machines: no relay is configured ({}), so the account's directory cannot \
                         be read; --offline answers from what this machine remembers",
                        path.display()
                    ),
                )
            }
            Err(e) => return Outcome::failed(Code::Usage, format!("machines: {e}")),
        };
        let (key, cert) = match paired_identity(&self.identity_root) {
            Ok(pair) => pair,
            Err(message) => {
                return Outcome::failed(Code::Unreachable, format!("machines: {message}"))
            }
        };
        let session = match RelaySession::dial(settings.addr, &settings.account, &key, &cert).await
        {
            Ok(session) => session,
            Err(e) => {
                return Outcome::failed(
                    Code::Unreachable,
                    format!("machines: cannot reach the relay at {}: {e}", settings.addr),
                )
            }
        };
        match session.machines(false).await {
            Ok(reply) => match (reply.refused, reply.machines) {
                (Some(reason), _) => Outcome::failed(
                    Code::Conflict,
                    format!("machines: the relay refused to answer: {reason}"),
                ),
                (None, rows) => {
                    let now = now_ms();
                    let mut machines: Vec<Machine> = rows
                        .iter()
                        .map(|row| Machine::from_relay(row, now))
                        .collect();
                    machines.sort_by(|a, b| a.name.cmp(&b.name));
                    Outcome {
                        code: Code::Ok,
                        message: format!("{} machine(s) from the relay", machines.len()),
                        machines,
                        grants: Vec::new(),
                    }
                }
            },
            Err(e) => Outcome::failed(
                Code::Unreachable,
                format!("machines: the relay did not answer: {e}"),
            ),
        }
    }

    /// `arreo machines rename <old> <new>`.
    pub async fn machines_rename(&self, old: &str, new: &str) -> Outcome {
        let session = match self.session().await {
            Ok(session) => session,
            Err(outcome) => return outcome,
        };
        // The row first: the verb takes names, the wire takes ids, and this is
        // also what makes "no such machine" exit 3 before anything is written.
        let row = match row_named(&session, old, true).await {
            Ok(row) => row,
            Err(outcome) => return outcome,
        };
        if let Err(e) = Name::parse(new) {
            return Outcome::failed(
                Code::Conflict,
                format!("machines: {new:?} is not a machine name: {e}"),
            );
        }
        match session.rename_machine(row.machine_id.as_str(), new).await {
            Ok(reply) => match (reply.refused, reply.granted) {
                (Some(reason), _) => {
                    Outcome::failed(refusal_code(&reason), format!("machines: {reason}"))
                }
                // The relay's row is what is printed: the caller sees what the
                // directory now holds, not what it asked for.
                (None, Some(updated)) => {
                    Outcome::ok(format!("renamed {old} → {}", updated.name.as_str()))
                }
                (None, None) => Outcome::failed(
                    Code::Conflict,
                    "machines: the relay answered a rename without a row".to_string(),
                ),
            },
            Err(e) => Outcome::failed(
                Code::Unreachable,
                format!("machines: the relay did not answer: {e}"),
            ),
        }
    }

    /// `arreo machines remove <name> [--force]`.
    ///
    /// `force` is the `--force` the CLI's refusal asks for, and it is only
    /// reachable through the confirmation the UI shows (T-0074's safety rule):
    /// tombstoning a machine that is answering right now is almost always a
    /// mistake.
    pub async fn machines_remove(&self, name: &str, force: bool) -> Outcome {
        let session = match self.session().await {
            Ok(session) => session,
            Err(outcome) => return outcome,
        };
        let row = match row_named(&session, name, true).await {
            Ok(row) => row,
            Err(outcome) => return outcome,
        };
        if row.presence == Presence::Online && !force {
            return Outcome::failed(
                Code::Conflict,
                format!(
                    "machines: {name} is online right now (seen {} ago); pass --force to \
                     tombstone it anyway",
                    human_age(age_secs(row.last_seen_ms, now_ms()))
                ),
            );
        }
        match session.remove_machine(row.machine_id.as_str()).await {
            Ok(reply) => match (reply.refused, reply.granted) {
                (Some(reason), _) => {
                    Outcome::failed(refusal_code(&reason), format!("machines: {reason}"))
                }
                (None, Some(removed)) => match removed.tombstone_until_ms {
                    Some(until) => Outcome::ok(format!(
                        "removed {} — the name is held until {} ({}), then it is free",
                        removed.name,
                        rfc3339_ms(until),
                        human_age(((until - now_ms()).max(0)) / 1000)
                    )),
                    None => Outcome::ok(format!(
                        "removed {} — the relay reported no tombstone, which means it is gone",
                        removed.name
                    )),
                },
                (None, None) => Outcome::failed(
                    Code::Conflict,
                    "machines: the relay answered a removal without a row".to_string(),
                ),
            },
            Err(e) => Outcome::failed(
                Code::Unreachable,
                format!("machines: the relay did not answer: {e}"),
            ),
        }
    }

    /// `arreo machines add <pairing-code> --uri <invite>` (T-0058): join the
    /// account, then register this machine's own directory row.
    ///
    /// The pairing exchange is blocking — a mailbox poll loop, bounded by the
    /// invite's TTL — so it runs on the blocking pool: a TUI frozen solid while
    /// a phone is expected is the one thing this must not be.
    pub async fn machines_add(&self, code: &str, uri: &str) -> Outcome {
        if code.trim().is_empty() {
            return Outcome::failed(
                Code::Usage,
                "machines add: needs the pairing code the admitting machine displayed".to_string(),
            );
        }
        if uri.trim().is_empty() {
            return Outcome::failed(
                Code::Usage,
                "machines add: needs --uri (the invite the admitting machine printed, which \
                 carries the mailbox, the session and its key)"
                    .to_string(),
            );
        }
        let code = code.to_string();
        let uri = uri.to_string();
        let fleet = self.clone();
        let (paired, invite) =
            match tokio::task::spawn_blocking(move || fleet.join_pairing(&code, &uri)).await {
                Ok(Ok(pair)) => pair,
                Ok(Err(outcome)) => return outcome,
                Err(e) => {
                    return Outcome::failed(
                        Code::Failure,
                        format!("machines add: the pairing task failed: {e}"),
                    )
                }
            };
        let Some(directory) = invite.directory else {
            return Outcome::failed(
                Code::Unreachable,
                "machines add: this invite names no account and relay, so there is nothing to join \
                 (the admitting machine printed it without a [relay] configuration). Ask it to run \
                 `arreo pair` again, with its relay configured, or use `arreo pair --join` for an \
                 ordinary pairing"
                    .to_string(),
            );
        };
        let addr: std::net::SocketAddr = match directory.relay.parse() {
            Ok(addr) => addr,
            Err(e) => {
                return Outcome::failed(
                    Code::Usage,
                    format!(
                        "machines add: the invite names the relay as {:?}, which is not an IP:PORT \
                         address: {e}",
                        directory.relay
                    ),
                )
            }
        };
        let session =
            match RelaySession::dial(addr, &directory.account, &paired.key, &paired.cert).await {
                Ok(session) => session,
                Err(e) => {
                    return Outcome::failed(
                        Code::Unreachable,
                        format!(
                            "machines add: joined the account's devices, but cannot reach the \
                             relay at {addr} to register this machine: {e}"
                        ),
                    )
                }
            };
        let root = match RootKey::load_or_generate(&self.layout().root_key) {
            Ok(root) => root,
            Err(e) => {
                return Outcome::failed(
                    Code::Failure,
                    format!("machines add: cannot read this machine's key: {e}"),
                )
            }
        };
        let fingerprint = MachineId::from_key(&root.public());
        let requested = default_machine_name();
        let machine_key = root.public_hex();
        let payload = join_proof_payload(
            session.nonce(),
            &session.account(),
            &machine_key,
            &requested,
        );
        let request = JoinRequest {
            v: RELAY_VERSION,
            name: requested.clone(),
            proto_version: arreo_core::proto::VERSION,
            machine_key,
            signature: root.sign(&payload).to_bytes().to_vec(),
        };
        match session.join_machine(request).await {
            Ok(reply) => match (reply.refused, reply.granted) {
                (Some(reason), _) => Outcome::failed(
                    Code::Conflict,
                    format!("machines add: the relay would not register this machine: {reason}"),
                ),
                (None, Some(row)) => {
                    let granted = row.name.as_str();
                    let mut message = if granted == requested {
                        format!("joined as {}", paired.cert.device().display_id())
                    } else {
                        format!(
                            "joined as {} ({granted} — {requested:?} was taken, so the relay added \
                             the suffix)",
                            paired.cert.device().display_id()
                        )
                    };
                    message.push_str(&format!(
                        " · machine {granted} · id {}",
                        fingerprint.as_str()
                    ));
                    Outcome::ok(message)
                }
                (None, None) => Outcome::failed(
                    Code::Conflict,
                    "machines add: the relay answered a join without a row".to_string(),
                ),
            },
            Err(e) => Outcome::failed(
                Code::Unreachable,
                format!("machines add: the relay did not answer: {e}"),
            ),
        }
    }

    /// `arreo machines trust --list`: who may use this machine.
    pub fn trust_list(&self) -> Outcome {
        let (machine, name) = match self.local_machine(TrustVerb::Grant) {
            Ok(pair) => pair,
            Err(outcome) => return outcome,
        };
        if let Some(outcome) = self.remote_refusal(TrustVerb::Grant, &machine, &name) {
            return outcome;
        }
        let ledger = match self.local_ledger(&name, TrustVerb::Grant) {
            Ok(ledger) => ledger,
            Err(outcome) => return outcome,
        };
        let rows = match ledger.devices() {
            Ok(rows) => rows,
            Err(e) => {
                return Outcome::failed(
                    Code::Failure,
                    format!("machines trust: cannot read this machine's grants: {e}"),
                )
            }
        };
        let grants = rows
            .iter()
            .map(|row| Grant {
                device: row.device.display_id(),
                role: row.role,
                live: row.is_live(),
            })
            .collect();
        Outcome {
            code: Code::Ok,
            message: format!("{} grant(s) on {name}", rows.len()),
            machines: Vec::new(),
            grants,
        }
    }

    /// Everything the grant needs before it is confirmed: the fingerprint, what
    /// this machine already thinks of the device, and the role it would get.
    ///
    /// The pinned check runs **before** the confirmation, exactly as the CLI
    /// orders it, so the operator is never asked to confirm something that
    /// cannot work — and it is the check most likely to catch a mistyped
    /// fingerprint.
    pub fn trust_preview(&self, raw: &str, role_text: &str) -> Result<GrantPreview, Outcome> {
        let device = DeviceId::parse(raw).map_err(|e| {
            Outcome::failed(
                Code::UnknownMachine,
                format!("machines trust: {raw:?} is not a device id: {e}"),
            )
        })?;
        let role = if role_text.trim().is_empty() {
            // Least privilege by default: widening a grant is easy, noticing
            // one you did not mean is not.
            Role::Viewer
        } else {
            Role::parse(role_text).map_err(|_| {
                Outcome::failed(
                    Code::Usage,
                    format!("machines trust: --role takes viewer or operator (got {role_text:?})"),
                )
            })?
        };
        let (machine, name) = self.local_machine(TrustVerb::Grant)?;
        if let Some(outcome) = self.remote_refusal(TrustVerb::Grant, &machine, &name) {
            return Err(outcome);
        }
        let ledger = self.local_ledger(&name, TrustVerb::Grant)?;
        if !pinned_here(self.layout(), &device) {
            return Err(Outcome::failed(
                Code::UnknownMachine,
                format!(
                    "machines trust: {} is not pinned on this machine, so a grant would do \
                     nothing (it could not authenticate here). Pin it first: `arreo devices \
                     issue --key …` on this machine, or `arreo pair` on the device.",
                    device.display_id()
                ),
            ));
        }
        let existing = ledger.devices().unwrap_or_default();
        let previous = existing.iter().find(|row| row.device == device);
        let line = format!(
            "grant {} ({}) {} access to this machine ({name})?",
            device.display_id(),
            describe_previous(previous),
            role.operator_term()
        );
        Ok(GrantPreview {
            device,
            role,
            line,
            machine_name: name,
        })
    }

    /// `arreo machines trust <device> --role …`, after the human confirmed.
    pub fn trust_grant(&self, preview: &GrantPreview) -> Outcome {
        let ledger = match self.local_ledger(&preview.machine_name, TrustVerb::Grant) {
            Ok(ledger) => ledger,
            Err(outcome) => return outcome,
        };
        match ledger.grant(&preview.device, preview.role, &preview.device, now_ms()) {
            Ok(_) => Outcome::ok(format!(
                "{} may now {} on {}",
                preview.device.display_id(),
                describe_role(preview.role),
                preview.machine_name
            )),
            Err(e) => Outcome::failed(
                Code::Failure,
                format!("machines trust: the grant could not be recorded: {e}"),
            ),
        }
    }

    /// `arreo devices revoke <device> --machine <this one>`: cut this machine's
    /// grant, and only it (T-0059).
    pub fn trust_revoke(&self, raw: &str) -> Outcome {
        let (machine, name) = match self.local_machine(TrustVerb::Revoke) {
            Ok(pair) => pair,
            Err(outcome) => return outcome,
        };
        if let Some(outcome) = self.remote_refusal(TrustVerb::Revoke, &machine, &name) {
            return outcome;
        }
        let Some(device) = DeviceId::parse(raw).ok() else {
            return Outcome::failed(
                Code::UnknownMachine,
                format!("devices revoke: {raw:?} is not a device id"),
            );
        };
        let ledger = match self.local_ledger(&name, TrustVerb::Revoke) {
            Ok(ledger) => ledger,
            Err(outcome) => return outcome,
        };
        match ledger.revoke(&device, now_ms()) {
            Ok(true) => Outcome::ok(format!(
                "cut {}'s grant on {name} — it keeps whatever access it has elsewhere",
                device.display_id()
            )),
            // Idempotent, like the device-level revoke: the desired state holds.
            Ok(false) => Outcome::ok(format!(
                "{} had no live grant on {name}",
                device.display_id()
            )),
            Err(e) => Outcome::failed(
                Code::Failure,
                format!("devices revoke: cannot cut the grant: {e}"),
            ),
        }
    }
}

/// The CLI's `--machine` comparison: id or name, because both are things an
/// operator has in hand.
fn machine_matches(named: &str, this_machine: &MachineId, this_name: &str) -> bool {
    named == this_name || named.eq_ignore_ascii_case(this_name) || named == this_machine.as_str()
}

/// Is this device pinned in this machine's authority? A grant for an unpinned
/// key could never authenticate, so it would do nothing.
fn pinned_here(layout: Layout, device: &DeviceId) -> bool {
    match arreo_core::identity::authority::DeviceAuthority::load(layout) {
        Ok(authority) => authority
            .devices()
            .iter()
            .any(|record| &record.id == device),
        // A store that cannot be read is not "pinned": the grant would be inert
        // anyway, and the failure is reported by the ledger path.
        Err(_) => false,
    }
}

/// What this machine already thinks of the device, for the confirmation line —
/// the CLI's `describe_previous`.
fn describe_previous(previous: Option<&arreo_core::mesh::GrantedDevice>) -> String {
    match previous {
        None => "no grant here yet".to_string(),
        Some(row) if row.is_live() => format!("currently {}", row.role.operator_term()),
        Some(row) => format!(
            "grant revoked at {}",
            rfc3339_ms(row.revoked_at_ms.unwrap_or_default())
        ),
    }
}

fn describe_role(role: Role) -> &'static str {
    match role {
        Role::Owner => "observe and drive",
        Role::Viewer => "observe",
    }
}

/// Resolve a name to the row the directory holds, or the code for "no such
/// machine" (3) — the CLI's `row_named`.
async fn row_named(session: &RelaySession, wanted: &str, all: bool) -> Result<MachineRow, Outcome> {
    if let Err(e) = Name::parse(wanted) {
        return Err(Outcome::failed(
            Code::UnknownMachine,
            format!("machines: {wanted:?} is not a machine name: {e}"),
        ));
    }
    match session.machines(all).await {
        Ok(reply) if reply.refused.is_none() => match reply
            .machines
            .iter()
            .find(|row| row.name.as_str() == wanted)
        {
            Some(row) => Ok(row.clone()),
            None => Err(Outcome::failed(
                Code::UnknownMachine,
                format!("machines: no machine named {wanted:?} in this account"),
            )),
        },
        Ok(reply) => Err(Outcome::failed(
            Code::Conflict,
            format!(
                "machines: the relay refused to read the directory: {}",
                reply.refused.unwrap_or_default()
            ),
        )),
        Err(e) => Err(Outcome::failed(
            Code::Unreachable,
            format!("machines: the relay did not answer: {e}"),
        )),
    }
}

/// The relay's reply is a sentence, not a code, so the mapping is by what the
/// refusal says — the CLI's `refusal_code`, so a caller switching on the number
/// needs no second table.
fn refusal_code(reason: &str) -> Code {
    if reason.contains("NoSuchMachine") || reason.contains("no machine") {
        Code::UnknownMachine
    } else {
        Code::Conflict
    }
}

/// This machine's paired identity: the key it holds and the certificate the
/// server issued it — the CLI's `paired_identity`.
fn paired_identity(identity_root: &Path) -> Result<(DeviceKey, DeviceCert), String> {
    let root = identity_root;
    let key_path = root.join("device.key");
    let key = DeviceKey::load(&key_path).map_err(|e| {
        format!(
            "this machine has no paired identity ({}): {e}",
            key_path.display()
        )
    })?;
    let id = DeviceId::from_key(&key.public());
    // `DeviceCert::save` names the file after the bare hex id, not the
    // `dev_`-prefixed display form.
    let cert_path = root.join("devices").join(format!("{}.cert", id.as_str()));
    let cert = DeviceCert::load(&cert_path)
        .map_err(|e| format!("no certificate at {}: {e}", cert_path.display()))?;
    Ok((key, cert))
}

/// Age of a heartbeat in seconds, saturating and never negative.
fn age_secs(last_seen_ms: i64, now_ms: i64) -> i64 {
    now_ms.saturating_sub(last_seen_ms).max(0) / 1000
}

/// A human age, in the CLI's units, so a refusal reads the same on both
/// surfaces — and the machines panel's LAST SEEN column matches the table.
#[must_use]
pub fn human_age(secs: i64) -> String {
    match secs {
        0..=1 => "now".to_string(),
        2..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

impl Fleet {
    /// The phone half of a pairing, start to finish: parse, exchange, and persist
    /// what came back — the CLI's `join_pairing`.
    ///
    /// The persist-only-on-success rule and the key-reuse rule are the point: a
    /// failed pairing must leave nothing behind, and re-pairing the same device
    /// keeps its identity.
    fn join_pairing(
        &self,
        code_text: &str,
        uri: &str,
    ) -> Result<
        (
            arreo_core::pairing::flow::PairedDevice,
            arreo_core::pairing::Invite,
        ),
        Outcome,
    > {
        use arreo_core::pairing::flow::PairingPhone;
        use arreo_core::pairing::{Code as PairingCode, Invite};

        let code = PairingCode::parse(code_text)
            .map_err(|e| Outcome::failed(Code::Usage, format!("machines add: {e}")))?;
        let invite = Invite::parse_uri(uri)
            .map_err(|e| Outcome::failed(Code::Usage, format!("machines add: {e}")))?;
        let label = default_machine_name();
        let Some(label) = sanitize_device_name(&label) else {
            return Err(Outcome::failed(
                Code::Usage,
                "machines add: the device name is empty".to_string(),
            ));
        };

        let key_path = self.identity_root.join("device.key");
        let (key, generated) = match DeviceKey::load(&key_path) {
            Ok(key) => (key, false),
            Err(arreo_core::identity::KeyError::Missing { .. }) => (
                DeviceKey::generate()
                    .map_err(|e| Outcome::failed(Code::Failure, format!("machines add: {e}")))?,
                true,
            ),
            Err(e) => {
                return Err(Outcome::failed(
                    Code::Failure,
                    format!("machines add: cannot read {}: {e}", key_path.display()),
                ))
            }
        };

        let phone = PairingPhone::join(&invite, &code, key, &label)
            .map_err(|e| Outcome::failed(Code::Failure, format!("machines add: {e}")))?;
        let paired = phone
            .await_cert()
            .map_err(|e| Outcome::failed(Code::Failure, format!("machines add: {e}")))?;

        // Success is the only moment anything is written.
        if generated {
            paired.key.save(&key_path).map_err(|e| {
                Outcome::failed(
                    Code::Failure,
                    format!("machines add: paired, but cannot save the device key: {e}"),
                )
            })?;
        }
        let cert_dir = self.identity_root.join("devices");
        paired.cert.save(&cert_dir).map_err(|e| {
            Outcome::failed(
                Code::Failure,
                format!("machines add: paired, but cannot save the certificate: {e}"),
            )
        })?;
        // Pin the server's key so later connections can verify its certificates
        // without the invite (public material — no secret here).
        let server_key_path = self.identity_root.join("server.key");
        let pinned = self.identity_root.clone();
        identity::keys::create_private_dir(&pinned)
            .and_then(|()| {
                std::fs::write(&server_key_path, format!("{}\n", invite.server_key)).map_err(|e| {
                    arreo_core::identity::KeyError::Io {
                        path: server_key_path.clone(),
                        detail: e.to_string(),
                    }
                })
            })
            .map_err(|e| {
                Outcome::failed(
                    Code::Failure,
                    format!("machines add: paired, but cannot save the server key: {e}"),
                )
            })?;
        Ok((paired, invite))
    }
}

/// The device label a pairing sends: trimmed, control-free, non-empty.
fn sanitize_device_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}
