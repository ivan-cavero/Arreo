//! The relay's durable store: one SQLite connection, one migration owner
//! (T-0043).
//!
//! **Why this module exists as a named boundary.** The relay is the only place
//! in the product that keeps durable state on behalf of an *account* rather
//! than a machine: the machine directory today, the durable inbox (T-0030) and
//! presence (T-0031) next. Two writers each inventing their own connection,
//! PRAGMAs and schema-version bookkeeping is how a database grows two
//! incompatible histories, so every relay table is created here, by one
//! migration function, in one place — the same shape `arreo-core`'s session
//! store already uses (T-0018), deliberately reused rather than re-invented.
//!
//! The relay's data is **metadata only**: names, presence, identities, and
//! opaque envelopes. Nothing here can read a user's traffic, and a schema test
//! (`tests/directory.rs`) fails if a column ever suggests otherwise.

use rusqlite::{Connection, OptionalExtension};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Current schema version. Bumped only alongside a migration below.
pub const SCHEMA_VERSION: u32 = 4;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("relay store: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("relay store is poisoned")]
    Poisoned,
}

/// The relay's database handle.
///
/// One connection behind a mutex, like the session store: the relay's writes are
/// small and serialized anyway (a claim has to be serialized to be correct), and
/// WAL lets readers proceed while a writer holds the write lock.
#[derive(Clone)]
pub struct RelayStore {
    /// Shared, not copied: the relay has exactly one connection (the migration
    /// owner), and the inbox and the router both hold a handle to it rather than
    /// opening a second one — two connections would mean two WAL writers and two
    /// ideas about locking.
    conn: std::sync::Arc<Mutex<Connection>>,
    path: Option<PathBuf>,
}

impl RelayStore {
    /// Open (or create) the store at `path` and migrate it.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: std::sync::Arc::new(Mutex::new(conn)),
            path: Some(path.to_path_buf()),
        })
    }

    /// In-memory store, for tests and for a relay configured to forget.
    pub fn open_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: std::sync::Arc::new(Mutex::new(conn)),
            path: None,
        })
    }

    /// Where this store lives, if it is on disk.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The schema every relay table is created by. Idempotent, so a restart
    /// migrates rather than recreates.
    fn migrate(conn: &Connection) -> Result<(), StoreError> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
             CREATE TABLE IF NOT EXISTS account(
               account_id TEXT PRIMARY KEY,
               created_at_ms INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS machine(
               machine_id TEXT PRIMARY KEY,
               account_id TEXT NOT NULL REFERENCES account(account_id),
               name TEXT NOT NULL,
               -- The live claim on a name. Three states, and the middle one is
               -- what makes tombstones work with UNIQUE(account_id, name_key):
               --   a live row            -> casefold(name)
               --   an unexpired tombstone -> casefold(name)  (still holds it)
               --   an expired tombstone   -> NULL            (released; SQLite
               --                            treats NULLs as distinct, so many
               --                            released rows coexist)
               name_key TEXT,
               presence TEXT NOT NULL,
               last_seen_ms INTEGER NOT NULL,
               proto_version INTEGER NOT NULL,
               tombstone_until_ms INTEGER,
               name_conflict INTEGER NOT NULL DEFAULT 0,
               -- The public key a client dials to reach this machine's daemon
               -- (T-0045). Written from the certificate that authenticated the
               -- session asserting the row, so it is verified rather than
               -- self-reported; NULL for a row written before this column.
               daemon_key TEXT,
               UNIQUE(account_id, name_key));
             CREATE INDEX IF NOT EXISTS machine_account ON machine(account_id);
             CREATE INDEX IF NOT EXISTS machine_last_seen ON machine(last_seen_ms);",
        )?;

        // v2 (T-0029): the account's root public key — the anchor the relay
        // verifies a device certificate against — and the device registry, which
        // is how a routed envelope's destination can be "known" (so an unknown
        // one is a typed refusal rather than a guess). Both are metadata: a
        // public key and a fingerprint, never a secret.
        let version: u32 = conn
            .query_row(
                "SELECT value FROM meta WHERE key='schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map(|v| v.parse().unwrap_or(0))
            .unwrap_or(0);
        if version < 2 {
            if !has_column(conn, "account", "root_key")? {
                conn.execute_batch("ALTER TABLE account ADD COLUMN root_key TEXT;")?;
            }
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS relay_device(
                   account_id TEXT NOT NULL,
                   device_id TEXT NOT NULL,
                   first_seen_ms INTEGER NOT NULL,
                   last_seen_ms INTEGER NOT NULL,
                   PRIMARY KEY (account_id, device_id));
                 CREATE INDEX IF NOT EXISTS relay_device_seen ON relay_device(last_seen_ms);",
            )?;
        }
        // v3 (T-0030): the durable per-device inbox. The row is deliberately
        // four columns of bookkeeping plus one opaque blob — there is nowhere to
        // put a key, a pairing code or agent state, and a test asserts the
        // column set for that reason.
        if version < 3 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS inbox(
                   device_id TEXT NOT NULL,
                   seq INTEGER NOT NULL,
                   received_at_ms INTEGER NOT NULL,
                   expires_at_ms INTEGER NOT NULL,
                   bytes BLOB NOT NULL,
                   PRIMARY KEY (device_id, seq));
                 CREATE INDEX IF NOT EXISTS inbox_expiry ON inbox(expires_at_ms);
                 -- Per-device counters that must survive a restart, because a
                 -- drop the operator cannot count is a drop that never happened.
                 CREATE TABLE IF NOT EXISTS inbox_stats(
                   device_id TEXT PRIMARY KEY,
                   dropped_total INTEGER NOT NULL DEFAULT 0,
                   expired_total INTEGER NOT NULL DEFAULT 0,
                   bytes INTEGER NOT NULL DEFAULT 0,
                   queued INTEGER NOT NULL DEFAULT 0,
                   -- The dropped_total watermark the last drain reported: the
                   -- per-drain drop count is durable rather than inferred, and a
                   -- drain cannot report the same drop twice.
                   dropped_reported INTEGER NOT NULL DEFAULT 0);",
            )?;
        }
        // v4: the dial key on a machine's row (T-0045). An `ALTER` rather than a
        // rebuilt table — the row is live data an account depends on, and SQLite
        // adds a nullable column in place. Existing rows get NULL, which the
        // reader treats as "not routable": honest, because nothing has asserted a
        // dial key for them yet, and the next join fills it in.
        if version < 4 && !has_column(conn, "machine", "daemon_key")? {
            conn.execute_batch("ALTER TABLE machine ADD COLUMN daemon_key TEXT;")?;
        }
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [SCHEMA_VERSION.to_string()],
        )?;
        Ok(())
    }

    /// Record that a device authenticated, or refresh when it last did.
    ///
    /// `first_seen_ms` is written once: a device's first contact is the fact a
    /// later task (presence, T-0031) wants, and an upsert that overwrote it
    /// would lose it.
    pub fn touch_device(
        &self,
        account_id: &str,
        device_id: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO relay_device(account_id, device_id, first_seen_ms, last_seen_ms)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(account_id, device_id) DO UPDATE SET last_seen_ms = excluded.last_seen_ms",
            rusqlite::params![account_id, device_id, now_ms],
        )?;
        Ok(())
    }

    /// Has this device ever authenticated into this account?
    pub fn device_known(&self, account_id: &str, device_id: &str) -> Result<bool, StoreError> {
        let conn = self.lock()?;
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM relay_device WHERE account_id = ?1 AND device_id = ?2",
                rusqlite::params![account_id, device_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }

    /// Every device the relay has ever seen in an account, with its last-seen
    /// timestamp: the read path presence reports from (T-0031).
    ///
    /// One query over the `relay_device_seen` index — no table scan, no join —
    /// because this is the query a 10,000-device listing runs. The caller
    /// derives the variant with the one rule (`presence_at`), so the window is
    /// never re-derived here.
    pub fn device_presence(&self, account_id: &str) -> Result<Vec<(String, i64)>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT device_id, last_seen_ms FROM relay_device
             WHERE account_id = ?1 ORDER BY device_id",
        )?;
        let rows = stmt.query_map(rusqlite::params![account_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// The account's root public key, if the account is registered.
    pub fn account_root(&self, account_id: &str) -> Result<Option<[u8; 32]>, StoreError> {
        let conn = self.lock()?;
        let key: Option<Option<String>> = conn
            .query_row(
                "SELECT root_key FROM account WHERE account_id = ?1",
                rusqlite::params![account_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(key.flatten().and_then(|hex| parse_hex_32(&hex)))
    }

    /// Register (or re-key) an account. Called through [`crate::Directory`], so
    /// there is one way to create an account rather than two that can drift.
    pub(crate) fn set_account_root(
        &self,
        account_id: &str,
        root_key_hex: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO account(account_id, created_at_ms, root_key) VALUES (?1, ?2, ?3)
             ON CONFLICT(account_id) DO UPDATE SET root_key = excluded.root_key",
            rusqlite::params![account_id, now_ms, root_key_hex],
        )?;
        Ok(())
    }

    /// The schema version on disk.
    pub fn schema_version(&self) -> Result<u32, StoreError> {
        let conn = self.lock()?;
        let value: String = conn.query_row(
            "SELECT value FROM meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )?;
        Ok(value.parse().unwrap_or(0))
    }

    /// The query plan for the presence listing (T-0031): the test asserts the
    /// index serves it, because "fast on this box today" is not a guarantee and
    /// a plan is.
    pub fn explain_presence_query(&self) -> Result<String, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "EXPLAIN QUERY PLAN SELECT device_id, last_seen_ms FROM relay_device
             WHERE account_id = ?1 ORDER BY device_id",
        )?;
        let parts = stmt.query_map(["acct-1"], |row| {
            Ok(format!(
                "{}|{}|{}|{}",
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?
            ))
        })?;
        Ok(parts.collect::<Result<Vec<_>, _>>()?.join("\n"))
    }

    /// The column names of `table`, in definition order.
    pub fn columns(&self, table: &str) -> Result<Vec<String>, StoreError> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let mut rows = stmt.query([])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(row.get::<_, String>(1)?);
        }
        Ok(out)
    }

    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, Connection>, StoreError> {
        self.conn.lock().map_err(|_| StoreError::Poisoned)
    }
}

/// True when `table` has `column` (the v1→v2 migration adds one in place).
fn has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, StoreError> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Parse a 64-character hex string into 32 bytes.
fn parse_hex_32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, chunk) in text.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        out[index] = (hi * 16 + lo) as u8;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrating_twice_is_a_no_op_and_reports_one_version() {
        let dir = std::env::temp_dir().join(format!("arreo-relay-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let path = dir.join("relay.db");

        let store = RelayStore::open(&path).expect("open");
        assert_eq!(store.schema_version().expect("version"), SCHEMA_VERSION);
        drop(store);
        // A restart migrates, never recreates: the tables survive.
        let store = RelayStore::open(&path).expect("reopen");
        assert_eq!(store.schema_version().expect("version"), SCHEMA_VERSION);
        assert!(store
            .columns("machine")
            .expect("columns")
            .contains(&"machine_id".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
