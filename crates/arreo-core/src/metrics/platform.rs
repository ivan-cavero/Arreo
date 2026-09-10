//! Cross-platform probe trait + per-OS modules.
//!
//! Linux is real here (T-0006). Windows/macOS return `Unimplemented` with a
//! pointer to the task that owns them — honest stubs, never fake numbers.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ProbeError {
    #[error("process {0} not found")]
    NotFound(u32),
    #[error("proc read: {0}")]
    Io(#[from] std::io::Error),
    #[error("proc parse: {0}")]
    Parse(String),
    #[error("unimplemented on this OS (see {0})")]
    Unimplemented(&'static str),
}

/// One instantaneous sample of a process tree.
#[derive(Debug, Clone)]
pub struct TreeSample {
    /// All PIDs in the tree (root first).
    pub pids: Vec<u32>,
    /// Summed RSS across the tree, bytes.
    pub rss_bytes: u64,
    /// Summed utime+stime ticks across the tree (for CPU% deltas).
    pub total_ticks: u64,
    /// cgroup v2 `memory.current` for the root's cgroup, if available.
    pub cgroup_bytes: Option<u64>,
}

/// OS probe: list tree, sum RSS/ticks.
pub trait Probe: Send + Sync {
    fn sample_tree(&self, root: u32) -> Result<TreeSample, ProbeError>;
    fn clock_ticks_per_sec(&self) -> u64;
}

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use linux::LinuxProbe;

#[cfg(not(target_os = "linux"))]
pub mod other;
#[cfg(not(target_os = "linux"))]
pub use other::OtherProbe;
