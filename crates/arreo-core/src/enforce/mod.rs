//! Resource enforcement v0 (T-0019): per-agent cgroup v2 budgets on Linux.
//!
//! One sentence: each pane gets its own child cgroup with `memory.max` +
//! `pids.max`; breach surfaces as a `Throttled` event (notify first) and an
//! optional kill — the harness tells you BEFORE it acts.
//!
//! Layout: `Guard` owns the Linux mechanism; `platform` holds the honest
//! matrix stubs (Windows Job Objects / macOS rlimit land later, tracked by
//! `EnforceError::Unimplemented`). Groups live under OUR OWN cgroup scope
//! (resolved from `/proc/self/cgroup`), so unprivileged user daemons work —
//! no root, no systemd-run needed for v0.
//!
//! Breach semantics: `breached()` reads `memory.events` (`max` counter) and
//! `pids.events` — kernel-counted, no polling heuristics. `Throttled` means
//! "at ceiling now"; the daemon notifies (state event) and, if configured,
//! kills (the kill switch).

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::Guard;

#[cfg(not(target_os = "linux"))]
mod other;
#[cfg(not(target_os = "linux"))]
pub use other::Guard;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EnforceError {
    #[error("enforce io: {0}")]
    Io(#[from] std::io::Error),
    #[error("cgroup unavailable: {0}")]
    Unavailable(String),
    #[error("unimplemented on this OS (see {0})")]
    Unimplemented(&'static str),
}

/// Per-pane budget. `None` = no limit on that controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub memory_max: Option<u64>,
    pub pids_max: Option<u32>,
}

impl Budget {
    /// No limits (guard still tracks membership + breach state).
    #[must_use]
    pub const fn unlimited() -> Self {
        Self {
            memory_max: None,
            pids_max: None,
        }
    }
}

/// What `breached()` found (kernel-counted, not guessed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Breach {
    /// `memory.events:max` fired (or OOM-killed a member).
    Memory,
    /// `pids.events:max` fired (fork refused).
    Pids,
}
