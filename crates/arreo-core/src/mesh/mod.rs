//! Machine directory (T-0043, ROADMAP §3.7): which machines exist, what they
//! are called, and whether they are up.
//!
//! One sentence: the account owns *machines*; the directory is metadata-only
//! and lives at the relay, while the names have to resolve on a machine even
//! when the relay is unreachable — so this module is the vocabulary (identity,
//! name rules, presence thresholds, the read-only cache) and `arreo-relay` owns
//! the durable table.
//!
//! Why the split. The relay is AGPL and self-hostable; the server and the TUI
//! are not, and must not link it. Putting the *types and rules* here means the
//! two sides agree on what a name is by construction rather than by convention,
//! and the relay's SQLite store is the only thing that differs between the
//! managed and self-hosted tiers.
//!
//! What this module deliberately does **not** hold: agent state, device grants,
//! or anything secret. The directory answers "which machines and are they up" —
//! a schema test in `arreo-relay` fails if such a column ever appears, and the
//! types here have nowhere to put one.

pub mod directory;
pub mod trust;

pub use directory::{
    default_machine_name, CachedMachine, DirectoryCache, DirectoryError, MachineId, MachineRow,
    Name, Presence, HEARTBEAT_SECS, JOIN_TICKET_SECS, NAME_MAX, ONLINE_WINDOW_SECS,
    STALE_AFTER_SECS, TOMBSTONE_SECS,
};
pub use trust::{denial_message, evaluate, TrustDenial, TrustRecord};
