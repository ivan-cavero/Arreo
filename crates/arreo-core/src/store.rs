//! Session + audit store (T-0018): versioned SQLite schema with migrations.
//!
//! One sentence: the daemon's memory (pane topology + scrollback) and its
//! conscience (append-only audit log) live in one WAL SQLite file, migrated
//! forward from the T-0006 metrics schema — never wiped.
//!
//! Schema:
//! - v1 (T-0006): `meta` + `rollups` (metrics history).
//! - v2 (T-0018): + `panes(id TEXT PRIMARY KEY, program, args JSON,
//!   cols, rows)` + `scrollback(pane, line_no, text)` + `audit(ts_ms,
//!   device, agent, prompt, redacted)`.
//! - v3 (T-0025): + `devices(id, name, role, public_key, serial, issued_at_ms,
//!   last_seen_ms, revoked)` and `audit.kind` (so a refusal is a first-class
//!   event, not a prompt with a strange name). **Public material only**: the
//!   schema has no column a private key could occupy — a device's secret half
//!   lives in its own 0600 file, never here, because the database is the thing
//!   that gets copied, backed up and synced.
//! - `open` runs `migrate()` (v1→v2→v3 `CREATE TABLE IF NOT EXISTS` + version
//!   bumps); future versions append `migrate_vN` steps. Data is never dropped
//!   by a migration — the migration test pins a surviving rollup row.
//!
//! Audit redaction: prompts are scanned with the fixture secret patterns
//! (`scan_secrets`); hits are replaced with `[REDACTED:<label>]` and the row
//! is flagged `redacted=1`. Field names survive (debuggable), key material
//! never touches disk.

use rusqlite::{params, Connection, OptionalExtension};
use thiserror::Error;

use crate::fixtures::scan_secrets;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("session json: {0}")]
    Json(#[from] serde_json::Error),
}

pub const SCHEMA_VERSION: u32 = 7;

/// One pane's persisted record: how to respawn it + what it showed.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredPane {
    pub id: String,
    pub program: String,
    pub args: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    pub scrollback: Vec<String>,
}

/// The outcome half of an audit row: what a review reads to tell an intention
/// from a result. `ok | refused | expired` is deliberately small — an audit row
/// that needs a taxonomy to interpret is a row nobody reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOutcome {
    /// The action was carried out.
    Ok,
    /// It was refused, and the reason is in the row's `detail`.
    Refused,
    /// Something the machine was holding ran out of time (T-0030's inbox drops).
    Expired,
}

impl AuditOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Refused => "refused",
            Self::Expired => "expired",
        }
    }

    /// An unrecognized value reads as `Ok` rather than an error: the log is
    /// append-only and must stay readable by an older binary.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text {
            "refused" => Self::Refused,
            "expired" => Self::Expired,
            _ => Self::Ok,
        }
    }
}

/// The actions this product writes, as constants.
///
/// A vocabulary, not free strings: the writers, the CLI's filters and the docs
/// all name the same events, and a typo in one of them would silently produce a
/// row that no query finds. (The relay's own actions live in T-0053.)
pub mod actions {
    /// A device (or the local operator) opened a session.
    pub const SESSION_CONNECT: &str = "session.connect";
    /// A session ended, with why in `detail`.
    pub const SESSION_DISCONNECT: &str = "session.disconnect";
    /// A client attached to a pane's stream.
    pub const ATTACH: &str = "attach";
    /// Input was sent to a pane.
    pub const SEND: &str = "send";
    /// A pane was spawned.
    pub const SPAWN: &str = "spawn";
    /// A pane was split.
    pub const SPLIT: &str = "split";
    /// A device was pinned (issued a certificate).
    pub const DEVICE_ISSUE: &str = "device.issue";
    /// A device was moved onto a new key.
    pub const DEVICE_ROTATE: &str = "device.rotate";
    /// A device was revoked (T-0026).
    pub const DEVICE_REVOKE: &str = "device.revoke";
    /// A connection was refused before any work happened.
    pub const AUTH_REJECT: &str = "auth.reject";
    /// A pairing attempt that produced no certificate (T-0024).
    pub const PAIRING_FAILED: &str = "pairing.failed";
    /// A prompt was sent to an agent (the original T-0018 row).
    pub const PROMPT: &str = "prompt";
    /// Old rows, and rows whose action an older schema did not record.
    pub const UNKNOWN: &str = "unknown";
    /// An operator pruned the log.
    pub const PRUNE: &str = "audit.prune";
    /// A resource budget was breached and the daemon acted on it (T-0019).
    pub const ENFORCE_BREACH: &str = "enforce.breach";
    /// A graded cgroup alert fired (T-0041): `warn` at 80%, `critical` at 95%.
    /// The level rides in `detail`, so one action name covers the ladder and an
    /// operator greps one string for the whole episode.
    pub const ENFORCE_ALERT: &str = "enforce.alert";
}

/// One audit row (prompt already redacted on write).
///
/// One struct and one writer, because the same event was previously written by
/// three methods (`audit`, `audit_event`, `audit_action`) that each filled a
/// different subset of the columns — which is how `action` came to be missing
/// from two of them.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEvent {
    pub ts_ms: u64,
    /// The event's own name, from [`actions`].
    pub action: String,
    /// The coarse classification the prompt-oriented readers predate; kept
    /// because existing tooling filters on it.
    pub kind: AuditKind,
    pub outcome: AuditOutcome,
    /// The device the row is **about** — always the subject, never the actor, so
    /// one column answers "what happened to this device" for every action.
    /// [`crate::identity::revocation::LOCAL_CLI`] and `daemon` appear for the
    /// machine's own operator and its background work.
    pub device: String,
    /// What it acted on (a pane id, an agent name). Empty when the action has no
    /// object.
    pub agent: String,
    /// The human-readable content: a prompt, a reason, a description. Redacted on
    /// write by the secret scan.
    pub prompt: String,
    /// The peer's address, **truncated at write** (see [`truncate_peer`]).
    pub peer: Option<String>,
    /// Anything else worth keeping that is not the prompt: a count, a serial, a
    /// protocol version — and, where the actor is not the subject (a revocation
    /// names who performed it), who did it.
    pub detail: Option<String>,
}

impl AuditEvent {
    /// The common case: an action with an outcome, everything else unset.
    #[must_use]
    pub fn new(action: &str, kind: AuditKind, outcome: AuditOutcome, now_ms: u64) -> Self {
        Self {
            ts_ms: now_ms,
            action: action.to_string(),
            kind,
            outcome,
            device: String::new(),
            agent: String::new(),
            prompt: String::new(),
            peer: None,
            detail: None,
        }
    }
}

/// Truncate a peer address so the log cannot become a location history.
///
/// ROADMAP §4's threat model includes a compromised cloud and a stolen phone, so
/// a full-address trail is a record of where someone was; a /24 (IPv4) or /48
/// (IPv6) still answers the question the log is for — "did this come from a
/// network I recognize". Truncation happens **at write**, not at export: a flag
/// someone forgets is a leak, while a value never stored cannot leak.
#[must_use]
pub fn truncate_peer(peer: &std::net::SocketAddr) -> String {
    match peer.ip() {
        std::net::IpAddr::V4(v4) => {
            let octets = v4.octets();
            format!("{}.{}.{}.0/24", octets[0], octets[1], octets[2])
        }
        std::net::IpAddr::V6(v6) => {
            // The first three 16-bit groups are the /48.
            let groups = v6.segments();
            format!("{:x}:{:x}:{:x}::/48", groups[0], groups[1], groups[2])
        }
    }
}

/// Audited event as read back (adds the redaction flag).
#[derive(Debug, Clone, PartialEq)]
pub struct StoredAudit {
    pub ts_ms: u64,
    pub device: String,
    pub agent: String,
    pub prompt: String,
    pub redacted: bool,
    pub kind: AuditKind,
    /// The event's own name (`prompt`, `device.revoke`, …), which is what an
    /// operator filters by; `kind` is the older, coarser classification.
    pub action: String,
    /// `ok | refused | expired` — what a review reads to tell intent from result.
    pub outcome: AuditOutcome,
    /// The peer's network, truncated at write.
    pub peer: Option<String>,
    /// Anything that is not the prompt: a count, a serial, a version.
    pub detail: Option<String>,
}

/// What an audit row is about. Refusals are auditable events in their own
/// right (T-0025): "the daemon said no" must be as visible as "someone asked".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditKind {
    /// An agent interaction (the original T-0018 rows).
    Prompt,
    /// A connection refused before any work happened.
    AuthReject,
    /// A device paired, rotated or revoked.
    DeviceChange,
    /// A pairing attempt that did not produce a certificate (T-0024): a wrong
    /// code, an expired window, or a peer that never completed. The reason
    /// lives in `prompt`, and the session id in `agent`.
    PairingFailed,
    /// A row written by a newer schema than this build knows.
    Unknown,
}

impl AuditKind {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prompt => "prompt",
            Self::AuthReject => "auth_reject",
            Self::DeviceChange => "device_change",
            Self::PairingFailed => "pairing_failed",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a stored value. An unrecognized kind is `Unknown` rather than an
    /// error: the audit log is append-only and must stay readable by an older
    /// binary (forward compatibility for the operator's eyes, not for policy).
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text {
            "prompt" => Self::Prompt,
            "auth_reject" => Self::AuthReject,
            "device_change" => Self::DeviceChange,
            "pairing_failed" => Self::PairingFailed,
            _ => Self::Unknown,
        }
    }
}

/// True when `table` has `column` (v2→v3 added `audit.kind` in place).
fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, SessionError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

pub struct SessionStore {
    conn: std::sync::Mutex<Connection>,
}

impl SessionStore {
    fn migrate(conn: &Connection) -> Result<(), SessionError> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE IF NOT EXISTS rollups(pane TEXT NOT NULL, ts_ms INTEGER NOT NULL,
               rss_bytes INTEGER NOT NULL, cpu REAL NOT NULL, pids INTEGER NOT NULL,
               PRIMARY KEY (pane, ts_ms));",
        )?;
        let version: u32 = conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map(|v| v.parse().unwrap_or(0))
            .unwrap_or(0);
        if version < 2 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS panes(
                   id TEXT PRIMARY KEY, program TEXT NOT NULL, args TEXT NOT NULL,
                   cols INTEGER NOT NULL, rows INTEGER NOT NULL);
                 CREATE TABLE IF NOT EXISTS scrollback(
                   pane TEXT NOT NULL, line_no INTEGER NOT NULL, text TEXT NOT NULL,
                   PRIMARY KEY (pane, line_no));
                 CREATE TABLE IF NOT EXISTS audit(
                   ts_ms INTEGER NOT NULL, device TEXT NOT NULL, agent TEXT NOT NULL,
                   prompt TEXT NOT NULL, redacted INTEGER NOT NULL);",
            )?;
            conn.execute(
                "INSERT INTO meta(key, value) VALUES ('schema_version', '2')
                 ON CONFLICT(key) DO UPDATE SET value='2'",
                [],
            )?;
        }
        if version < 3 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS devices(
                   id TEXT PRIMARY KEY, name TEXT NOT NULL, role TEXT NOT NULL,
                   public_key TEXT NOT NULL, serial INTEGER NOT NULL,
                   issued_at_ms INTEGER NOT NULL, last_seen_ms INTEGER,
                   revoked INTEGER NOT NULL DEFAULT 0, retired_to TEXT);
                 CREATE INDEX IF NOT EXISTS devices_public_key ON devices(public_key);",
            )?;
            // Existing audit rows predate the kind column: they are all prompt
            // events, which is exactly what the default says.
            if !has_column(conn, "audit", "kind")? {
                conn.execute_batch(
                    "ALTER TABLE audit ADD COLUMN kind TEXT NOT NULL DEFAULT 'prompt';",
                )?;
            }
            conn.execute(
                "INSERT INTO meta(key, value) VALUES ('schema_version', '3')
                 ON CONFLICT(key) DO UPDATE SET value='3'",
                [],
            )?;
        }
        // v4 (T-0026): *who* revoked and *when*, plus the audit action. A
        // revocation that records only "revoked" cannot answer the question an
        // operator actually asks afterwards ("who cut this device off, and
        // when?"), and the answer has to be durable because the log outlives the
        // person who made the call.
        if version < 4 {
            if !has_column(conn, "devices", "revoked_at")? {
                conn.execute_batch(
                    "ALTER TABLE devices ADD COLUMN revoked_at INTEGER;
                     ALTER TABLE devices ADD COLUMN revoked_by TEXT;",
                )?;
            }
            if !has_column(conn, "audit", "action")? {
                // Existing rows predate actions: they are all prompt events,
                // which is exactly what the default says.
                conn.execute_batch(
                    "ALTER TABLE audit ADD COLUMN action TEXT NOT NULL DEFAULT 'prompt';",
                )?;
            }
        }
        // v5 (T-0033): the audit trail grows from "what was typed" to "what
        // happened, to whom, and how it ended" — an action's outcome, the peer's
        // (truncated) network, and a free-form detail for counts and versions.
        if version < 5 {
            if !has_column(conn, "audit", "outcome")? {
                conn.execute_batch(
                    "ALTER TABLE audit ADD COLUMN outcome TEXT NOT NULL DEFAULT 'ok';
                     ALTER TABLE audit ADD COLUMN peer TEXT;
                     ALTER TABLE audit ADD COLUMN detail TEXT;",
                )?;
            }
            // History: every existing row is an action that happened, so `ok` is
            // the honest default — and the prompt rows keep their meaning.
            conn.execute_batch(
                "UPDATE audit SET action = 'prompt' WHERE action = 'prompt' OR action IS NULL;",
            )?;
        }
        // v6 (T-0040): metrics history — a real time series beside the live
        // sampler. Three tiers in one table, keyed (pane, ts_ms, step_ms) so a
        // re-run rollup is idempotent: 10 s rows kept 24 h, 1 m rollups 30 d,
        // 1 h rollups 365 d. Every row carries average AND peak RSS (peak is
        // the number people act on) plus cpu and pids. The old `rollups` table
        // (T-0006, unkeyed by step) is left alone — data never dropped by a
        // migration; the series API reads the new table and the old rows age
        // out through the normal store lifecycle.
        if version < 6 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS metrics_series(
                   pane TEXT NOT NULL, ts_ms INTEGER NOT NULL, step_ms INTEGER NOT NULL,
                   rss_avg INTEGER NOT NULL, rss_peak INTEGER NOT NULL,
                   cpu_avg REAL NOT NULL, cpu_peak REAL NOT NULL,
                   pids_avg REAL NOT NULL, pids_peak INTEGER NOT NULL,
                   samples INTEGER NOT NULL,
                   PRIMARY KEY (pane, ts_ms, step_ms));
                 CREATE INDEX IF NOT EXISTS metrics_series_range
                   ON metrics_series(pane, ts_ms);",
            )?;
        }
        // v7: per-machine device trust (T-0046). A grant is keyed
        // `(machine_id, device_id)`: the machine is part of the key, so a grant
        // made on one machine is one row here and nothing at all on another —
        // which is the model §3.7 asks for, expressed as a primary key rather
        // than as a promise.
        //
        // The device id is stored in its canonical bare-hex form (the
        // `DeviceId::as_str` spelling the certificate uses), because a table
        // that accepted either spelling would let one device be two rows. The
        // role is stored as text for the same reason the devices table does it:
        // a reader can see what it says, and a migration never has to decode it.
        if version < 7 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS machine_trust(
                   machine_id TEXT NOT NULL, device_id TEXT NOT NULL,
                   role TEXT NOT NULL, granted_at_ms INTEGER NOT NULL,
                   granted_by TEXT NOT NULL, revoked_at_ms INTEGER,
                   PRIMARY KEY (machine_id, device_id));
                 CREATE INDEX IF NOT EXISTS machine_trust_device ON machine_trust(device_id);",
            )?;
        }
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// Every trust grant this machine holds (T-0046), including revoked ones.
    ///
    /// Revoked rows are returned rather than filtered: the callers that enforce
    /// need to tell "never had a grant" from "had one and it was cut", and the
    /// callers that display need to show both. Filtering here would make the
    /// distinction unrepresentable at exactly the layer that has to state it.
    pub fn trust_records(&self) -> Result<Vec<crate::mesh::TrustRecord>, SessionError> {
        use crate::identity::{DeviceId, Role};
        use crate::mesh::{MachineId, TrustRecord};
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT machine_id, device_id, role, granted_at_ms, granted_by, revoked_at_ms
             FROM machine_trust ORDER BY machine_id, device_id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (machine_id, device_id, role, granted_at_ms, granted_by, revoked_at_ms) = row?;
            let bad = || SessionError::Sqlite(rusqlite::Error::InvalidQuery);
            out.push(TrustRecord {
                machine_id: MachineId::parse(&machine_id).map_err(|_| bad())?,
                device_id: DeviceId::parse(&device_id).map_err(|_| bad())?,
                role: Role::parse(&role).map_err(|_| bad())?,
                granted_at_ms,
                granted_by: DeviceId::parse(&granted_by).map_err(|_| bad())?,
                revoked_at_ms,
            });
        }
        Ok(out)
    }

    /// Write (or replace) one grant.
    ///
    /// Replacing rather than inserting twice is what makes "grant again" mean
    /// "grant again with this role, live now" — including re-granting a device
    /// whose access was revoked, which is the ordinary way an operator undoes a
    /// revocation. The `granted_by`/`granted_at_ms` of the new grant replace the
    /// old ones; the audit log is where the history lives, not this row.
    pub fn record_trust(&self, record: &crate::mesh::TrustRecord) -> Result<(), SessionError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO machine_trust(machine_id, device_id, role, granted_at_ms, granted_by,
                                       revoked_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(machine_id, device_id) DO UPDATE SET
               role=excluded.role, granted_at_ms=excluded.granted_at_ms,
               granted_by=excluded.granted_by, revoked_at_ms=excluded.revoked_at_ms",
            rusqlite::params![
                record.machine_id.as_str(),
                record.device_id.as_str(),
                record.role.as_str(),
                record.granted_at_ms,
                record.granted_by.as_str(),
                record.revoked_at_ms,
            ],
        )?;
        Ok(())
    }

    /// Revoke one grant. Returns whether a *live* grant was cut, so a caller can
    /// tell "revoked" from "there was nothing to revoke" without a second read.
    pub fn revoke_trust(
        &self,
        machine_id: &crate::mesh::MachineId,
        device_id: &crate::identity::DeviceId,
        at_ms: i64,
    ) -> Result<bool, SessionError> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE machine_trust SET revoked_at_ms = ?3
             WHERE machine_id = ?1 AND device_id = ?2 AND revoked_at_ms IS NULL",
            rusqlite::params![machine_id.as_str(), device_id.as_str(), at_ms],
        )?;
        Ok(changed > 0)
    }

    /// Has this machine's trust ledger ever been initialized (T-0046)?
    ///
    /// A one-way marker, and the reason it is stored rather than inferred: an
    /// operator who revokes every device leaves a legitimately empty ledger, and
    /// a backfill that ran again would silently restore the access that was just
    /// taken away. Absent or unparsable reads as "not yet", which is the safe
    /// direction — it backfills a machine that has never run this code, and a
    /// marker anyone can clear by hand is a marker an operator can re-run.
    pub fn trust_initialized(&self) -> Result<bool, SessionError> {
        let conn = self.lock()?;
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM meta WHERE key='trust_initialized'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value.as_deref() == Some("1"))
    }

    /// Record that the trust ledger has been initialized, once and for all.
    pub fn mark_trust_initialized(&self) -> Result<(), SessionError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('trust_initialized', '1')
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [],
        )?;
        Ok(())
    }

    /// Persist a device row (public material only). Idempotent by id.
    pub fn upsert_device(
        &self,
        device: &crate::identity::DeviceRecord,
    ) -> Result<(), SessionError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO devices(id, name, role, public_key, serial, issued_at_ms,
                                 last_seen_ms, revoked, retired_to, revoked_at, revoked_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
             ON CONFLICT(id) DO UPDATE SET
               name=excluded.name, role=excluded.role, public_key=excluded.public_key,
               serial=excluded.serial, issued_at_ms=excluded.issued_at_ms,
               last_seen_ms=excluded.last_seen_ms, revoked=excluded.revoked,
               retired_to=excluded.retired_to,
               -- `COALESCE` on the way in: a later upsert of a still-revoked
               -- device must not erase who revoked it and when. Losing that on a
               -- routine `touch` (last_seen) would make the audit trail rot.
               revoked_at=COALESCE(devices.revoked_at, excluded.revoked_at),
               revoked_by=COALESCE(devices.revoked_by, excluded.revoked_by)",
            params![
                device.id.as_str(),
                device.name,
                device.role.as_str(),
                device.public_hex(),
                device.serial as i64,
                device.issued_at_ms,
                device.last_seen_ms,
                device.revoked as i64,
                device.retired_to.as_ref().map(|id| id.as_str()),
                device.revoked_at_ms,
                device.revoked_by.as_deref(),
            ],
        )?;
        Ok(())
    }

    /// Every device, newest issuance first is not useful here — id order is
    /// stable, which is what the CLI and tests want.
    pub fn devices(&self) -> Result<Vec<crate::identity::DeviceRecord>, SessionError> {
        use crate::identity::{DeviceId, Role};
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT id, name, role, public_key, serial, issued_at_ms, last_seen_ms,
                    revoked, retired_to, revoked_at, revoked_by
             FROM devices ORDER BY id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<i64>>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (
                id,
                name,
                role,
                public_key,
                serial,
                issued_at_ms,
                last_seen_ms,
                revoked,
                retired,
                revoked_at_ms,
                revoked_by,
            ) = row?;
            let role = Role::parse(&role)
                .map_err(|_| SessionError::Sqlite(rusqlite::Error::InvalidQuery))?;
            let id = DeviceId::parse(&id)
                .map_err(|_| SessionError::Sqlite(rusqlite::Error::InvalidQuery))?;
            let mut public = [0u8; 32];
            for (index, byte) in public.iter_mut().enumerate() {
                let pair = public_key
                    .get(index * 2..index * 2 + 2)
                    .ok_or(SessionError::Sqlite(rusqlite::Error::InvalidQuery))?;
                *byte = u8::from_str_radix(pair, 16)
                    .map_err(|_| SessionError::Sqlite(rusqlite::Error::InvalidQuery))?;
            }
            out.push(crate::identity::DeviceRecord {
                id,
                name,
                role,
                public_key: public,
                serial: serial as u64,
                issued_at_ms,
                last_seen_ms,
                revoked: revoked != 0,
                revoked_at_ms,
                revoked_by,
                retired_to: match retired {
                    Some(text) => Some(
                        DeviceId::parse(&text)
                            .map_err(|_| SessionError::Sqlite(rusqlite::Error::InvalidQuery))?,
                    ),
                    None => None,
                },
            });
        }
        Ok(out)
    }

    /// Revoke a device, recording who did it and when — in one statement, so a
    /// revocation can never be durable without its provenance.
    ///
    /// The `WHERE revoked = 0` is what makes the call *idempotent* and
    /// self-describing: it reports whether this call is the one that revoked the
    /// device, so a second run can say "already revoked" rather than pretending
    /// to have done something. It also refuses to overwrite the original
    /// timestamp, because the interesting moment is the first one.
    pub fn revoke_device(
        &self,
        id: &str,
        revoked_by: &str,
        now_ms: i64,
    ) -> Result<bool, SessionError> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE devices SET revoked = 1, revoked_at = ?3, revoked_by = ?2
             WHERE id = ?1 AND revoked = 0",
            params![id, revoked_by, now_ms],
        )?;
        Ok(changed > 0)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>, SessionError> {
        self.conn
            .lock()
            .map_err(|_| SessionError::Sqlite(rusqlite::Error::InvalidQuery))
    }

    /// Open (or create + migrate) a file store.
    pub fn open(path: &std::path::Path) -> Result<Self, SessionError> {
        let conn = Connection::open(path)?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    /// In-memory store (tests).
    pub fn open_memory() -> Result<Self, SessionError> {
        let conn = Connection::open_in_memory()?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    pub fn schema_version(&self) -> Result<u32, SessionError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| SessionError::Sqlite(rusqlite::Error::InvalidQuery))?;
        let value: String = conn.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        Ok(value.parse().unwrap_or(0))
    }

    /// Replace the whole topology snapshot (spawn/exit/drain-tick callers).
    /// Scrollback lines are stored in order (line_no = index).
    pub fn save_topology(&self, panes: &[StoredPane]) -> Result<(), SessionError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| SessionError::Sqlite(rusqlite::Error::InvalidQuery))?;
        // Whole-snapshot replace inside one transaction: readers never see
        // a half-written topology.
        let tx_conn: &Connection = &conn;
        tx_conn.execute("DELETE FROM scrollback", [])?;
        tx_conn.execute("DELETE FROM panes", [])?;
        for pane in panes {
            tx_conn.execute(
                "INSERT INTO panes(id, program, args, cols, rows) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    pane.id,
                    pane.program,
                    serde_json::to_string(&pane.args).map_err(SessionError::Json)?,
                    pane.cols as i64,
                    pane.rows as i64,
                ],
            )?;
            for (line_no, text) in pane.scrollback.iter().enumerate() {
                tx_conn.execute(
                    "INSERT INTO scrollback(pane, line_no, text) VALUES (?1, ?2, ?3)",
                    params![pane.id, line_no as i64, text],
                )?;
            }
        }
        Ok(())
    }

    /// Load the topology snapshot, panes ordered by id, scrollback in order.
    pub fn load_topology(&self) -> Result<Vec<StoredPane>, SessionError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| SessionError::Sqlite(rusqlite::Error::InvalidQuery))?;
        let mut stmt =
            conn.prepare("SELECT id, program, args, cols, rows FROM panes ORDER BY id ASC")?;
        let panes = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for pane in panes {
            let (id, program, args_json, cols, rows) = pane?;
            let args: Vec<String> = serde_json::from_str(&args_json).unwrap_or_default();
            let mut lines =
                conn.prepare("SELECT text FROM scrollback WHERE pane = ?1 ORDER BY line_no ASC")?;
            let scrollback = lines
                .query_map(params![id], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            out.push(StoredPane {
                id,
                program,
                args,
                cols: cols as u16,
                rows: rows as u16,
                scrollback,
            });
        }
        Ok(out)
    }

    /// Append one audit event (redacting secret-shaped content first).
    /// There is deliberately NO update/delete API — append-only by construction.
    /// Append one audit row. **The** writer: the schema has one shape, so it has
    /// one method, and a field cannot go missing because a caller used a variant
    /// that filled a different subset of the columns.
    ///
    /// Redaction happens here, before the write, which is what makes "no flag can
    /// un-redact" true: the secret scan runs on the way in, and the peer address
    /// is truncated on the way in, so neither the plaintext nor the full address
    /// ever reaches the file.
    pub fn record(&self, event: &AuditEvent) -> Result<(), SessionError> {
        let (prompt, redacted) = redact(&event.prompt);
        let peer = event.peer.as_deref().map(redact_peer_text);
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO audit(ts_ms, device, agent, prompt, redacted, kind, action,
                               outcome, peer, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                event.ts_ms as i64,
                event.device,
                event.agent,
                prompt,
                redacted as i64,
                event.kind.as_str(),
                event.action,
                event.outcome.as_str(),
                peer,
                event.detail,
            ],
        )?;
        Ok(())
    }

    /// Audit rows, newest last (the order a review reads them in), filtered.
    ///
    /// `since_ms`/`until_ms` bound the window; `action` narrows to one event
    /// name. Ordered by `(ts_ms, rowid)` rather than `ts_ms` alone: a clock that
    /// steps backwards (NTP, a suspended VM) must not reorder history, and the
    /// rowid is the only monotonic thing the table has.
    pub fn audit_query(&self, query: &AuditQuery) -> Result<Vec<StoredAudit>, SessionError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT ts_ms, device, agent, prompt, redacted, kind, action, outcome, peer, detail
             FROM audit
             WHERE (?1 IS NULL OR ts_ms >= ?1)
               AND (?2 IS NULL OR ts_ms <= ?2)
               AND (?3 IS NULL OR action = ?3)
             ORDER BY ts_ms ASC, rowid ASC
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                query.since_ms.map(clamp_ms),
                query.until_ms.map(clamp_ms),
                query.action.as_deref(),
                clamp_limit(query.limit),
            ],
            read_audit_row,
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Newest audit rows first (operator reads the tail), limited.
    pub fn audit_recent(&self, limit: usize) -> Result<Vec<StoredAudit>, SessionError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT ts_ms, device, agent, prompt, redacted, kind, action, outcome, peer, detail
             FROM audit ORDER BY ts_ms DESC, rowid DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], read_audit_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Every row with a given action, newest first — the operator's "show me
    /// every revocation" query.
    pub fn audit_by_action(
        &self,
        action: &str,
        limit: usize,
    ) -> Result<Vec<StoredAudit>, SessionError> {
        self.audit_query(&AuditQuery {
            action: Some(action.to_string()),
            ..AuditQuery::all(limit)
        })
    }

    /// Export the audit log, oldest first, as JSON lines or a JSON array.
    ///
    /// The same filters as [`SessionStore::audit_query`], and the same rows: the
    /// export is a *view*, never a second source of truth, so an export of one
    /// window twice is byte-identical.
    pub fn audit_export(
        &self,
        query: &AuditQuery,
        format: ExportFormat,
    ) -> Result<String, SessionError> {
        let events = self.audit_query(query)?;
        let values: Vec<serde_json::Value> = events.iter().map(audit_json).collect();
        match format {
            ExportFormat::Jsonl => {
                let mut out = String::new();
                for value in &values {
                    out.push_str(&serde_json::to_string(value).map_err(SessionError::Json)?);
                    out.push('\n');
                }
                Ok(out)
            }
            ExportFormat::Json => {
                let array = serde_json::Value::Array(values);
                let mut out = serde_json::to_string_pretty(&array).map_err(SessionError::Json)?;
                out.push('\n');
                Ok(out)
            }
        }
    }

    /// How many rows the log holds, and roughly how many bytes — the numbers the
    /// size guard reports.
    pub fn audit_size(&self) -> Result<(u64, u64), SessionError> {
        let conn = self.lock()?;
        let (rows, bytes): (i64, i64) = conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(LENGTH(device) + LENGTH(agent) + LENGTH(prompt)
                                          + LENGTH(action) + COALESCE(LENGTH(peer), 0)
                                          + COALESCE(LENGTH(detail), 0)), 0)
             FROM audit",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok((rows.max(0) as u64, bytes.max(0) as u64))
    }

    /// Delete rows older than `before_ms`, returning how many went.
    ///
    /// **Never automatic.** Nothing in this product prunes the audit log on a
    /// timer: an append-only log that quietly deletes itself is not an audit log,
    /// and the whole point of §4's trail is that it outlives the session that
    /// wrote it. The operator asks, and the prune is itself recorded — a deletion
    /// the log does not mention would be the one hole in it.
    pub fn audit_prune(&self, before_ms: u64, now_ms: u64) -> Result<u64, SessionError> {
        let removed = {
            let conn = self.lock()?;
            conn.execute(
                "DELETE FROM audit WHERE ts_ms < ?1 AND action != ?2",
                params![clamp_ms(before_ms), actions::PRUNE],
            )? as u64
        };
        // Recorded after the delete, and excluded from it, so the prune row
        // always survives the prune that wrote it.
        self.record(&AuditEvent {
            ts_ms: now_ms,
            action: actions::PRUNE.to_string(),
            kind: AuditKind::Unknown,
            outcome: AuditOutcome::Ok,
            device: crate::identity::revocation::LOCAL_CLI.to_string(),
            agent: String::new(),
            prompt: format!("removed {removed} row(s) older than {before_ms}"),
            peer: None,
            detail: Some(format!("before_ms={before_ms} removed={removed}")),
        })?;
        Ok(removed)
    }

    /// Record one metrics sample at `ts_ms`, floored to `step_ms` (T-0040).
    ///
    /// The floor is what makes re-running a rollup idempotent: two writers
    /// recording the same bucket land on the same `(pane, ts_ms, step_ms)` key,
    /// and `INSERT OR REPLACE` keeps the later write rather than duplicating
    /// the row. Peak RSS is the max of the two writes' peaks, so a re-run can
    /// only keep the worst moment, never lose it.
    pub fn metrics_record(&self, sample: &MetricsSample) -> Result<(), SessionError> {
        let conn = self.lock()?;
        let bucket = sample.ts_ms - (sample.ts_ms % sample.step_ms.max(1));
        // A fresh row carries the sample's own weight (rollups arrive with the
        // tier below's count already summed); a re-run merges by weight rather
        // than by row, so a rollup re-run keeps the same total.
        let weight = sample.samples.max(1) as i64;
        conn.execute(
            "INSERT INTO metrics_series(pane, ts_ms, step_ms, rss_avg, rss_peak,
                                        cpu_avg, cpu_peak, pids_avg, pids_peak, samples)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(pane, ts_ms, step_ms) DO UPDATE SET
               rss_avg = (rss_avg * samples + excluded.rss_avg * excluded.samples) / (samples + excluded.samples),
               rss_peak = MAX(rss_peak, excluded.rss_peak),
               cpu_avg = (cpu_avg * samples + excluded.cpu_avg * excluded.samples) / (samples + excluded.samples),
               cpu_peak = MAX(cpu_peak, excluded.cpu_peak),
               pids_avg = (pids_avg * samples + excluded.pids_avg * excluded.samples) / (samples + excluded.samples),
               pids_peak = MAX(pids_peak, excluded.pids_peak),
               samples = samples + excluded.samples",
            params![
                sample.pane,
                bucket as i64,
                sample.step_ms as i64,
                sample.rss_avg as i64,
                sample.rss_peak as i64,
                sample.cpu_avg,
                sample.cpu_peak,
                sample.pids_avg,
                sample.pids_peak as i64,
                weight,
            ],
        )?;
        Ok(())
    }

    /// One indexed range scan on `(pane, ts_ms)` at exactly `step_ms` (T-0040).
    ///
    /// Oldest-first (chart order). No interpolation and no cross-step blending:
    /// a caller that wants a coarser step rolls up from this tier itself, and a
    /// caller that asks finer than available is told to downshift by
    /// [`metrics_step_for`] rather than receiving an empty series.
    pub fn metrics_range(
        &self,
        pane: &str,
        since_ms: u64,
        until_ms: u64,
        step_ms: u64,
    ) -> Result<Vec<MetricsRow>, SessionError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT ts_ms, rss_avg, rss_peak, cpu_avg, cpu_peak,
                    pids_avg, pids_peak, samples
             FROM metrics_series
             WHERE pane = ?1 AND step_ms = ?2 AND ts_ms >= ?3 AND ts_ms <= ?4
             ORDER BY ts_ms ASC",
        )?;
        let rows = stmt.query_map(
            params![pane, step_ms as i64, clamp_ms(since_ms), clamp_ms(until_ms)],
            |row| {
                Ok(MetricsRow {
                    ts_ms: row.get::<_, i64>(0)?.max(0) as u64,
                    rss_avg: row.get::<_, i64>(1)?.max(0) as u64,
                    rss_peak: row.get::<_, i64>(2)?.max(0) as u64,
                    cpu_avg: row.get::<_, f64>(3)?,
                    cpu_peak: row.get::<_, f64>(4)?,
                    pids_avg: row.get::<_, f64>(5)?,
                    pids_peak: row.get::<_, i64>(6)?.max(0) as u64,
                    samples: row.get::<_, i64>(7)?.max(0) as u64,
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Roll one tier into the next coarser one (T-0040): every `step_ms` bucket
    /// in `[since_ms, until_ms]` becomes one row at `into_step_ms`, aligned to
    /// the coarser boundary.
    ///
    /// Reads go through [`SessionStore::metrics_range`] and writes through
    /// [`SessionStore::metrics_record`], so a rollup is a pure function of the
    /// tier below — never a re-sample of `/proc`, which is what keeps a pane
    /// restored late from leaving a hole where the underlying row exists.
    /// Returns rows written.
    pub fn metrics_rollup(
        &self,
        pane: &str,
        since_ms: u64,
        until_ms: u64,
        step_ms: u64,
        into_step_ms: u64,
    ) -> Result<u64, SessionError> {
        let rows = self.metrics_range(pane, since_ms, until_ms, step_ms)?;
        let mut buckets: std::collections::BTreeMap<u64, Vec<MetricsRow>> =
            std::collections::BTreeMap::new();
        for row in rows {
            buckets
                .entry(row.ts_ms - (row.ts_ms % into_step_ms))
                .or_default()
                .push(row);
        }
        let mut written = 0u64;
        for (bucket, members) in buckets {
            if members.is_empty() {
                continue;
            }
            let total_samples: u64 = members.iter().map(|m| m.samples.max(1)).sum();
            let weight = total_samples.max(1) as f64;
            let rss_avg = (members
                .iter()
                .map(|m| m.rss_avg as f64 * m.samples.max(1) as f64)
                .sum::<f64>()
                / weight) as u64;
            let cpu_avg = members
                .iter()
                .map(|m| m.cpu_avg * m.samples.max(1) as f64)
                .sum::<f64>()
                / weight;
            let pids_avg = members
                .iter()
                .map(|m| m.pids_avg * m.samples.max(1) as f64)
                .sum::<f64>()
                / weight;
            self.metrics_record(&MetricsSample {
                pane: pane.to_string(),
                ts_ms: bucket,
                step_ms: into_step_ms,
                rss_avg,
                rss_peak: members.iter().map(|m| m.rss_peak).max().unwrap_or(0),
                cpu_avg,
                cpu_peak: members
                    .iter()
                    .map(|m| ordered_f64(m.cpu_peak))
                    .max()
                    .map(unorder_f64)
                    .unwrap_or(0.0),
                pids_avg,
                pids_peak: members.iter().map(|m| m.pids_peak).max().unwrap_or(0),
                samples: total_samples,
            })?;
            written += 1;
        }
        Ok(written)
    }

    /// Hourly prune tick (T-0040): drop every tier's rows older than its
    /// retention, and never the newest row of a live pane.
    ///
    /// "Live" is decided by the caller through `live_panes`: the store cannot
    /// know which panes are running, and a graph must never go empty because a
    /// tick ran while its pane was briefly between samples. Returns rows
    /// removed, per tier in `(step_ms, removed)` order.
    pub fn metrics_prune(
        &self,
        now_ms: u64,
        live_panes: &[&str],
    ) -> Result<Vec<(u64, u64)>, SessionError> {
        let mut out = Vec::new();
        for (step_ms, retention_ms) in metrics_retention() {
            let cutoff = now_ms.saturating_sub(retention_ms) as i64;
            let conn = self.lock()?;
            let removed = conn.execute(
                "DELETE FROM metrics_series WHERE step_ms = ?1 AND ts_ms < ?2
                 AND NOT (pane IN (SELECT value FROM json_each(?3))
                          AND ts_ms = (SELECT MAX(ts_ms) FROM metrics_series AS keep
                                       WHERE keep.pane = metrics_series.pane
                                         AND keep.step_ms = metrics_series.step_ms))",
                params![
                    step_ms as i64,
                    cutoff,
                    serde_json::json!(live_panes).to_string()
                ],
            )? as u64;
            out.push((step_ms, removed));
        }
        Ok(out)
    }

    /// Bytes the series holds for one pane (T-0040's ≤ 2 MB bar, asserted not
    /// assumed).
    pub fn metrics_bytes(&self, pane: &str) -> Result<u64, SessionError> {
        let conn = self.lock()?;
        let bytes: i64 = conn.query_row(
            "SELECT COALESCE(SUM(LENGTH(pane) + 48), 0) FROM metrics_series WHERE pane = ?1",
            params![pane],
            |row| row.get(0),
        )?;
        Ok(bytes.max(0) as u64)
    }
}

/// A millisecond bound as SQLite's signed integer, clamped.
///
/// Timestamps are `u64` in Rust and `i64` in SQLite, and a naive `as i64` turns
/// `u64::MAX` — the natural "everything" bound — into `-1`, which matches
/// nothing: a prune asked to remove everything would silently remove nothing.
/// Clamping is the honest conversion, because the two ranges agree everywhere
/// below the epoch-plus-292-million-years point where a millisecond timestamp
/// stops being meaningful anyway.
fn clamp_ms(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

/// A row limit as SQLite's signed integer, clamped.
///
/// The mirror of [`clamp_ms`] and for the same reason: `usize::MAX as i64` is
/// `-1`, and SQLite reads a negative `LIMIT` as "no limit" — so the value that
/// most clearly means "everything" would take a code path nobody intended.
fn clamp_limit(limit: usize) -> i64 {
    limit.min(i64::MAX as usize) as i64
}

/// One metrics sample to record (T-0040): average and peak RSS (peak is the
/// number people act on) plus cpu and pids, at `step_ms` granularity.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsSample {
    pub pane: String,
    pub ts_ms: u64,
    pub step_ms: u64,
    pub rss_avg: u64,
    pub rss_peak: u64,
    pub cpu_avg: f64,
    pub cpu_peak: f64,
    pub pids_avg: f64,
    pub pids_peak: u64,
    /// How many raw samples this row aggregates (1 for a fresh 10 s row).
    pub samples: u64,
}

/// One stored series row, oldest-first out of [`SessionStore::metrics_range`].
#[derive(Debug, Clone, PartialEq)]
pub struct MetricsRow {
    pub ts_ms: u64,
    pub rss_avg: u64,
    pub rss_peak: u64,
    pub cpu_avg: f64,
    pub cpu_peak: f64,
    pub pids_avg: f64,
    pub pids_peak: u64,
    pub samples: u64,
}

/// The three tiers (T-0040): `(step_ms, retention_ms)`. 10 s rows kept 24 h,
/// 1 m rollups 30 d, 1 h rollups 365 d.
#[must_use]
pub fn metrics_retention() -> [(u64, u64); 3] {
    [
        (10_000, 24 * 60 * 60 * 1000),
        (60_000, 30 * 24 * 60 * 60 * 1000),
        (3_600_000, 365 * 24 * 60 * 60 * 1000),
    ]
}

/// The step a query over `[since_ms, until_ms]` should read (T-0040): the
/// finest tier whose retention covers the whole window, so asking finer than
/// available downshifts to the nearest real step instead of returning empty.
#[must_use]
pub fn metrics_step_for(since_ms: u64, until_ms: u64) -> (u64, bool) {
    let span = until_ms.saturating_sub(since_ms);
    for (step_ms, retention_ms) in metrics_retention() {
        if span <= retention_ms {
            return (step_ms, false);
        }
    }
    (3_600_000, true)
}

/// Total order for f64 peaks: the bit pattern, with the sign bit flipped so
/// negatives sort below positives. NaN sorts high — a sensor that reported NaN
/// is a finding, not a maximum, and must not become one silently.
fn ordered_f64(value: f64) -> u64 {
    let bits = value.to_bits();
    if bits >> 63 == 0 {
        bits ^ 0x8000_0000_0000_0000
    } else {
        !bits
    }
}

fn unorder_f64(order: u64) -> f64 {
    let bits = if order >> 63 == 1 {
        order ^ 0x8000_0000_0000_0000
    } else {
        !order
    };
    f64::from_bits(bits)
}

/// What to read out of the audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditQuery {
    pub since_ms: Option<u64>,
    pub until_ms: Option<u64>,
    pub action: Option<String>,
    pub limit: usize,
}

impl AuditQuery {
    /// Everything, capped at `limit`.
    #[must_use]
    pub fn all(limit: usize) -> Self {
        Self {
            since_ms: None,
            until_ms: None,
            action: None,
            limit,
        }
    }
}

/// How an export is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// One JSON object per line — what a log pipeline eats.
    Jsonl,
    /// A JSON array, pretty-printed — what a human reads.
    Json,
}

impl ExportFormat {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Jsonl => "jsonl",
            Self::Json => "json",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "jsonl" => Some(Self::Jsonl),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// An epoch-millisecond instant as RFC3339 UTC (`2026-09-11T09:30:00Z`).
///
/// **Why a formatter and not a date library.** `chrono` and `time` are
/// dependencies for one conversion — days-to-civil, which is twenty lines of
/// well-known arithmetic (Howard Hinnant's `civil_from_days`). A dependency
/// that large, for a value that appears in one JSON field, is the scaffolding
/// AGENTS.md forbids; and the arithmetic is exactly the kind of thing that can
/// be tested against known instants, which the tests below do.
///
/// **Why RFC3339 for a script contract at all.** The audit ledger's rows carry
/// `ts_ms` (an integer) because they are read by machines that compare numbers.
/// A directory listing is read by humans and by scripts, and `as_of` answers
/// "how stale is this" at a glance — which a millisecond count does not. The
/// integer form travels beside it (`age_secs`) for anyone who wants to compare.
#[must_use]
pub fn rfc3339_ms(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let day_secs = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let (hour, minute, second) = (day_secs / 3600, (day_secs % 3600) / 60, day_secs % 60);
    let mut out = format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}");
    if millis != 0 {
        out.push_str(&format!(".{millis:03}"));
    }
    out.push('Z');
    out
}

/// Days since 1970-01-01 to a civil `(year, month, day)`, proleptic Gregorian.
///
/// Hinnant's algorithm: shift the epoch to 0000-03-01 so leap days land at the
/// end of the year, which makes the month lengths periodic and the whole
/// conversion branch-free apart from the era adjustment.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// One row as the export and `--json` readers see it. Field names are the
/// contract a script parses, so they are snake_case and stable.
#[must_use]
pub fn audit_json(event: &StoredAudit) -> serde_json::Value {
    serde_json::json!({
        "ts_ms": event.ts_ms,
        "action": event.action,
        "kind": event.kind.as_str(),
        "outcome": event.outcome.as_str(),
        "device": event.device,
        "agent": event.agent,
        "prompt": event.prompt,
        "redacted": event.redacted,
        "peer": event.peer,
        "detail": event.detail,
    })
}

/// Read one audit row. Shared by every reader so a new column cannot be filled
/// in one query and left empty in another.
fn read_audit_row(row: &rusqlite::Row<'_>) -> Result<StoredAudit, rusqlite::Error> {
    Ok(StoredAudit {
        ts_ms: row.get::<_, i64>(0)? as u64,
        device: row.get(1)?,
        agent: row.get(2)?,
        prompt: row.get(3)?,
        redacted: row.get::<_, i64>(4)? != 0,
        kind: AuditKind::parse(&row.get::<_, String>(5)?),
        action: row.get(6)?,
        outcome: AuditOutcome::parse(&row.get::<_, String>(7)?),
        peer: row.get(8)?,
        detail: row.get(9)?,
    })
}

/// Truncate a peer that arrives as text (already-stored values, or a caller that
/// has the address in string form).
#[must_use]
pub fn redact_peer_text(peer: &str) -> String {
    match peer.parse::<std::net::SocketAddr>() {
        Ok(addr) => truncate_peer(&addr),
        // Not an address: keep the shape but drop anything that looks specific.
        Err(_) => peer.to_string(),
    }
}

/// Mask every secret-shaped token in the line.
fn mask_tokens(line: &str) -> (String, bool) {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    let mut masked = false;
    while let Some((at, len, _)) = crate::fixtures::find_token(rest) {
        out.push_str(&rest[..at]);
        out.push_str("[REDACTED:token]");
        rest = &rest[at + len..];
        masked = true;
    }
    out.push_str(rest);
    (out, masked)
}

/// Redact secret-shaped substrings, keeping field names for debugging.
/// Returns (redacted_text, was_redacted).
fn redact(prompt: &str) -> (String, bool) {
    let findings = scan_secrets(prompt);
    if findings.is_empty() {
        return (prompt.to_string(), false);
    }
    // Redact per line: replace the VALUE side of assignments and known
    // token shapes, keep the key names. Simple + auditable over clever.
    let mut redacted_any = false;
    let out: Vec<String> = prompt
        .lines()
        .map(|line| {
            let mut line = line.to_string();
            // sk-/AKIA/ghp_/gho_/xox tokens, wherever they sit in the line.
            let (masked, did) = mask_tokens(&line);
            if did {
                line = masked;
                redacted_any = true;
            }
            // KEY=VALUE assignments: mask values longer than 8 chars.
            for sep in ['=', ':'] {
                if let Some(pos) = line.find(sep) {
                    let (key, value) = line.split_at(pos + 1);
                    let value = value.trim();
                    // Two named cases rather than one chained condition:
                    // `a && b && c || d` reads as "all three, or d", which
                    // quietly stopped the length rule from applying to the
                    // secret/password branches.
                    let lower = key.to_lowercase();
                    let names_a_secret = ["secret", "password", "passwd"]
                        .iter()
                        .any(|word| lower.contains(word));
                    let long_enough = !value.is_empty() && value.len() >= 8;
                    if !value.is_empty()
                        && (names_a_secret || (long_enough && lower.contains("key")))
                    {
                        line = format!("{key}[REDACTED:value]");
                        redacted_any = true;
                    }
                }
            }
            // Private key blocks: the scan flagged the line; mask it wholly
            // except a label.
            if line.contains("BEGIN") && line.contains("PRIVATE KEY") {
                line = "[REDACTED:private-key-block]".to_string();
                redacted_any = true;
            }
            line
        })
        .collect();
    // The scan flagged something and no rule claimed it. Storing the line as it
    // arrived would make `redacted = 1` a statement about the scanner rather
    // than about the bytes, and a row that says "redacted" while holding the
    // secret is worse than either extreme. So the fallback is the safe one.
    if !redacted_any {
        return (
            prompt
                .lines()
                .map(|_| "[REDACTED:secret]".to_string())
                .collect::<Vec<_>>()
                .join("\n"),
            true,
        );
    }
    (out.join("\n"), true)
}

#[cfg(test)]
mod timestamp_tests {
    use super::rfc3339_ms;

    /// Known instants, including the ones a naive implementation gets wrong:
    /// the epoch, a leap day, a century that is not a leap year, a negative
    /// instant, and a millisecond remainder.
    #[test]
    fn rfc3339_matches_known_instants() {
        assert_eq!(rfc3339_ms(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_ms(1_000), "1970-01-01T00:00:01Z");
        assert_eq!(rfc3339_ms(1_500), "1970-01-01T00:00:01.500Z");
        assert_eq!(rfc3339_ms(-1), "1969-12-31T23:59:59.999Z");
        // 2000-02-29: a leap day in a leap century.
        assert_eq!(rfc3339_ms(951_782_400_000), "2000-02-29T00:00:00Z");
        // 2100-03-01: 2100 is *not* a leap year, so February has 28 days.
        assert_eq!(rfc3339_ms(4_107_542_400_000), "2100-03-01T00:00:00Z");
        // 2026-09-11T09:30:00Z, the day this was written.
        assert_eq!(rfc3339_ms(1_789_119_000_000), "2026-09-11T09:30:00Z");
    }

    /// The conversion is monotonic over a span that crosses leap days, years and
    /// the epoch: a formatter that is off by one day somewhere shows up here.
    #[test]
    fn rfc3339_is_monotonic_and_round_trips_through_days() {
        let mut previous = String::new();
        let mut ms = -100_000_000_000i64;
        while ms < 100_000_000_000 {
            let text = rfc3339_ms(ms);
            if !previous.is_empty() {
                assert!(text > previous, "{text} must follow {previous}");
            }
            previous = text;
            ms += 3_600_000;
        }
    }
}

#[cfg(test)]
mod trust_store_tests {
    use crate::identity::{DeviceId, Role};
    use crate::mesh::{MachineId, TrustRecord};
    use crate::store::SessionStore;

    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "arreo-trust-store-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.join("store.db")
    }

    fn machine(hex: char) -> MachineId {
        MachineId::parse(&hex.to_string().repeat(32)).expect("machine id")
    }

    fn device(hex: char) -> DeviceId {
        DeviceId::parse(&hex.to_string().repeat(32)).expect("device id")
    }

    fn grant(on: &MachineId, to: &DeviceId, role: Role, by: &DeviceId) -> TrustRecord {
        TrustRecord {
            machine_id: on.clone(),
            device_id: to.clone(),
            role,
            granted_at_ms: 1_000,
            granted_by: by.clone(),
            revoked_at_ms: None,
        }
    }

    /// **The criterion T-0046 exists for.** A grant made on machine A leaves
    /// machine B's grant set empty — not "present but unevaluated", exactly zero
    /// rows — because the machine is part of the primary key.
    #[test]
    fn a_grant_on_one_machine_is_nothing_on_another() {
        let store = SessionStore::open(&scratch("isolation")).expect("store");
        let a = machine('a');
        let b = machine('b');
        let phone = device('1');

        store
            .record_trust(&grant(&a, &phone, Role::Owner, &device('2')))
            .expect("grant on A");

        let all = store.trust_records().expect("read");
        assert_eq!(all.len(), 1, "one grant was made: {all:?}");
        assert_eq!(all[0].machine_id, a);

        let on_b: Vec<_> = all.iter().filter(|r| r.machine_id == b).collect();
        assert!(
            on_b.is_empty(),
            "machine B must have no rows at all after A's grant: {on_b:?}"
        );
        // And the rule agrees: reading B's grant for that device finds nothing,
        // which is a refusal rather than a permissive default.
        let for_b = all
            .iter()
            .find(|r| r.machine_id == b && r.device_id == phone);
        assert!(for_b.is_none());
        assert!(crate::mesh::evaluate(for_b, crate::identity::role::Verb::Read).is_err());
    }

    /// Granting twice is a replacement, not a second row — including re-granting
    /// a device whose access was revoked, which is how an operator undoes one.
    #[test]
    fn granting_again_replaces_and_revives() {
        let store = SessionStore::open(&scratch("replace")).expect("store");
        let on = machine('c');
        let phone = device('3');
        let operator = device('4');

        store
            .record_trust(&grant(&on, &phone, Role::Viewer, &operator))
            .expect("first grant");
        let mut upgraded = grant(&on, &phone, Role::Owner, &operator);
        upgraded.granted_at_ms = 2_000;
        store.record_trust(&upgraded).expect("re-grant");

        let rows = store.trust_records().expect("read");
        assert_eq!(rows.len(), 1, "one machine, one device, one row: {rows:?}");
        assert_eq!(rows[0].role, Role::Owner);
        assert_eq!(rows[0].granted_at_ms, 2_000);

        // Revoke, then grant again: the row goes live, and the revoked stamp is
        // cleared rather than left behind to make a live row look revoked.
        assert!(store.revoke_trust(&on, &phone, 3_000).expect("revoke"));
        let rows = store.trust_records().expect("read");
        assert_eq!(rows[0].revoked_at_ms, Some(3_000));
        assert!(!rows[0].is_live());

        store
            .record_trust(&grant(&on, &phone, Role::Owner, &operator))
            .expect("re-grant after revoke");
        let rows = store.trust_records().expect("read");
        assert!(rows[0].is_live(), "a re-grant revives the row: {rows:?}");
        assert_eq!(rows.len(), 1);
    }

    /// Revoking reports whether it cut something live, so a caller can say
    /// "revoked" rather than "there was nothing to revoke" — and revoking twice
    /// does not rewrite the first revocation's timestamp.
    #[test]
    fn revoking_is_idempotent_and_keeps_the_first_timestamp() {
        let store = SessionStore::open(&scratch("revoke")).expect("store");
        let on = machine('d');
        let phone = device('5');
        store
            .record_trust(&grant(&on, &phone, Role::Owner, &device('6')))
            .expect("grant");

        assert!(store.revoke_trust(&on, &phone, 7_000).expect("revoke"));
        assert!(
            !store
                .revoke_trust(&on, &phone, 9_000)
                .expect("revoke again"),
            "the second revoke cut nothing live"
        );
        let rows = store.trust_records().expect("read");
        assert_eq!(
            rows[0].revoked_at_ms,
            Some(7_000),
            "the first revocation is when access ended"
        );

        // Revoking a device that was never granted changes nothing and says so.
        assert!(!store
            .revoke_trust(&on, &device('7'), 9_000)
            .expect("unknown device"));
    }

    /// A store on disk keeps its grants across a reopen — the row is durable
    /// state, not a cache of the running process.
    #[test]
    fn grants_survive_a_reopen() {
        let path = scratch("durable");
        let on = machine('e');
        let phone = device('8');
        {
            let store = SessionStore::open(&path).expect("store");
            store
                .record_trust(&grant(&on, &phone, Role::Viewer, &device('9')))
                .expect("grant");
        }
        let store = SessionStore::open(&path).expect("reopen");
        let rows = store.trust_records().expect("read");
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].machine_id, on);
        assert_eq!(rows[0].device_id, phone);
        assert_eq!(rows[0].role, Role::Viewer);
        assert!(rows[0].is_live());
    }
}
