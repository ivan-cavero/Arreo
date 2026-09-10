//! Arreo core: PTY management, VT state, state engine, metrics, protocol.
//!
//! The dependency direction is one-way: `arreo-server` and `arreo-cli` depend
//! on this crate; nothing in core depends on them (enforced by
//! `xtask/tests/workspace_deps.rs`, T-0001).

pub mod enforce;
pub mod fixtures;
pub mod lifecycle;
pub mod metrics;
pub mod proto;
pub mod pty;
pub mod state;
#[cfg(feature = "sqlite")]
pub mod store;
pub mod vt;
