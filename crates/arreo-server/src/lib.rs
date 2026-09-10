//! Arreo server library: daemon internals (PTY sessions, socket API v1).
//!
//! T-0014: framed MessagePack `Message` over the Unix socket (Linux/macOS).
//! The T-0005 JSONL framing is gone — one framing, not two.

pub mod daemon;
pub mod lifecycle;
pub mod protocol;

pub use daemon::Daemon;
pub use lifecycle::{unit_file, unit_path, DrainReport, ServiceKind, SHUTDOWN_DEADLINE};
pub use protocol::{AgentState, Message, PaneInfo, VERSION};
