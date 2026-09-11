//! The directory's vocabulary: machine identity, name rules, presence, and the
//! read-only cache a server keeps (T-0043).
//!
//! Everything here is pure: no I/O, no clock reads, no store. The relay applies
//! these rules inside its transactions; the server applies them to a cache it
//! cannot write to.

use crate::identity::VerifyingKey;
use serde::{Deserialize, Serialize};

/// Longest name the rule admits (`[a-z0-9][a-z0-9-]{0,31}`).
pub const NAME_MAX: usize = 32;

/// The heartbeat a machine is expected to send.
pub const HEARTBEAT_SECS: u64 = 30;

/// A machine is `online` while its last heartbeat is at most this old — three
/// missed heartbeats, so one lost packet is not an outage.
pub const ONLINE_WINDOW_SECS: u64 = 90;

/// Beyond this, a machine is `stale` rather than merely offline: it has not been
/// seen for long enough that its name is up for reclaiming.
pub const STALE_AFTER_SECS: u64 = 30 * 24 * 60 * 60;

/// How long a removed name is held for the machine that had it.
pub const TOMBSTONE_SECS: u64 = 30 * 24 * 60 * 60;

/// How long a join code stays valid (ROADMAP §4).
pub const JOIN_TICKET_SECS: u64 = 5 * 60;

/// Everything that can go wrong parsing or applying directory rules.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DirectoryError {
    #[error("not a machine id: {0:?} (want 32 hex characters)")]
    BadMachineId(String),
    #[error(
        "invalid machine name {0:?}: names are ASCII lowercase letters, digits and \
         hyphens, 1-{NAME_MAX} characters, and must not start with a hyphen"
    )]
    InvalidName(String),
    #[error("a machine is already registered under {0:?}")]
    NameTaken(String),
    #[error("no machine {0:?} in this directory")]
    NoSuchMachine(String),
    #[error("no machine is registered under {0:?}")]
    NoSuchName(String),
    #[error("the join ticket is {state}")]
    Ticket { state: &'static str },
}

/// A machine's identity: the fingerprint of its ed25519 key.
///
/// The same 32-hex shape as a [`crate::identity::DeviceId`], derived the same
/// way (`sha256(pubkey)` truncated to 128 bits) — one fingerprint rule in the
/// product, not two. The type is distinct because the *role* is distinct: a
/// machine is a host in the directory, a device is a client the host trusts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MachineId(String);

impl MachineId {
    /// Derive the id from a public key.
    #[must_use]
    pub fn from_key(key: &VerifyingKey) -> Self {
        Self(
            crate::identity::DeviceId::from_key(key)
                .as_str()
                .to_string(),
        )
    }

    /// Parse an id from its textual form. Length and alphabet are checked, so an
    /// id from a log line or a URL cannot smuggle anything into a query.
    pub fn parse(text: &str) -> Result<Self, DirectoryError> {
        let trimmed = text.trim();
        if trimmed.len() != 32 || !trimmed.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(DirectoryError::BadMachineId(trimmed.to_string()));
        }
        Ok(Self(trimmed.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for MachineId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A machine name, canonicalized and validated.
///
/// **Why there is no NFC normalization step.** The rule admits
/// `[a-z0-9][a-z0-9-]{0,31}` — ASCII by construction — and NFC is the identity
/// over ASCII, so normalizing first and validating after would accept exactly
/// the same set as validating the casefolded form directly. A name containing
/// non-ASCII is *refused*, which is strictly stronger than normalizing it: a
/// Unicode lookalike (`wоrkbox` with a Cyrillic `о`) can never enter the
/// directory at all, so it can never collide with, or be mistaken for, an
/// existing machine. Rejecting costs one clear error message; accepting costs a
/// confusable-name problem the directory would then own forever.
///
/// Case is normalized, not preserved: `Workbox` and `WORKBOX` are the same
/// machine name `workbox`. That is the uniqueness rule the schema enforces
/// (`UNIQUE(account_id, name_key)`), stated here once so the relay and the cache
/// cannot disagree about it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Name(String);

impl Name {
    /// Parse and canonicalize a requested name.
    pub fn parse(text: &str) -> Result<Self, DirectoryError> {
        let lowered = text.trim().to_ascii_lowercase();
        // Reject anything that is not ASCII *after* lowercasing: this is where a
        // confusable or a stray byte is turned away, before it can be stored.
        if lowered.is_empty()
            || lowered.len() > NAME_MAX
            || !lowered
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || lowered.starts_with('-')
        {
            return Err(DirectoryError::InvalidName(text.trim().to_string()));
        }
        Ok(Self(lowered))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The next candidate when this name is taken: `workbox` → `workbox-2`.
    ///
    /// The suffix is applied to a *truncated* base so the result still satisfies
    /// the length rule — a 32-character name plus `-2` would otherwise be
    /// rejected by our own validator, which would make the conflict path fail
    /// for exactly the longest names.
    #[must_use]
    pub fn with_suffix(&self, ordinal: usize) -> Self {
        let suffix = format!("-{ordinal}");
        let keep = NAME_MAX.saturating_sub(suffix.len());
        let base = &self.0[..self.0.len().min(keep)];
        // `base` cannot start with a hyphen (it never did), and trimming cannot
        // make it empty for a valid name, so this is always parseable.
        Self(format!("{base}{suffix}"))
    }
}

impl std::fmt::Display for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Whether a machine is reachable, from its last heartbeat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Presence {
    /// Heartbeat within [`ONLINE_WINDOW_SECS`].
    Online,
    /// Seen before, but not recently.
    Offline,
    /// Not seen for [`STALE_AFTER_SECS`]; its name may be reclaimed.
    Stale,
}

impl Presence {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Offline => "offline",
            Self::Stale => "stale",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "online" => Some(Self::Online),
            "offline" => Some(Self::Offline),
            "stale" => Some(Self::Stale),
            _ => None,
        }
    }
}

/// The single presence rule (T-0043's contract; the relay's presence component
/// consumes it rather than re-deriving its own).
///
/// `stale` outranks the online window: a machine that has been gone for a month
/// is not "offline for a long time", it is a name that is up for reclaiming.
#[must_use]
pub fn presence_of(last_seen_ms: i64, now_ms: i64) -> Presence {
    let age_ms = now_ms.saturating_sub(last_seen_ms).max(0);
    let age_secs = age_ms / 1000;
    if age_secs > STALE_AFTER_SECS as i64 {
        Presence::Stale
    } else if age_secs <= ONLINE_WINDOW_SECS as i64 {
        Presence::Online
    } else {
        Presence::Offline
    }
}

/// Age of a heartbeat, in milliseconds, clamped at zero so a clock that stepped
/// backwards reports "just now" rather than a negative age.
#[must_use]
pub fn age_ms(last_seen_ms: i64, now_ms: i64) -> i64 {
    now_ms.saturating_sub(last_seen_ms).max(0)
}

/// One machine as the directory holds it.
///
/// Serialized field order is the export order; the export is sorted by
/// `machine_id` so two runs over the same directory produce identical bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineRow {
    pub machine_id: MachineId,
    pub name: Name,
    /// Set when this machine could not have the name it asked for and was given
    /// a suffix instead. The flag is how "you asked for `workbox` and got
    /// `workbox-2`" reaches the user, rather than being a silent difference
    /// between what they typed and what the directory shows.
    pub name_conflict: bool,
    pub presence: Presence,
    pub last_seen_ms: i64,
    pub proto_version: u32,
    /// Set while a removed name is held for this machine; `None` for a live row.
    pub tombstone_until_ms: Option<i64>,
}

impl MachineRow {
    /// Is the tombstone on this row still holding the name?
    #[must_use]
    pub fn tombstone_active(&self, now_ms: i64) -> bool {
        self.tombstone_until_ms.is_some_and(|until| until > now_ms)
    }
}

/// The canonical, sorted export of a directory.
///
/// One line per machine, tab-separated, sorted by machine id — the form two runs
/// can be compared byte-for-byte, which is what the consistency check needs.
/// Absent values are the empty field, never a missing column, so the parser
/// cannot misread a shift.
#[must_use]
pub fn export_rows(rows: &[MachineRow]) -> String {
    let mut sorted: Vec<&MachineRow> = rows.iter().collect();
    sorted.sort_by(|a, b| a.machine_id.cmp(&b.machine_id));
    let mut out = String::new();
    for row in sorted {
        out.push_str(row.machine_id.as_str());
        out.push('\t');
        out.push_str(row.name.as_str());
        out.push('\t');
        out.push_str(if row.name_conflict { "conflict" } else { "ok" });
        out.push('\t');
        out.push_str(row.presence.as_str());
        out.push('\t');
        out.push_str(&row.last_seen_ms.to_string());
        out.push('\t');
        out.push_str(&row.proto_version.to_string());
        out.push('\t');
        out.push_str(
            &row.tombstone_until_ms
                .map_or(String::new(), |v| v.to_string()),
        );
        out.push('\n');
    }
    out
}

/// Parse an export back into rows. Returns `None` on a malformed line rather
/// than guessing, so a round-trip test cannot pass on a lenient parser.
#[must_use]
pub fn parse_export(text: &str) -> Option<Vec<MachineRow>> {
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() != 7 {
            return None;
        }
        let machine_id = MachineId::parse(fields[0]).ok()?;
        let name = Name::parse(fields[1]).ok()?;
        let name_conflict = match fields[2] {
            "conflict" => true,
            "ok" => false,
            _ => return None,
        };
        let presence = Presence::parse(fields[3])?;
        let last_seen_ms = fields[4].parse().ok()?;
        let proto_version = fields[5].parse().ok()?;
        let tombstone_until_ms = if fields[6].is_empty() {
            None
        } else {
            Some(fields[6].parse().ok()?)
        };
        rows.push(MachineRow {
            machine_id,
            name,
            name_conflict,
            presence,
            last_seen_ms,
            proto_version,
            tombstone_until_ms,
        });
    }
    Some(rows)
}

/// One machine as a *server* caches it.
///
/// The fields are the ones a *renderer* needs when the relay is unreachable
/// (T-0044's `source: "cache"`): who it is, how alive it looked, when, what it
/// spoke, and whether its name needed a suffix. Nothing here is derived — every
/// value is what the relay said, stored as it said it, because a cache that
/// computes its own presence would be a second presence rule (T-0043 has one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachedMachine {
    pub machine_id: MachineId,
    pub presence: Presence,
    pub last_seen_ms: i64,
    #[serde(default)]
    pub proto_version: u32,
    #[serde(default)]
    pub name_conflict: bool,
    /// When the name's tombstone expires, as the relay said it (T-0057): a
    /// removed machine keeps its name for that window, and a renderer that
    /// cannot say so would show a tombstoned machine as an ordinary one.
    #[serde(default)]
    pub tombstone_until_ms: Option<i64>,
}

/// A server's read-only mirror of the directory.
///
/// **Why "read-only" is a type and not a policy.** ROADMAP §3.7 wants names to
/// resolve with the relay unreachable, which means every server holds a copy;
/// the risk is that a copy starts acting like a source of truth (a stale cache
/// that renames or resurrects a machine). So the only way to change this value
/// is [`DirectoryCache::mirror`], which replaces the whole set with what the
/// relay said, and there is deliberately no `insert`, `rename` or `remove` to
/// call. Divergence resolves relay-first on the next contact; until then every
/// answer carries the `as_of` stamp it was learned at, so a caller can tell a
/// live answer from a remembered one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryCache {
    as_of_ms: i64,
    rows: std::collections::BTreeMap<String, CachedMachine>,
}

impl DirectoryCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the mirror with what the relay last said. The whole set is
    /// replaced, never merged: a merge would let a stale name survive a rename
    /// the relay already made.
    pub fn mirror(&mut self, rows: &[MachineRow], as_of_ms: i64) {
        self.as_of_ms = as_of_ms;
        self.rows.clear();
        for row in rows {
            self.rows.insert(
                row.name.as_str().to_string(),
                CachedMachine {
                    machine_id: row.machine_id.clone(),
                    presence: row.presence,
                    last_seen_ms: row.last_seen_ms,
                    proto_version: row.proto_version,
                    name_conflict: row.name_conflict,
                    tombstone_until_ms: row.tombstone_until_ms,
                },
            );
        }
    }

    /// When this mirror was learned. `0` means "never contacted the relay".
    #[must_use]
    pub fn as_of_ms(&self) -> i64 {
        self.as_of_ms
    }

    /// Resolve a name to the machine the relay last said holds it.
    #[must_use]
    pub fn lookup(&self, name: &Name) -> Option<&CachedMachine> {
        self.rows.get(name.as_str())
    }

    /// Resolve an address for a machine we already know, without letting the
    /// answer become a directory entry. This is the LAN/mDNS door: a peer may
    /// tell us where a known `machine_id` is, and may never tell us that a
    /// machine exists, is renamed, or is gone.
    #[must_use]
    pub fn resolve_known(&self, machine_id: &MachineId) -> Option<&CachedMachine> {
        self.rows
            .values()
            .find(|cached| &cached.machine_id == machine_id)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Every cached row, sorted by name (the map is a `BTreeMap`, so this is
    /// deterministic and stable across runs).
    #[must_use]
    pub fn rows(&self) -> Vec<(&str, &CachedMachine)> {
        self.rows
            .iter()
            .map(|(name, row)| (name.as_str(), row))
            .collect()
    }
}

/// This machine's default name: its hostname, or a stable fallback.
///
/// One implementation, because two callers want it — the daemon asserting its
/// directory row (T-0056) and the CLI naming a joining device (T-0044) — and two
/// copies would be two spellings of the same fact waiting to drift. Reads
/// `/etc/hostname` first (the file, not the environment: a shell that forgot to
/// export `HOSTNAME` should not rename a machine), then `HOSTNAME`, then a fixed
/// fallback so a container without either still gets a usable name.
#[must_use]
pub fn default_machine_name() -> String {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .or_else(|| {
            std::env::var("HOSTNAME")
                .ok()
                .filter(|text| !text.is_empty())
        })
        .unwrap_or_else(|| "machine".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_validated_and_case_normalized() {
        assert_eq!(Name::parse("workbox").expect("plain").as_str(), "workbox");
        assert_eq!(
            Name::parse("  WORKBOX  ").expect("case").as_str(),
            "workbox"
        );
        assert_eq!(Name::parse("pi5").expect("short").as_str(), "pi5");
        assert_eq!(Name::parse("a-1-2").expect("hyphens").as_str(), "a-1-2");

        for bad in [
            "",
            "-lead",
            "has space",
            "under_score",
            "dot.dot",
            "ümlaut",
            "workbox!",
        ] {
            assert!(
                matches!(Name::parse(bad), Err(DirectoryError::InvalidName(_))),
                "{bad:?} must be refused"
            );
        }
        // The Cyrillic `о` in "wоrkbox" is the confusable the ASCII rule turns
        // away; without it, the directory would have two machines that look
        // identical on screen.
        assert!(
            Name::parse("w\u{043e}rkbox").is_err(),
            "confusable must be refused"
        );
    }

    #[test]
    fn a_name_at_the_length_limit_still_takes_a_suffix() {
        let long = "a".repeat(NAME_MAX);
        let name = Name::parse(&long).expect("32 characters is legal");
        let suffixed = name.with_suffix(2);
        assert_eq!(suffixed.as_str().len(), NAME_MAX);
        assert!(suffixed.as_str().ends_with("-2"));
        // And the result is itself a legal name — the point of truncating.
        assert!(Name::parse(suffixed.as_str()).is_ok());
    }

    #[test]
    fn presence_follows_the_stated_thresholds() {
        let now = 1_000_000_000_000i64;
        // Within the online window.
        assert_eq!(presence_of(now - 30_000, now), Presence::Online);
        assert_eq!(presence_of(now - 90_000, now), Presence::Online);
        // Past it, but not stale.
        assert_eq!(presence_of(now - 91_000, now), Presence::Offline);
        assert_eq!(
            presence_of(now - (STALE_AFTER_SECS as i64) * 1000, now),
            Presence::Offline,
            "exactly at the boundary is still offline, not stale"
        );
        assert_eq!(
            presence_of(now - (STALE_AFTER_SECS as i64 + 1) * 1000, now),
            Presence::Stale
        );
        // A clock that stepped backwards reports "just now", never a negative age.
        assert_eq!(presence_of(now + 60_000, now), Presence::Online);
        assert_eq!(age_ms(now + 60_000, now), 0);
    }

    #[test]
    fn the_export_round_trips_and_is_order_independent() {
        let a = row(
            "11111111111111111111111111111111",
            "alpha",
            Presence::Online,
            5,
        );
        let b = row(
            "22222222222222222222222222222222",
            "beta",
            Presence::Offline,
            0,
        );
        let mut reversed = vec![b.clone(), a.clone()];
        let export = export_rows(&reversed);
        // Sorted by machine id, so insertion order cannot change the bytes.
        let parsed = parse_export(&export).expect("our own export parses");
        assert_eq!(parsed.len(), 2);
        reversed.sort_by(|x, y| x.machine_id.cmp(&y.machine_id));
        assert_eq!(parsed, reversed);
        assert_eq!(export_rows(&parsed), export, "round-trip is byte-identical");

        // A malformed line is refused rather than guessed at.
        assert!(parse_export("not\ta\trow\n").is_none());
        assert!(parse_export(&export.replace('\t', " ")).is_none());
    }

    #[test]
    fn the_cache_only_ever_mirrors_what_the_relay_said() {
        let mut cache = DirectoryCache::new();
        assert!(cache.is_empty());
        assert_eq!(cache.as_of_ms(), 0);

        let rows = vec![
            row(
                "11111111111111111111111111111111",
                "alpha",
                Presence::Online,
                1,
            ),
            row(
                "22222222222222222222222222222222",
                "beta",
                Presence::Online,
                1,
            ),
        ];
        cache.mirror(&rows, 42);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.as_of_ms(), 42);

        let alpha = Name::parse("alpha").expect("name");
        let machine = MachineId::parse("11111111111111111111111111111111").expect("id");
        assert_eq!(cache.lookup(&alpha).expect("cached").machine_id, machine);
        assert!(cache.resolve_known(&machine).is_some());

        // A machine the relay did not list is gone, and mirroring cannot
        // resurrect it: the mirror replaces, it never merges.
        cache.mirror(&rows[..1], 43);
        assert!(cache.lookup(&Name::parse("beta").expect("name")).is_none());
        assert!(cache
            .resolve_known(&MachineId::parse("22222222222222222222222222222222").expect("id"))
            .is_none());
        assert_eq!(cache.as_of_ms(), 43);
    }

    fn row(id: &str, name: &str, presence: Presence, proto: u32) -> MachineRow {
        MachineRow {
            machine_id: MachineId::parse(id).expect("id"),
            name: Name::parse(name).expect("name"),
            name_conflict: false,
            presence,
            last_seen_ms: 1,
            proto_version: proto,
            tombstone_until_ms: None,
        }
    }
}
