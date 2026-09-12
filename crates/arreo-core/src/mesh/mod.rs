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
/// The machine-local trust ledger. Gated like [`crate::store`] itself: it lives
/// on a `SessionStore`, and the sqlite-free build (the pure-Rust surface
/// `check-targets` type-checks for foreign targets) has no store to put it on.
/// The *rule* above is ungated — it is pure types and arithmetic, and a build
/// without a database can still answer "may this device do this".
#[cfg(feature = "sqlite")]
pub mod ledger;
/// Name → dialable target, through the account's directory (T-0045's dial key).
///
/// Gated like [`session`]: it hands back a `Target`, and the local half of that is
/// a Unix socket. Two clients resolve names — `arreo attach` and `arreo-tui` — and
/// they share this one implementation rather than each reading the directory
/// themselves; `arreo-tui` cannot depend on the CLI, so the shared home is here.
#[cfg(all(feature = "transport", unix))]
pub mod resolve;
/// The daemon client, over either transport (the Unix socket, or the relay to
/// another machine).
///
/// Gated on `transport` **and `unix`**: the local half connects to a Unix socket,
/// which is the only local transport the product has today (the daemon's listener
/// is a `UnixListener` too), and the two halves live in one client because
/// everything above it sees only [`session::Target`]. When Windows gets its local
/// transport (a named pipe), this gate splits and the remote half becomes
/// available there on its own — which is worth doing, since the remote path is
/// what a Windows machine would use.
#[cfg(all(feature = "transport", unix))]
pub mod session;
pub mod trust;

pub use directory::{
    default_machine_name, CachedMachine, DirectoryCache, DirectoryError, MachineId, MachineRow,
    Name, Presence, HEARTBEAT_SECS, JOIN_TICKET_SECS, NAME_MAX, ONLINE_WINDOW_SECS,
    STALE_AFTER_SECS, TOMBSTONE_SECS,
};
#[cfg(feature = "sqlite")]
pub use ledger::{GrantedDevice, LedgerError, SharedLedger, TrustLedger, TrustRefusal};
#[cfg(all(feature = "transport", unix))]
pub use session::{Client as MeshClient, ClientError as MeshClientError, RemoteTarget, Target};
pub use trust::{denial_message, evaluate, TrustDenial, TrustRecord};
