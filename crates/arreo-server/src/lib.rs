//! Arreo server library: daemon internals (PTY sessions, socket API v1).
//!
//! T-0014: framed MessagePack `Message` over the Unix socket (Linux/macOS).
//! The T-0005 JSONL framing is gone — one framing, not two.

pub mod audit;
pub mod daemon;
pub mod devices;
pub mod handoff;
pub mod lifecycle;
pub mod persist;
pub mod protocol;
pub mod relay_client;
pub mod transport;

pub use arreo_core::transport::TEST_LISTEN_ENV;

pub use audit::{Actor, SessionAudit};
pub use daemon::Daemon;
pub use devices::{AuthorityError, DeviceAuthority, Layout};
pub use lifecycle::{unit_file, unit_path, DrainReport, ServiceKind, SHUTDOWN_DEADLINE};
// The trust ledger is `arreo-core`'s (its store layer always was; the CLI writes
// grants and may not depend on this crate). Re-exported so a daemon caller has
// one import path for it.
pub use arreo_core::mesh::{GrantedDevice, LedgerError, SharedLedger, TrustLedger, TrustRefusal};
pub use persist::{db_path_for, restore, snapshot, PersistError};
pub use protocol::{AgentState, Message, PaneInfo, VERSION};
pub use relay_client::{
    backoff_delay, load_config, own_identity, RelayContext, RelaySession, RelaySettings,
    RelayStream, SessionError,
};
