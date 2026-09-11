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

use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// Current schema version. Bumped only alongside a migration below.
pub const SCHEMA_VERSION: u32 = 1;

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
pub struct RelayStore {
    conn: Mutex<Connection>,
    path: Option<PathBuf>,
}

impl RelayStore {
    /// Open (or create) the store at `path` and migrate it.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            path: Some(path.to_path_buf()),
        })
    }

    /// In-memory store, for tests and for a relay configured to forget.
    pub fn open_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
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
               UNIQUE(account_id, name_key));
             CREATE INDEX IF NOT EXISTS machine_account ON machine(account_id);
             CREATE INDEX IF NOT EXISTS machine_last_seen ON machine(last_seen_ms);",
        )?;
        conn.execute(
            "INSERT INTO meta(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [SCHEMA_VERSION.to_string()],
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
