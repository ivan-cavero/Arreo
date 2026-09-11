//! Device authority wiring for the daemon (T-0025).
//!
//! The authority itself lives in `arreo_core::identity::authority` — both the
//! daemon and the CLI operate on the same files and store, and the dependency
//! rule (T-0001) puts shared implementations in core. This module is the
//! server-side seam: it names the layout for a daemon socket and re-exports
//! what the daemon (and its tests) need.

pub use arreo_core::identity::authority::{
    client_key, client_key_path, now_ms, sidecar_db, AuthorityError, DeviceAuthority, Layout,
};

/// Load (or bootstrap) the authority for a daemon serving `socket`.
///
/// Called at boot, before the listener opens: a device-gated session must never
/// be possible against an authority that failed to load.
pub fn load_for_socket(socket: &std::path::Path) -> Result<DeviceAuthority, AuthorityError> {
    DeviceAuthority::load(Layout::for_socket(socket))
}
