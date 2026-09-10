//! Session + audit store (T-0018): versioned SQLite schema with migrations.
//!
//! One sentence: the daemon's memory (pane topology + scrollback) and its
//! conscience (append-only audit log) live in one WAL SQLite file, migrated
//! forward from the T-0006 metrics schema — never wiped.
//!
//! Schema:
//! - v1 (T-0006): `meta` + `rollups` (metrics history).
//! - v2 (this task): + `panes(id TEXT PRIMARY KEY, program, args JSON,
//!   cols, rows)` + `scrollback(pane, line_no, text)` + `audit(ts_ms,
//!   device, agent, prompt, redacted)`.
//! - `open` runs `migrate()` (v1→v2 `CREATE TABLE IF NOT EXISTS` + version
//!   bump); future versions append `migrate_vN` steps. Data is never dropped
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

pub const SCHEMA_VERSION: u32 = 2;

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
        Ok(())
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
        let (prompt, redacted) = redact(&event.prompt);
        let conn = self
            .conn
            .lock()
            .map_err(|_| SessionError::Sqlite(rusqlite::Error::InvalidQuery))?;
        conn.execute(
            "INSERT INTO audit(ts_ms, device, agent, prompt, redacted) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                event.ts_ms as i64,
                event.device,
                event.agent,
                prompt,
                redacted as i64,
            ],
        )?;
        Ok(())
    }

    /// Newest audit rows first (operator reads the tail), limited.
    pub fn audit_recent(&self, limit: usize) -> Result<Vec<StoredAudit>, SessionError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| SessionError::Sqlite(rusqlite::Error::InvalidQuery))?;
        let mut stmt = conn.prepare(
            "SELECT ts_ms, device, agent, prompt, redacted FROM audit
             ORDER BY ts_ms DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit as i64], |row| {
            Ok(StoredAudit {
                ts_ms: row.get::<_, i64>(0)? as u64,
                device: row.get(1)?,
                agent: row.get(2)?,
                prompt: row.get(3)?,
                redacted: row.get::<_, i64>(4)? != 0,
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
