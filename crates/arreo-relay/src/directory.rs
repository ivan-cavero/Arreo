//! The durable machine directory (T-0043, ROADMAP §3.7).
//!
//! One sentence: an account owns machines; each has a name that resolves
//! everywhere, a presence the relay observes, and a tombstone that keeps a
//! removed name for the machine that had it.
//!
//! Three rules this module exists to make true, because each is easy to get
//! subtly wrong:
//!
//! 1. **Joining is explicit.** A machine enters the directory only by
//!    completing an account join: a single-use ticket with a 5-minute life. LAN
//!    discovery may tell us *where* a known machine is; it can never tell us
//!    that one exists, is renamed, or is gone. That is why the only insert path
//!    is [`Directory::join`] with a consumed [`JoinTicket`], and why
//!    [`Directory::resolve_known`] takes a `machine_id` rather than a name.
//! 2. **A claim is serialized.** Two machines claiming one name at the same
//!    instant must not both get it, and the loser must not be rejected — it gets
//!    a deterministic suffix and a flag saying so. The claim runs inside one
//!    `BEGIN IMMEDIATE` transaction, so the check and the insert cannot
//!    interleave.
//! 3. **Nothing is silently renamed.** A conflict produces `workbox-2` *and*
//!    `name_conflict = 1`; a rename onto a taken name is refused outright and
//!    the old name is left exactly as it was.

use crate::store::{RelayStore, StoreError};
use arreo_core::mesh::directory::{
    export_rows, presence_of, DirectoryError, MachineId, MachineRow, Name, Presence,
    JOIN_TICKET_SECS, STALE_AFTER_SECS, TOMBSTONE_SECS,
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};

/// Milliseconds since the Unix epoch — the clock every row is stamped with.
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug, thiserror::Error)]
pub enum DirectoryFailure {
    #[error("directory store: {0}")]
    Store(#[from] StoreError),
    /// Raw `rusqlite` errors from statements run directly against the locked
    /// connection (the directory's own SQL, not the store's).
    #[error("directory sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Rule(#[from] DirectoryError),
    #[error("no account {0:?}")]
    NoSuchAccount(String),
}

/// A one-shot permission to join an account.
///
/// The ticket is the *only* way into the directory, which is what makes
/// "explicit join, never ambient discovery" a property of the code rather than
/// a promise in a document. It is single-use ([`JoinTicket::consume`] takes
/// `self` by value, so a second join needs a second ticket) and expires.
#[derive(Debug)]
pub struct JoinTicket {
    account_id: String,
    expires_at_ms: i64,
    used: bool,
}

impl JoinTicket {
    /// Mint a ticket for `account_id`, valid for [`JOIN_TICKET_SECS`].
    #[must_use]
    pub fn issue(account_id: &str, now_ms: i64) -> Self {
        Self {
            account_id: account_id.to_string(),
            expires_at_ms: now_ms + (JOIN_TICKET_SECS as i64) * 1000,
            used: false,
        }
    }

    #[must_use]
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    #[must_use]
    pub fn expires_at_ms(&self) -> i64 {
        self.expires_at_ms
    }

    /// Spend the ticket. Taking `self` is the single-use guarantee: the caller
    /// cannot spend it twice because it no longer has it.
    fn consume(mut self, now_ms: i64) -> Result<String, DirectoryError> {
        if self.used {
            return Err(DirectoryError::Ticket {
                state: "already used",
            });
        }
        if now_ms >= self.expires_at_ms {
            return Err(DirectoryError::Ticket { state: "expired" });
        }
        self.used = true;
        Ok(self.account_id)
    }
}

/// The machine directory, backed by the relay's store.
pub struct Directory {
    store: RelayStore,
}

impl Directory {
    #[must_use]
    pub fn new(store: RelayStore) -> Self {
        Self { store }
    }

    #[must_use]
    pub fn store(&self) -> &RelayStore {
        &self.store
    }

    /// Create (or re-key) an account, registering the root key its devices'
    /// certificates chain to.
    ///
    /// The root key is not optional: the relay authenticates a device by
    /// verifying its certificate against this key, so an account without one
    /// could never admit anybody. Making that a parameter is what stops an
    /// account from existing in a state where it is registered but unusable.
    pub fn create_account(
        &self,
        account_id: &str,
        root_key: &arreo_core::identity::VerifyingKey,
        now_ms: i64,
    ) -> Result<(), DirectoryFailure> {
        self.store
            .set_account_root(account_id, &hex32(&root_key.to_bytes()), now_ms)?;
        Ok(())
    }

    /// The account's root public key, if the account is registered.
    pub fn account_root(
        &self,
        account_id: &str,
    ) -> Result<Option<arreo_core::identity::VerifyingKey>, DirectoryFailure> {
        let Some(bytes) = self.store.account_root(account_id)? else {
            return Ok(None);
        };
        Ok(arreo_core::identity::VerifyingKey::from_bytes(&bytes).ok())
    }

    /// Register a machine against an account, spending a join ticket.
    ///
    /// The name is the machine's request, not its guarantee: if it is live for
    /// another machine, this one gets the deterministic suffix and the row says
    /// so. See [`Directory::claim_name`] for the rule.
    pub fn join(
        &self,
        ticket: JoinTicket,
        machine_id: &MachineId,
        requested: &Name,
        proto_version: u32,
        now_ms: i64,
    ) -> Result<MachineRow, DirectoryFailure> {
        let account_id = ticket.consume(now_ms)?;
        let mut conn = self.store.lock()?;
        let exists: Option<String> = conn
            .query_row(
                "SELECT account_id FROM account WHERE account_id = ?1",
                params![account_id],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_none() {
            return Err(DirectoryFailure::NoSuchAccount(account_id));
        }

        // One transaction, and an *immediate* one: the suffix scan must see a
        // consistent set of live names, and two joins racing for one name must
        // serialize rather than both reading "free". `IMMEDIATE` takes the write
        // lock up front, which is what makes the second claimer wait instead of
        // failing on commit.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let row = Self::claim_locked(
            &tx,
            &account_id,
            machine_id,
            requested,
            proto_version,
            now_ms,
        )?;
        tx.commit()?;
        Ok(row)
    }

    /// The claim itself, already inside a write transaction.
    fn claim_locked(
        conn: &rusqlite::Connection,
        account_id: &str,
        machine_id: &MachineId,
        requested: &Name,
        proto_version: u32,
        now_ms: i64,
    ) -> Result<MachineRow, DirectoryFailure> {
        // A tombstone that has run out stops holding its name. Doing this here —
        // inside the write transaction, before the suffix scan — is what lets
        // the name be reclaimed at all: the row stays as history, and its
        // `name_key` goes back to NULL so the unique index no longer counts it.
        Self::release_expired_tombstones(conn, account_id, now_ms)?;

        // A machine that already has a row keeps its name: rejoining is not a
        // rename, and it is how a returning machine reclaims its tombstoned name.
        if let Some(existing) = Self::row_for(conn, machine_id)? {
            if existing.name.as_str() != requested.as_str() {
                return Err(DirectoryError::NameTaken(existing.name.to_string()).into());
            }
            conn.execute(
                "UPDATE machine SET last_seen_ms = ?2, proto_version = ?3,
                                    tombstone_until_ms = NULL, name_key = ?4
                 WHERE machine_id = ?1",
                params![
                    machine_id.as_str(),
                    now_ms,
                    proto_version,
                    requested.as_str()
                ],
            )?;
            return Self::row_for(conn, machine_id)?
                .ok_or_else(|| DirectoryError::NoSuchMachine(machine_id.to_string()).into());
        }

        let (name, conflict) = Self::pick_name(conn, account_id, requested, machine_id, now_ms)?;
        conn.execute(
            "INSERT INTO machine(machine_id, account_id, name, name_key, presence,
                                 last_seen_ms, proto_version, tombstone_until_ms, name_conflict)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8)",
            params![
                machine_id.as_str(),
                account_id,
                name.as_str(),
                name.as_str(),
                Presence::Online.as_str(),
                now_ms,
                proto_version,
                i64::from(conflict)
            ],
        )?;
        Self::row_for(conn, machine_id)?
            .ok_or_else(|| DirectoryError::NoSuchMachine(machine_id.to_string()).into())
    }

    /// Stop expired tombstones from holding their names.
    ///
    /// A tombstone is a row that keeps `name_key` so nobody else can take the
    /// name; once it expires the name must be claimable, and the only thing
    /// standing in the way is that same unique index. So expiry releases the key
    /// (sets it to NULL) and leaves the row as history.
    fn release_expired_tombstones(
        conn: &rusqlite::Connection,
        account_id: &str,
        now_ms: i64,
    ) -> Result<usize, DirectoryFailure> {
        let released = conn.execute(
            "UPDATE machine SET name_key = NULL
             WHERE account_id = ?1 AND name_key IS NOT NULL
               AND tombstone_until_ms IS NOT NULL AND tombstone_until_ms <= ?2",
            params![account_id, now_ms],
        )?;
        Ok(released)
    }

    /// The first free name: the request, then `-2`, `-3`, … in order.
    ///
    /// Deterministic by construction — the scan is ordered and stops at the
    /// first gap, so the same directory plus the same requests always yields the
    /// same assignment. A tombstone held by *another* machine blocks the name
    /// until it expires; a tombstone held by *this* machine was already handled
    /// above (it reclaimed the name).
    fn pick_name(
        conn: &rusqlite::Connection,
        account_id: &str,
        requested: &Name,
        machine_id: &MachineId,
        now_ms: i64,
    ) -> Result<(Name, bool), DirectoryFailure> {
        let mut candidate = requested.clone();
        let mut ordinal = 1usize;
        loop {
            if Self::name_is_free(conn, account_id, &candidate, machine_id, now_ms)? {
                return Ok((candidate, ordinal > 1));
            }
            ordinal += 1;
            candidate = requested.with_suffix(ordinal);
            if ordinal > 10_000 {
                // A directory with ten thousand machines called `workbox` is not
                // a directory; refusing beats spinning.
                return Err(DirectoryError::NameTaken(requested.to_string()).into());
            }
        }
    }

    /// A name is free when no live row holds it and no *unexpired* tombstone
    /// held by another machine holds it.
    fn name_is_free(
        conn: &rusqlite::Connection,
        account_id: &str,
        name: &Name,
        machine_id: &MachineId,
        now_ms: i64,
    ) -> Result<bool, DirectoryFailure> {
        let holder: Option<(String, Option<i64>)> = conn
            .query_row(
                "SELECT machine_id, tombstone_until_ms FROM machine
                 WHERE account_id = ?1 AND name_key = ?2",
                params![account_id, name.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(match holder {
            None => true,
            Some((holder_id, tombstone)) => {
                // Our own row (or our own tombstone) is not a conflict.
                if holder_id == machine_id.as_str() {
                    return Ok(true);
                }
                match tombstone {
                    // A tombstone that has expired releases the name.
                    Some(until) => until <= now_ms,
                    None => false,
                }
            }
        })
    }

    /// Rename a machine: atomic, identity-preserving, and refused on collision.
    ///
    /// A refusal leaves the old name exactly as it was — no partial state, no
    /// silent suffix (a *claim* may suffix because the machine has no name yet;
    /// a rename onto a taken name is a mistake the caller should see).
    pub fn rename(
        &self,
        machine_id: &MachineId,
        new_name: &Name,
        now_ms: i64,
    ) -> Result<MachineRow, DirectoryFailure> {
        let mut conn = self.store.lock()?;
        // The account is read from the row itself, so a future multi-account
        // store cannot rename across accounts by passing the wrong id.
        let account_id: Option<String> = conn
            .query_row(
                "SELECT account_id FROM machine WHERE machine_id = ?1",
                params![machine_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(account_id) = account_id else {
            return Err(DirectoryError::NoSuchMachine(machine_id.to_string()).into());
        };
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Same reason as a claim: an expired tombstone must not block a rename.
        Self::release_expired_tombstones(&tx, &account_id, now_ms)?;
        if !Self::name_is_free(&tx, &account_id, new_name, machine_id, now_ms)? {
            return Err(DirectoryError::NameTaken(new_name.to_string()).into());
        }
        tx.execute(
            "UPDATE machine SET name = ?2, name_key = ?2, name_conflict = 0
             WHERE machine_id = ?1",
            params![machine_id.as_str(), new_name.as_str()],
        )?;
        tx.commit()?;
        drop(conn);
        let conn = self.store.lock()?;
        Self::row_for(&conn, machine_id)?
            .ok_or_else(|| DirectoryError::NoSuchMachine(machine_id.to_string()).into())
    }

    /// Remove a machine, holding its name for it for [`TOMBSTONE_SECS`].
    ///
    /// The row stays (a tombstone is state, not an absence): the name is not
    /// free for another machine until it expires, and the machine that had it
    /// can rejoin and take it back.
    pub fn remove(
        &self,
        machine_id: &MachineId,
        now_ms: i64,
    ) -> Result<MachineRow, DirectoryFailure> {
        let conn = self.store.lock()?;
        if Self::row_for(&conn, machine_id)?.is_none() {
            return Err(DirectoryError::NoSuchMachine(machine_id.to_string()).into());
        }
        let until = now_ms + (TOMBSTONE_SECS as i64) * 1000;
        conn.execute(
            "UPDATE machine SET tombstone_until_ms = ?2 WHERE machine_id = ?1",
            params![machine_id.as_str(), until],
        )?;
        Self::row_for(&conn, machine_id)?
            .ok_or_else(|| DirectoryError::NoSuchMachine(machine_id.to_string()).into())
    }

    /// The rows of an account, presence computed from the stated thresholds.
    pub fn list(&self, account_id: &str, now_ms: i64) -> Result<Vec<MachineRow>, DirectoryFailure> {
        let conn = self.store.lock()?;
        let mut stmt = conn.prepare(
            "SELECT machine_id, name, name_conflict, last_seen_ms, proto_version, tombstone_until_ms
             FROM machine WHERE account_id = ?1 ORDER BY name_key",
        )?;
        let mut rows = stmt.query(params![account_id])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            out.push(Self::row_from(row, now_ms)?);
        }
        Ok(out)
    }

    /// The canonical, sorted export of an account's directory.
    pub fn export(&self, account_id: &str, now_ms: i64) -> Result<String, DirectoryFailure> {
        Ok(export_rows(&self.list(account_id, now_ms)?))
    }

    /// Record a heartbeat: the presence rule reads `last_seen_ms`, so this is
    /// all "I am alive" has to write.
    pub fn heartbeat(&self, machine_id: &MachineId, now_ms: i64) -> Result<(), DirectoryFailure> {
        let conn = self.store.lock()?;
        let changed = conn.execute(
            "UPDATE machine SET last_seen_ms = ?2 WHERE machine_id = ?1",
            params![machine_id.as_str(), now_ms],
        )?;
        if changed == 0 {
            return Err(DirectoryError::NoSuchMachine(machine_id.to_string()).into());
        }
        Ok(())
    }

    /// Resolve a name to a machine id. This is the *only* lookup that answers a
    /// name, and it never creates anything.
    pub fn resolve(
        &self,
        account_id: &str,
        name: &Name,
    ) -> Result<Option<MachineId>, DirectoryFailure> {
        let conn = self.store.lock()?;
        let id: Option<String> = conn
            .query_row(
                "SELECT machine_id FROM machine WHERE account_id = ?1 AND name_key = ?2",
                params![account_id, name.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        match id {
            Some(id) => Ok(Some(MachineId::parse(&id)?)),
            None => Ok(None),
        }
    }

    /// Where a *known* machine is, without letting the answer become a
    /// directory entry.
    ///
    /// This is the LAN/mDNS door (ROADMAP §3.4): a peer may report an address
    /// for a `machine_id` we already have, and the call returns `None` for
    /// anything else. There is no name argument, so discovery cannot create,
    /// rename or resurrect a machine even by accident.
    pub fn resolve_known(
        &self,
        account_id: &str,
        machine_id: &MachineId,
    ) -> Result<Option<MachineId>, DirectoryFailure> {
        let conn = self.store.lock()?;
        let found: Option<String> = conn
            .query_row(
                "SELECT machine_id FROM machine WHERE account_id = ?1 AND machine_id = ?2",
                params![account_id, machine_id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.map(|_| machine_id.clone()))
    }

    /// Remove exactly the stale machines' *names*, idempotently.
    ///
    /// "Prunes exactly the stale set" means the predicate is the same presence
    /// rule every reader sees, not a second threshold that could disagree with
    /// it — and a second run removes nothing because the first already did.
    pub fn remove_stale(
        &self,
        account_id: &str,
        now_ms: i64,
    ) -> Result<Vec<MachineId>, DirectoryFailure> {
        let stale: Vec<MachineId> = self
            .list(account_id, now_ms)?
            .into_iter()
            .filter(|row| row.presence == Presence::Stale && !row.tombstone_active(now_ms))
            .map(|row| row.machine_id)
            .collect();
        for machine in &stale {
            self.remove(machine, now_ms)?;
        }
        Ok(stale)
    }

    fn row_for(
        conn: &rusqlite::Connection,
        machine_id: &MachineId,
    ) -> Result<Option<MachineRow>, DirectoryFailure> {
        let mut stmt = conn.prepare(
            "SELECT machine_id, name, name_conflict, last_seen_ms, proto_version, tombstone_until_ms
             FROM machine WHERE machine_id = ?1",
        )?;
        let mut rows = stmt.query(params![machine_id.as_str()])?;
        match rows.next()? {
            Some(row) => Ok(Some(Self::row_from(row, 0)?)),
            None => Ok(None),
        }
    }

    fn row_from(row: &rusqlite::Row<'_>, now_ms: i64) -> Result<MachineRow, DirectoryFailure> {
        let machine_id = MachineId::parse(&row.get::<_, String>(0)?)?;
        let name = Name::parse(&row.get::<_, String>(1)?)?;
        let conflict = row.get::<_, i64>(2)? != 0;
        let last_seen_ms = row.get::<_, i64>(3)?;
        let proto_version = row.get::<_, i64>(4)? as u32;
        let tombstone_until_ms = row.get::<_, Option<i64>>(5)?;
        Ok(MachineRow {
            machine_id,
            name,
            name_conflict: conflict,
            presence: presence_of(last_seen_ms, now_ms),
            last_seen_ms,
            proto_version,
            tombstone_until_ms,
        })
    }
}

/// Lowercase hex, the spelling the store keeps.
fn hex32(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// The stale threshold in milliseconds, for callers that report ages rather
/// than deciding presence themselves.
#[must_use]
pub fn stale_after_ms() -> i64 {
    (STALE_AFTER_SECS as i64) * 1000
}
