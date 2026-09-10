//! Arreo server library: daemon internals (PTY sessions, socket API).
//!
//! T-0005: JSON-lines protocol over a Unix socket (Linux/macOS). The
//! versioned MessagePack protocol (T-0013) supersedes this wire format —
//! clients must not assume JSONL past Phase 0 (documented in the protocol
//! module, enforced by the `v` field).

pub mod daemon;
pub mod protocol;

pub use daemon::Daemon;
pub use protocol::{Request, Response};
