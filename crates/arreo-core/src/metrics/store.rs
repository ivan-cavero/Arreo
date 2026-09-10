//! SQLite rollup store (T-0006 schema, versioned for T-0018 migrations).
//!
//! Schema v1:
//! ```sql
//! meta(key TEXT PRIMARY KEY, value TEXT)            -- schema_version=1
//! rollups(pane TEXT, ts_ms INTEGER, rss_bytes INTEGER, cpu REAL, pids INTEGER,
//!         PRIMARY KEY (pane, ts_ms))
//! ```
//! WAL mode for concurrent daemon readers. Retention pruning is explicit
//! (`prune_older_than`) — the daemon calls it on its rollup tick.

use rusqlite::{params, Connection};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("store sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct Rollup {
    pub pane: String,
    pub ts_ms: u64,
    pub rss_bytes: u64,
    pub cpu_percent: f64,
    pub pids: usize,
}

pub struct Store {
    conn: std::sync::Mutex<Connection>,
}

impl Store {
    fn init(conn: &Connection) -> Result<(), StoreError> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE IF NOT EXISTS rollups(
               pane TEXT NOT NULL, ts_ms INTEGER NOT NULL,
               rss_bytes INTEGER NOT NULL, cpu REAL NOT NULL, pids INTEGER NOT NULL,
               PRIMARY KEY (pane, ts_ms));",
        )?;
        conn.execute(
            "INSERT OR IGNORE INTO meta(key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// Open (or create) a file store.
    pub fn open(path: &std::path::Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::init(&conn)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    /// In-memory store (tests).
    pub fn open_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::init(&conn)?;
        Ok(Self {
            conn: std::sync::Mutex::new(conn),
        })
    }

    pub fn schema_version(&self) -> Result<u32, StoreError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| StoreError::Sqlite(rusqlite::Error::InvalidQuery))?;
        let value: String = conn.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        Ok(value.parse().unwrap_or(0))
    }

    pub fn insert_rollup(
        &self,
        pane: &str,
        ts_ms: u64,
        rss_bytes: u64,
        cpu_percent: f64,
        pids: usize,
    ) -> Result<(), StoreError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| StoreError::Sqlite(rusqlite::Error::InvalidQuery))?;
        conn.execute(
            "INSERT OR REPLACE INTO rollups(pane, ts_ms, rss_bytes, cpu, pids)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                pane,
                ts_ms as i64,
                rss_bytes as i64,
                cpu_percent,
                pids as i64
            ],
        )?;
        Ok(())
    }

    /// Newest-first? No — oldest-first (chart order), limited.
    pub fn recent(&self, pane: &str, limit: usize) -> Result<Vec<Rollup>, StoreError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| StoreError::Sqlite(rusqlite::Error::InvalidQuery))?;
        let mut stmt = conn.prepare(
            "SELECT pane, ts_ms, rss_bytes, cpu, pids FROM rollups
             WHERE pane = ?1 ORDER BY ts_ms ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pane, limit as i64], |row| {
            Ok(Rollup {
                pane: row.get(0)?,
                ts_ms: row.get::<_, i64>(1)? as u64,
                rss_bytes: row.get::<_, i64>(2)? as u64,
                cpu_percent: row.get(3)?,
                pids: row.get::<_, i64>(4)? as usize,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::Sqlite)
    }

    /// Delete rollups older than `cutoff_ms`. Returns rows removed.
    pub fn prune_older_than(&self, cutoff_ms: u64) -> Result<usize, StoreError> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| StoreError::Sqlite(rusqlite::Error::InvalidQuery))?;
        let removed = conn.execute(
            "DELETE FROM rollups WHERE ts_ms < ?1",
            params![cutoff_ms as i64],
        )?;
        Ok(removed)
    }
}
