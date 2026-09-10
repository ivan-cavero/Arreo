//! Arreo core: PTY management, VT state, state engine, metrics, protocol.
//!
//! The dependency direction is one-way: `arreo-server` and `arreo-cli` depend
//! on this crate; nothing in core depends on them (enforced by
//! `xtask/tests/workspace_deps.rs`, T-0001).

pub mod pty;
