//! Arreo server library: daemon internals (PTY sessions, socket API v1).
//!
//! T-0014: framed MessagePack `Message` over the Unix socket (Linux/macOS).
//! The T-0005 JSONL framing is gone — one framing, not two.

pub mod daemon;
pub mod devices;
pub mod lifecycle;
pub mod persist;
pub mod protocol;
pub mod relay_client;
pub mod transport;

pub use arreo_core::transport::TEST_LISTEN_ENV;

pub use daemon::Daemon;
pub use devices::{AuthorityError, DeviceAuthority, Layout};
pub use lifecycle::{unit_file, unit_path, DrainReport, ServiceKind, SHUTDOWN_DEADLINE};
pub use persist::{db_path_for, restore, snapshot, PersistError};
pub use protocol::{AgentState, Message, PaneInfo, VERSION};
pub use relay_client::{backoff_delay, RelaySession, RelayStream, SessionError};
