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

/// One cgroup pressure reading (T-0041): the ceiling, the total, and the
/// kernel's own counters. All `Option` — a group can vanish mid-read, and a
/// gap in the series beats an error that stops the tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pressure {
    /// `memory.current` bytes (children included).
    pub current: Option<u64>,
    /// Effective `memory.max` bytes (`None` = unlimited).
    pub max: Option<u64>,
    /// `pids.current`.
    pub pids_current: Option<u32>,
    /// `memory.events:oom_kill` — members the kernel killed.
    pub oom_kill: Option<u64>,
    /// `memory.events:max` — ceiling hits (throttle/reclaim, not only OOM).
    pub max_events: Option<u64>,
}

impl Pressure {
    /// Usage ratio against the ceiling, when both are known.
    #[must_use]
    pub fn ratio(&self) -> Option<f64> {
        match (self.current, self.max) {
            (Some(current), Some(max)) if max > 0 => Some(current as f64 / max as f64),
            _ => None,
        }
    }
}

/// Graded alert level (T-0041): warn at 80%, critical at 95%, breach at 100%.
/// A level re-arms only after the reading falls below warn − 10% (70%), so a
/// hovering reading emits one row per crossing, never a storm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AlertLevel {
    Warn,
    Critical,
    Breach,
}

impl AlertLevel {
    /// The ratio that fires this level.
    #[must_use]
    pub const fn threshold(self) -> f64 {
        match self {
            Self::Warn => 0.80,
            Self::Critical => 0.95,
            Self::Breach => 1.0,
        }
    }

    /// The ratio below which every level re-arms.
    pub const REARM: f64 = 0.70;

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "warn",
            Self::Critical => "critical",
            Self::Breach => "breach",
        }
    }
}

/// Per-pane alert state: the highest level fired in the current episode.
/// Held by the poller (the daemon's `PaneEntry`), not by the guard — the guard
/// is the mechanism, this is the episode memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AlertState {
    fired: Option<AlertLevel>,
}

impl AlertState {
    /// The highest level fired in the current episode, if any (T-0041): the
    /// attention signal the daemon reports per pane.
    #[must_use]
    pub fn level(&self) -> Option<AlertLevel> {
        self.fired
    }

    /// Check one reading: returns the newly-crossed level, if any.
    ///
    /// Fires on the first tick that crosses each level going up (warn, then
    /// critical, then breach — each at most once per episode) and re-arms all
    /// levels when the reading falls below 70%. A reading with no ratio
    /// (unlimited budget, unreadable group) fires nothing and re-arms nothing.
    pub fn check(&mut self, ratio: Option<f64>) -> Option<AlertLevel> {
        let ratio = ratio?;
        if ratio < AlertLevel::REARM {
            self.fired = None;
            return None;
        }
        for level in [AlertLevel::Warn, AlertLevel::Critical, AlertLevel::Breach] {
            if ratio >= level.threshold() && self.fired.is_none_or(|fired| level > fired) {
                self.fired = Some(level);
                return Some(level);
            }
        }
        None
    }
}
