//! The machine directory, as a client sees it (T-0104).
//!
//! One sentence: which machines exist, what they are called, and whether they
//! are up — plus the read-only cache a client keeps so names resolve with the
//! relay unreachable.
//!
//! **Why the cache is here at all.** It is pure: a `BTreeMap` with no store, no
//! file and no socket, and its only mutator replaces the whole set with what the
//! relay last said (`mirror`) — the core says so in the same words, and that is
//! what makes it safe to hand a phone. There is deliberately no `insert`,
//! `rename` or `remove` to expose, so a phone's copy cannot become a second
//! source of truth.

use std::sync::{Arc, Mutex, MutexGuard};

use arreo_core::mesh::{CachedMachine, DirectoryCache, MachineId, MachineRow, Name, Presence};

use crate::errors::DirectoryFfiError;

/// Whether a machine is reachable, from its last heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiPresence {
    /// Heartbeat within the online window.
    Online,
    /// Seen before, but not recently.
    Offline,
    /// Not seen for the stale window; its name may be reclaimed.
    Stale,
}

impl From<Presence> for FfiPresence {
    fn from(presence: Presence) -> Self {
        match presence {
            Presence::Online => Self::Online,
            Presence::Offline => Self::Offline,
            Presence::Stale => Self::Stale,
        }
    }
}

impl From<FfiPresence> for Presence {
    fn from(presence: FfiPresence) -> Self {
        match presence {
            FfiPresence::Online => Self::Online,
            FfiPresence::Offline => Self::Offline,
            FfiPresence::Stale => Self::Stale,
        }
    }
}

/// The operator's word for a presence (`online`, `offline`, `stale`) — the same
/// one the CLI prints.
#[uniffi::export]
#[must_use]
pub fn presence_word(presence: FfiPresence) -> String {
    Presence::from(presence).as_str().to_string()
}

/// One machine's directory row.
///
/// `machine_id` is the bare 32-hex fingerprint of the machine's key — the same
/// shape a device id has, and the same derivation, because the product has one
/// fingerprint rule and not two. The *role* is what differs: a machine is a host
/// in the directory, a device is a client the host trusts.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MachineRowInfo {
    /// The machine's fingerprint, bare 32 hex.
    pub machine_id: String,
    /// Its name, canonicalized (lowercase).
    pub name: String,
    /// Set when the machine asked for a name that was taken and was given a
    /// suffix instead — how "you asked for `workbox` and got `workbox-2`"
    /// reaches the user.
    pub name_conflict: bool,
    pub presence: FfiPresence,
    /// Unix milliseconds of the last heartbeat.
    pub last_seen_ms: i64,
    /// The protocol version the machine's daemon speaks.
    pub proto_version: u32,
    /// Set while a removed name is held for this machine; absent for a live row.
    pub tombstone_until_ms: Option<i64>,
    /// Hex of the public key a client must dial to reach this machine's daemon,
    /// or absent for a row written before the field existed.
    pub daemon_key: Option<String>,
}

impl MachineRowInfo {
    /// From a directory row (a live answer from the relay).
    pub(crate) fn from_row(row: &MachineRow) -> Self {
        Self {
            machine_id: row.machine_id.as_str().to_string(),
            name: row.name.as_str().to_string(),
            name_conflict: row.name_conflict,
            presence: FfiPresence::from(row.presence),
            last_seen_ms: row.last_seen_ms,
            proto_version: row.proto_version,
            tombstone_until_ms: row.tombstone_until_ms,
            daemon_key: row.daemon_key.clone(),
        }
    }

    /// From a cached row. The cache keys by name and stores no name of its own
    /// (a second copy could disagree with the key), so the name comes in.
    ///
    /// **The dial key is always absent here**, and that is the cache's own rule
    /// rather than an omission: it is a *name* mirror, and a key a client could
    /// dial out of a stale row is exactly the "a copy acts like a source of
    /// truth" hazard the cache's type exists to prevent.
    pub(crate) fn from_cached(name: &str, row: &CachedMachine) -> Self {
        Self {
            machine_id: row.machine_id.as_str().to_string(),
            name: name.to_string(),
            name_conflict: row.name_conflict,
            presence: FfiPresence::from(row.presence),
            last_seen_ms: row.last_seen_ms,
            proto_version: row.proto_version,
            tombstone_until_ms: row.tombstone_until_ms,
            daemon_key: None,
        }
    }
}

/// A client's read-only mirror of the account's directory.
#[derive(uniffi::Object, Default)]
pub struct DirectoryCacheHandle {
    /// Interior mutability because `mirror` needs `&mut DirectoryCache` and a
    /// UniFFI object is shared by `Arc` — the boundary cannot express `&mut
    /// self`, so the lock is where that difference is paid.
    cache: Mutex<DirectoryCache>,
}

#[uniffi::export]
#[must_use]
pub fn directory_cache_new() -> Arc<DirectoryCacheHandle> {
    Arc::new(DirectoryCacheHandle::default())
}

#[uniffi::export]
impl DirectoryCacheHandle {
    /// Replace the mirror with what the relay last said.
    ///
    /// The whole set is replaced, never merged: a merge would let a stale name
    /// survive a rename the relay already made.
    pub fn mirror(&self, rows: Vec<MachineRowInfo>, as_of_ms: i64) {
        let rows: Vec<MachineRow> = rows.iter().filter_map(to_row).collect();
        self.lock().mirror(&rows, as_of_ms);
    }

    /// When this mirror was learned. `0` means "never contacted the relay".
    #[must_use]
    pub fn as_of_ms(&self) -> i64 {
        self.lock().as_of_ms()
    }

    #[must_use]
    pub fn len(&self) -> u64 {
        self.lock().len() as u64
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Every cached row, sorted by name — deterministic, so two runs agree.
    #[must_use]
    pub fn rows(&self) -> Vec<MachineRowInfo> {
        let cache = self.lock();
        cache
            .rows()
            .into_iter()
            .map(|(name, row)| MachineRowInfo::from_cached(name, row))
            .collect()
    }

    /// Resolve a name to the machine the relay last said holds it.
    pub fn lookup(&self, name: String) -> Result<Option<MachineRowInfo>, DirectoryFfiError> {
        let name = Name::parse(&name)?;
        let cache = self.lock();
        Ok(cache
            .lookup(&name)
            .map(|row| MachineRowInfo::from_cached(name.as_str(), row)))
    }

    /// Resolve a row for a machine we already know, without letting the answer
    /// become a directory entry — the LAN/mDNS door: a peer may tell us *where* a
    /// known machine is, and may never tell us that a machine exists.
    pub fn resolve_known(
        &self,
        machine_id: String,
    ) -> Result<Option<MachineRowInfo>, DirectoryFfiError> {
        let machine_id = MachineId::parse(&machine_id)?;
        let cache = self.lock();
        let name = cache
            .rows()
            .into_iter()
            .find(|(_, row)| row.machine_id == machine_id)
            .map_or_else(String::new, |(name, _)| name.to_string());
        Ok(cache
            .resolve_known(&machine_id)
            .map(|row| MachineRowInfo::from_cached(&name, row)))
    }
}

impl DirectoryCacheHandle {
    fn lock(&self) -> MutexGuard<'_, DirectoryCache> {
        // A poisoned cache is still a consistent `BTreeMap` — the only writer is
        // `mirror`, which replaces the map in one assignment — so recovering the
        // guard is strictly better than refusing every later read.
        match self.cache.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// A row the caller handed in, back as the core's type.
///
/// A machine id or a name that does not parse is **dropped** rather than
/// refused: this is a mirror of what the relay said, and one row that cannot be
/// represented is not a reason to lose the rows that can. The parse rules are
/// still enforced where a caller's own input is at stake — `lookup`, above.
fn to_row(info: &MachineRowInfo) -> Option<MachineRow> {
    Some(MachineRow {
        machine_id: MachineId::parse(&info.machine_id).ok()?,
        name: Name::parse(&info.name).ok()?,
        name_conflict: info.name_conflict,
        presence: Presence::from(info.presence),
        last_seen_ms: info.last_seen_ms,
        proto_version: info.proto_version,
        tombstone_until_ms: info.tombstone_until_ms,
        daemon_key: info.daemon_key.clone(),
    })
}
