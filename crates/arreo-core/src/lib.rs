//! Arreo core: PTY management, VT state, state engine, metrics, protocol.
//!
//! The dependency direction is one-way: `arreo-server` and `arreo-cli` depend
//! on this crate; nothing in core depends on them (enforced by
//! `xtask/tests/workspace_deps.rs`, T-0001).

pub mod enforce;
pub mod fixtures;
pub mod identity;
pub mod lifecycle;
pub mod mesh;
pub mod metrics;
pub mod pairing;
pub mod proto;
pub mod pty;
pub mod state;
#[cfg(feature = "sqlite")]
pub mod store;
pub mod theme;
#[cfg(feature = "transport")]
pub mod transport;
pub mod vt;

/// Write bytes to an owner-only file (0600 on Unix, created before writing).
/// Shared by identity key files and device certificates (T-0025).
pub(crate) fn write_private_bytes(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}
