//! The relay's audit trail (T-0053, ROADMAP §4): who connected to it, what it
//! refused, and what it had to drop — metadata only, in its own database,
//! outliving every machine that goes offline (§3.14).
//!
//! One sentence: the relay records what it *did* — a session it admitted, a
//! refusal it made, a message it dropped — and never what it *carried*.
//!
//! ## Why this is not the machine's log
//!
//! T-0033 landed the machine's trail (`arreo_core::store`'s `audit`): it knows
//! which pane was touched, by which device, and what the agent was asked. The
//! relay knows none of that and must not learn it. What the relay knows is that
//! a device authenticated, that a peer was refused, and that an envelope was
//! dropped — all of it metadata about *movement*, none of it about content.
//!
//! Keeping the two tables in two databases in two processes is the design, not a
//! convenience: it is what makes "the relay cannot accumulate content" a
//! structural fact rather than a promise. A shared table would need one side to
//! hold the other's database, and their lifetimes differ (a machine's log is the
//! machine's; the relay's outlives every machine that used it).
//!
//! What the two halves **do** share is the format: the same
//! [`arreo_core::store::render_export`], the same [`ExportFormat`], the same
//! [`AuditQuery`] filters, and the same [`arreo_core::store::redact_peer_text`]
//! truncation rule. Those are borrowed rather than re-implemented, so "the same
//! filter semantics and byte-identical output" is a property of the code.

use arreo_core::store::{AuditOutcome, ExportFormat};

/// The actions the relay writes. A closed vocabulary: a typo in a writer is a
/// compile error rather than a row nobody can find later.
pub mod actions {
    /// A device authenticated and its session began.
    pub const SESSION_CONNECT: &str = "session.connect";
    /// A session ended — cleanly or because the transport broke.
    pub const SESSION_DISCONNECT: &str = "session.disconnect";
    /// The relay turned a peer away: unknown account, bad certificate, bad proof
    /// of possession, or over its handshake budget.
    pub const REFUSE: &str = "relay.refuse";
    /// The inbox evicted a message because the device's queue was full.
    pub const INBOX_DROP: &str = "inbox.drop";
    /// The inbox deleted a message that reached its time-to-live.
    pub const INBOX_EXPIRE: &str = "inbox.expire";
    /// The operator pruned the trail itself.
    pub const PRUNE: &str = "audit.prune";
}

/// One row to append.
///
/// **The peer is a `SocketAddr`, not a string.** Truncation happens in
/// [`crate::RelayStore::record`], which means a caller cannot pass an untruncated
/// address even by accident — the type it has to hand over is the full one, and
/// the store is the only thing that turns it into text. `None` is for the rows
/// that genuinely have no peer: a prune, an inbox sweep.
#[derive(Debug, Clone)]
pub struct RelayAuditEvent {
    pub action: &'static str,
    pub outcome: AuditOutcome,
    pub device_id: Option<String>,
    pub account_id: Option<String>,
    pub peer: Option<std::net::SocketAddr>,
    pub proto_version: Option<u32>,
    pub detail: Option<String>,
}

impl RelayAuditEvent {
    /// An event with nothing but its action and outcome.
    #[must_use]
    pub fn new(action: &'static str, outcome: AuditOutcome) -> Self {
        Self {
            action,
            outcome,
            device_id: None,
            account_id: None,
            peer: None,
            proto_version: None,
            detail: None,
        }
    }

    #[must_use]
    pub fn device(mut self, device_id: impl Into<String>) -> Self {
        self.device_id = Some(device_id.into());
        self
    }

    #[must_use]
    pub fn account(mut self, account_id: impl Into<String>) -> Self {
        self.account_id = Some(account_id.into());
        self
    }

    #[must_use]
    pub fn peer(mut self, peer: std::net::SocketAddr) -> Self {
        self.peer = Some(peer);
        self
    }

    #[must_use]
    pub fn proto_version(mut self, version: u32) -> Self {
        self.proto_version = Some(version);
        self
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// The longest a `detail` may be. A reason is a sentence; anything longer is
/// either a bug or an attempt to store something that is not a reason.
pub const DETAIL_MAX: usize = 240;

/// One stored row, as the readers see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRelayAudit {
    pub ts_ms: u64,
    pub action: String,
    pub outcome: AuditOutcome,
    pub device_id: Option<String>,
    pub account_id: Option<String>,
    /// Already truncated when it was written; there is no untruncated form to
    /// recover, which is what makes "stays truncated in exports" true.
    pub peer: Option<String>,
    pub proto_version: Option<u32>,
    pub detail: Option<String>,
}

/// One row as the export and any `--json` reader sees it.
///
/// Field names are a script's contract, so they are stable and snake_case. They
/// are deliberately **not** the machine's field set: there is no `agent` and no
/// `prompt` here, because the relay has neither — a row that carried an empty
/// `prompt` column would invite a future writer to fill it.
#[must_use]
pub fn relay_audit_json(row: &StoredRelayAudit) -> serde_json::Value {
    serde_json::json!({
        "ts_ms": row.ts_ms,
        "action": row.action,
        "outcome": row.outcome.as_str(),
        "device": row.device_id,
        "account": row.account_id,
        "peer": row.peer,
        "proto_version": row.proto_version,
        "detail": row.detail,
    })
}

/// Render rows exactly as the machine's audit export renders its own.
///
/// A thin wrapper, on purpose: the relay names its own entry point while the
/// bytes come from the one implementation, so a change to the shape lands in both
/// logs or in neither.
pub fn render(
    rows: &[StoredRelayAudit],
    format: ExportFormat,
) -> Result<String, serde_json::Error> {
    let values: Vec<serde_json::Value> = rows.iter().map(relay_audit_json).collect();
    arreo_core::store::render_export(&values, format)
}
