//! Session-scoped audit writing for the daemon (T-0033).
//!
//! One sentence: every session knows who is acting and from where, so every
//! action it takes can be recorded with that identity attached — which is what
//! turns the log from "something happened" into "**this device** did this".
//!
//! **Why a session-scoped recorder instead of a free function.** The prompt log
//! T-0018 shipped recorded `device: "cli"` for everything, because the write site
//! (inside verb dispatch) had no idea who was connected. For a remote device that
//! is simply false, and §3.7's "machine-local truth" criterion is exactly about
//! not being false here: a remote `send` must be attributed to the device that
//! made it. So the identity travels with the session and the recorder carries it.
//!
//! **Why the writes are best-effort.** An audit failure must not break the
//! session it is describing: the action already happened, and refusing to serve a
//! keystroke because a log line could not be written would trade a small
//! bookkeeping gap for a broken product. Failures are logged loudly instead, so
//! the gap is visible rather than silent.

use arreo_core::store::{actions, AuditEvent, AuditKind, AuditOutcome, SessionStore};
use std::path::{Path, PathBuf};

/// Who a session is, for the audit trail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Actor {
    /// A device id, or [`arreo_core::identity::revocation::LOCAL_CLI`] when the
    /// peer is this machine's own operator on the Unix socket.
    pub device: String,
    /// The peer's network, already truncated (`10.0.0.0/24`), or `None` for the
    /// local socket.
    pub peer: Option<String>,
}

impl Actor {
    /// The local operator: this machine's own Unix socket.
    #[must_use]
    pub fn local() -> Self {
        Self {
            device: arreo_core::identity::revocation::LOCAL_CLI.to_string(),
            peer: None,
        }
    }

    /// A remote device, with its network truncated at the moment of capture.
    #[must_use]
    pub fn remote(device: impl Into<String>, peer: std::net::SocketAddr) -> Self {
        Self {
            device: device.into(),
            peer: Some(arreo_core::store::truncate_peer(&peer)),
        }
    }
}

/// The audit writer for one session.
#[derive(Debug, Clone)]
pub struct SessionAudit {
    db: PathBuf,
    actor: Actor,
}

impl SessionAudit {
    #[must_use]
    pub fn new(db: PathBuf, actor: Actor) -> Self {
        Self { db, actor }
    }

    #[must_use]
    pub fn actor(&self) -> &Actor {
        &self.actor
    }

    /// Record one action.
    ///
    /// `agent` is what the action acted on (a pane id, an agent name); `detail`
    /// is anything that is not the prompt (a count, a protocol version).
    pub fn record(&self, action: &str, outcome: AuditOutcome, agent: &str, detail: Option<&str>) {
        self.record_with_prompt(action, outcome, agent, "", detail);
    }

    /// Record one action whose *content* matters (a prompt, a refusal reason).
    ///
    /// The content is redacted on the way in by the store, so nothing here has to
    /// remember to do it.
    pub fn record_with_prompt(
        &self,
        action: &str,
        outcome: AuditOutcome,
        agent: &str,
        prompt: &str,
        detail: Option<&str>,
    ) {
        let event = AuditEvent {
            device: self.actor.device.clone(),
            agent: agent.to_string(),
            prompt: prompt.to_string(),
            peer: self.actor.peer.clone(),
            detail: detail.map(str::to_string),
            ..AuditEvent::new(action, kind_for(action), outcome, now_ms())
        };
        if let Err(e) = self.write(&event) {
            // Loud, because a silently missing audit row is the failure mode this
            // whole module exists to prevent.
            eprintln!(
                "arreo-server: cannot write the audit row for {action} ({}): {e}",
                self.actor.device
            );
        }
    }

    fn write(&self, event: &AuditEvent) -> Result<(), arreo_core::store::SessionError> {
        SessionStore::open(&self.db)?.record(event)
    }

    /// The store, for a caller that needs to read back (tests, and the size
    /// guard).
    pub fn open_store(&self) -> Result<SessionStore, arreo_core::store::SessionError> {
        SessionStore::open(&self.db)
    }
}

/// The coarse kind for an action.
///
/// Derived rather than passed, so a writer cannot file a `send` under the wrong
/// kind — the classification follows from the action's name, which is the thing
/// the writer already has to get right.
fn kind_for(action: &str) -> AuditKind {
    match action {
        actions::PROMPT | actions::SEND | actions::ATTACH => AuditKind::Prompt,
        actions::AUTH_REJECT => AuditKind::AuthReject,
        actions::PAIRING_FAILED => AuditKind::PairingFailed,
        actions::DEVICE_ISSUE | actions::DEVICE_ROTATE | actions::DEVICE_REVOKE => {
            AuditKind::DeviceChange
        }
        // Sessions, spawns and splits are lifecycle events with no older kind;
        // `Unknown` is the honest label, and the `action` is what readers use.
        _ => AuditKind::Unknown,
    }
}

/// Milliseconds since the Unix epoch.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Warn when the audit log has grown past the size the operator should notice.
///
/// A warning, never a prune: §4's trail is append-only, and a log that deletes
/// itself to stay small is not a log. The operator decides, with
/// `arreo audit prune`.
pub const AUDIT_WARN_BYTES: u64 = 100 * 1024 * 1024;

/// Check the log's size and warn once if it is over the threshold.
///
/// Called at boot rather than on every write: the check is a table scan, and a
/// scan per keystroke would be a self-inflicted performance problem for a
/// warning that changes at most once per session.
pub fn warn_if_large(db: &Path) {
    let Ok(store) = SessionStore::open(db) else {
        return;
    };
    let Ok((rows, bytes)) = store.audit_size() else {
        return;
    };
    if bytes >= AUDIT_WARN_BYTES {
        eprintln!(
            "arreo-server: the audit log is {:.1} MB across {rows} rows ({}); \
             nothing prunes it automatically — `arreo audit prune --before <ms>` when you mean to",
            bytes as f64 / (1024.0 * 1024.0),
            db.display()
        );
    }
}
