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
pub const SCHEMA_VERSION: u32 = 5;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("relay store: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("relay store: {0}")]
    Json(#[from] serde_json::Error),
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
        // v5 (T-0053): the relay's own audit trail. Deliberately **not** a mirror
        // of the machine's table (`arreo_core::store`'s `audit`): the machine knows
        // which pane was touched and by which device, the relay knows only that
        // bytes moved between two ids — and keeping the two apart is what stops
        // this database from becoming somewhere content could accumulate.
        //
        // No declared primary key, so SQLite's implicit `rowid` is what breaks a
        // tie in the ordering: two rows in the same millisecond (a reconnect that
        // displaces a session writes both) still read back in the order they were
        // written, even if the clock steps backwards between them.
        if version < 5 {
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS relay_audit(
                   ts_ms INTEGER NOT NULL,
                   action TEXT NOT NULL,
                   outcome TEXT NOT NULL,
                   device_id TEXT,
                   account_id TEXT,
                   -- Truncated at write (IPv4 /24, IPv6 /48). The relay is the box
                   -- most likely to be someone else's, and a full-address trail
                   -- there is a location history of everyone who used it.
                   peer TEXT,
                   proto_version INTEGER,
                   -- A bounded, relay-authored reason. Never a payload: the writers
                   -- are format strings over identifiers, and `record` caps the
                   -- length so a pathological identifier cannot smuggle a blob.
                   detail TEXT);
                 CREATE INDEX IF NOT EXISTS relay_audit_time ON relay_audit(ts_ms);",
            )?;
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

    // ---- the audit trail (T-0053) ------------------------------------------

    /// Append one audit row. **The** writer: there is deliberately no update or
    /// delete API, so the log is append-only by construction rather than by
    /// convention, and the only removal is the explicit [`Self::audit_prune`].
    ///
    /// Two things happen here rather than at the call sites, because a rule that
    /// every writer must remember is a rule that will eventually be forgotten:
    ///
    /// - **the peer is truncated** (IPv4 /24, IPv6 /48) by
    ///   [`arreo_core::store::truncate_peer`] — the same function the machine's
    ///   trail uses, so the two logs redact identically;
    /// - **the detail is bounded** to [`crate::audit::DETAIL_MAX`]. A reason is a
    ///   sentence; a longer one is a bug, and capping it here means a pathological
    ///   identifier cannot become a place content accumulates.
    pub fn record(&self, event: &crate::audit::RelayAuditEvent) -> Result<(), StoreError> {
        let peer = event.peer.as_ref().map(arreo_core::store::truncate_peer);
        let detail = event.detail.as_ref().map(|text| {
            if text.chars().count() <= crate::audit::DETAIL_MAX {
                text.clone()
            } else {
                let head: String = text.chars().take(crate::audit::DETAIL_MAX).collect();
                format!("{head}…")
            }
        });
        let conn = self.lock()?;
        conn.execute(
            "INSERT INTO relay_audit
               (ts_ms, action, outcome, device_id, account_id, peer, proto_version, detail)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                crate::directory::now_ms(),
                event.action,
                event.outcome.as_str(),
                event.device_id,
                event.account_id,
                peer,
                event.proto_version.map(i64::from),
                detail,
            ],
        )?;
        Ok(())
    }

    /// Read the trail, oldest first.
    ///
    /// Ordered by `(ts_ms, rowid)`: `ts_ms` is the clock, `rowid` is the order the
    /// rows were actually written, so a clock that steps backwards between two
    /// writes does not reorder history. The filter type is the machine's own
    /// [`AuditQuery`], so "the same filter semantics" is a shared type rather than
    /// two parsers that agree today.
    pub fn audit_query(
        &self,
        query: &arreo_core::store::AuditQuery,
    ) -> Result<Vec<crate::audit::StoredRelayAudit>, StoreError> {
        // **The filter is `u64`; a stored timestamp is `i64`.** A cutoff past
        // `i64::MAX` (which is what a caller passes when it means "no bound at
        // all" and reaches for `u64::MAX`) would wrap to a negative number in a
        // cast — and a negative `since` matches *every* row while a negative
        // `until` matches none. Both are the opposite of what was asked for, so
        // the impossible window is answered here rather than by a cast.
        if query.since_ms.is_some_and(|ms| ms > i64::MAX as u64) {
            return Ok(Vec::new());
        }
        let since = query.since_ms.map(|ms| ms.min(i64::MAX as u64) as i64);
        // `until` past the i64 range means every row is before it, so the clamp is
        // exactly equivalent rather than merely close.
        let until = query.until_ms.map(|ms| ms.min(i64::MAX as u64) as i64);
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT ts_ms, action, outcome, device_id, account_id, peer, proto_version, detail
               FROM relay_audit
              WHERE (?1 IS NULL OR ts_ms >= ?1)
                AND (?2 IS NULL OR ts_ms <= ?2)
                AND (?3 IS NULL OR action = ?3)
              ORDER BY ts_ms ASC, rowid ASC
              LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![since, until, query.action.as_deref(), query.limit as i64],
            |row| {
                Ok(crate::audit::StoredRelayAudit {
                    ts_ms: row.get::<_, i64>(0)? as u64,
                    action: row.get(1)?,
                    outcome: arreo_core::store::AuditOutcome::parse(&row.get::<_, String>(2)?),
                    device_id: row.get(3)?,
                    account_id: row.get(4)?,
                    peer: row.get(5)?,
                    proto_version: row.get::<_, Option<i64>>(6)?.map(|v| v as u32),
                    detail: row.get(7)?,
                })
            },
        )?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    /// Export the trail, byte-identically to the machine's own export for the
    /// same rows and filters — one renderer, in `arreo_core`.
    pub fn audit_export(
        &self,
        query: &arreo_core::store::AuditQuery,
        format: arreo_core::store::ExportFormat,
    ) -> Result<String, StoreError> {
        let rows = self.audit_query(query)?;
        crate::audit::render(&rows, format).map_err(StoreError::from)
    }

    /// How many rows the trail holds, and roughly how many bytes of text they
    /// carry. The numbers the size guard reports, and what a prune prints.
    pub fn audit_size(&self) -> Result<(u64, u64), StoreError> {
        let conn = self.lock()?;
        let (rows, bytes) = conn.query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(LENGTH(action) + LENGTH(COALESCE(device_id, ''))
                               + LENGTH(COALESCE(account_id, '')) + LENGTH(COALESCE(peer, ''))
                               + LENGTH(COALESCE(detail, ''))), 0)
               FROM relay_audit",
            [],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, i64>(1)? as u64)),
        )?;
        Ok((rows, bytes))
    }

    /// The size guard: a warning when the trail has grown past `limit_bytes`, or
    /// `None` while it is small.
    ///
    /// **Measured, not estimated from a row count.** A row's size depends on how
    /// long the identifiers are, so "a million rows" says nothing about disk. The
    /// text a row carries is the part that grows without bound, so that is what is
    /// summed; the report says it is text rather than claiming to be the file size.
    ///
    /// Nothing prunes automatically — a relay that silently deleted its own trail
    /// would defeat the point of having one. This only says when to run the
    /// documented offline prune.
    pub fn audit_size_warning(&self, limit_bytes: u64) -> Result<Option<String>, StoreError> {
        let (rows, bytes) = self.audit_size()?;
        if bytes <= limit_bytes {
            return Ok(None);
        }
        Ok(Some(format!(
            "the relay's audit trail holds {rows} row(s), about {} MiB of text (limit {} MiB): \
             prune it offline with `arreo-relay audit prune --before <ms>`",
            bytes / (1024 * 1024),
            limit_bytes / (1024 * 1024)
        )))
    }

    /// Delete trail rows older than `before_ms`, returning how many went.
    ///
    /// The prune records *itself* (an `audit.prune` row with the count) after the
    /// delete, so the trail always says that a prune happened and how much it took
    /// — a gap in a log with no explanation is worse than the log being long.
    pub fn audit_prune(&self, before_ms: u64) -> Result<u64, StoreError> {
        let removed = {
            let conn = self.lock()?;
            // The same `u64`-cutoff-versus-`i64`-column trap the query has: a
            // cutoff above the i64 range means "everything", and casting it would
            // wrap to a negative bound that deletes nothing.
            let deleted = if before_ms > i64::MAX as u64 {
                conn.execute("DELETE FROM relay_audit", [])?
            } else {
                conn.execute(
                    "DELETE FROM relay_audit WHERE ts_ms < ?1",
                    rusqlite::params![before_ms as i64],
                )?
            };
            deleted as u64
        };
        self.record(
            &crate::audit::RelayAuditEvent::new(
                crate::audit::actions::PRUNE,
                arreo_core::store::AuditOutcome::Ok,
            )
            .detail(format!(
                "removed {removed} row(s) before {}",
                arreo_core::store::rfc3339_ms(before_ms as i64)
            )),
        )?;
        Ok(removed)
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
