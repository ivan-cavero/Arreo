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

use rusqlite::{params, Connection};
use thiserror::Error;

use crate::fixtures::scan_secrets;

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("session json: {0}")]
    Json(#[from] serde_json::Error),
}

pub const SCHEMA_VERSION: u32 = 3;

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

/// One audit row (prompt already redacted on write).
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEvent {
    pub ts_ms: u64,
    pub device: String,
    pub agent: String,
    pub prompt: String,
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
                                 last_seen_ms, revoked, retired_to)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(id) DO UPDATE SET
               name=excluded.name, role=excluded.role, public_key=excluded.public_key,
               serial=excluded.serial, issued_at_ms=excluded.issued_at_ms,
               last_seen_ms=excluded.last_seen_ms, revoked=excluded.revoked,
               retired_to=excluded.retired_to",
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
                    revoked, retired_to
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
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (id, name, role, public_key, serial, issued_at_ms, last_seen_ms, revoked, retired) =
                row?;
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

    /// Flip a device's revoked flag (T-0026 owns the propagation story; this
    /// is the durable bit it needs).
    pub fn set_device_revoked(&self, id: &str, revoked: bool) -> Result<bool, SessionError> {
        let conn = self.lock()?;
        let changed = conn.execute(
            "UPDATE devices SET revoked=?2 WHERE id=?1",
            params![id, revoked as i64],
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
    pub fn audit(&self, event: AuditEvent) -> Result<(), SessionError> {
        self.audit_event(AuditKind::Prompt, event)
    }

    /// Append a typed audit event. `kind` is what makes a refusal readable as
    /// a refusal in `arreo audit` rather than "a prompt that looks odd".
    pub fn audit_event(&self, kind: AuditKind, event: AuditEvent) -> Result<(), SessionError> {
        let (prompt, redacted) = redact(&event.prompt);
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO audit(ts_ms, device, agent, prompt, redacted, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                event.ts_ms as i64,
                event.device,
                event.agent,
                prompt,
                redacted as i64,
                kind.as_str(),
            ],
        )?;
        Ok(())
    }

    /// Newest audit rows first (operator reads the tail), limited.
    pub fn audit_recent(&self, limit: usize) -> Result<Vec<StoredAudit>, SessionError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT ts_ms, device, agent, prompt, redacted, kind FROM audit
             ORDER BY ts_ms DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(StoredAudit {
                ts_ms: row.get::<_, i64>(0)? as u64,
                device: row.get(1)?,
                agent: row.get(2)?,
                prompt: row.get(3)?,
                redacted: row.get::<_, i64>(4)? != 0,
                kind: AuditKind::parse(&row.get::<_, String>(5)?),
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(SessionError::Sqlite)
    }

    /// Export the audit log as JSON lines (for `arreo audit` / compliance).
    pub fn audit_export(&self) -> Result<String, SessionError> {
        let events = self.audit_recent(usize::MAX / 2)?;
        let mut out = String::new();
        for event in events.iter().rev() {
            out.push_str(
                &serde_json::to_string(&serde_json::json!({
                    "ts_ms": event.ts_ms,
                    "device": event.device,
                    "agent": event.agent,
                    "prompt": event.prompt,
                    "redacted": event.redacted,
                    "kind": event.kind.as_str(),
                }))
                .map_err(SessionError::Json)?,
            );
            out.push('\n');
        }
        Ok(out)
    }
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
            // sk-/AKIA/ghp_/gho_/xox tokens: mask long token-ish runs.
            for token in line
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
            {
                if (token.starts_with("sk-")
                    || token.starts_with("AKIA")
                    || token.starts_with("ghp_")
                    || token.starts_with("gho_")
                    || token.starts_with("xox"))
                    && token.len() > 8
                {
                    line = line.replace(&token, "[REDACTED:token]");
                    redacted_any = true;
                }
            }
            // KEY=VALUE assignments: mask values longer than 8 chars.
            for sep in ['=', ':'] {
                if let Some(pos) = line.find(sep) {
                    let (key, value) = line.split_at(pos + 1);
                    let value = value.trim();
                    if !value.is_empty() && value.len() >= 8 && key.to_lowercase().contains("key")
                        || key.to_lowercase().contains("secret")
                        || key.to_lowercase().contains("password")
                        || key.to_lowercase().contains("passwd")
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
    // If the scan flagged something but no rule masked it, be conservative:
    // flag redacted and mask the flagged lines' long runs.
    if !redacted_any {
        return (prompt.to_string(), true);
    }
    (out.join("\n"), true)
}
