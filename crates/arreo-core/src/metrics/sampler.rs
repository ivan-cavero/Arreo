//! `Sampler`: tick-based CPU% over the probe + idle backoff state.
//!
//! CPU% = Δticks / (Δwall × ticks_per_sec) × 100 × num_cpus-normalized? No —
//! raw percent of ONE core ×100 scale would confuse; we report percent of
//! one core (200% = 2 cores busy), the `top` convention. Documented here so
//! the TUI (T-0015) doesn't reinterpret it.

#[cfg(target_os = "linux")]
use super::platform::LinuxProbe;
#[cfg(not(target_os = "linux"))]
use super::platform::OtherProbe;
use super::platform::{Probe, ProbeError};
use std::collections::HashMap;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct ProcessSample {
    pub pids: Vec<u32>,
    pub rss_bytes: u64,
    pub cgroup_bytes: Option<u64>,
    /// None on the first sample for a root (no delta yet).
    pub cpu_percent: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    /// Normal 1 s sampling.
    Fast,
    /// Backed off (idle pane): caller may sleep longer between samples.
    Slow,
}

pub struct Sampler {
    probe: Box<dyn Probe>,
    last: HashMap<u32, (u64, Instant)>,
    idle_ticks: HashMap<u32, u32>,
}

impl Sampler {
    #[must_use]
    pub fn new() -> Self {
        #[cfg(target_os = "linux")]
        let probe: Box<dyn Probe> = Box::new(LinuxProbe::new());
        #[cfg(not(target_os = "linux"))]
        let probe: Box<dyn Probe> = Box::new(OtherProbe);
        Self {
            probe,
            last: HashMap::new(),
            idle_ticks: HashMap::new(),
        }
    }

    /// Sample the tree rooted at `root`. First call per root yields
    /// `cpu_percent: None`; later calls compute the delta.
    pub fn sample_tree(&mut self, root: u32) -> Result<ProcessSample, ProbeError> {
        // `&mut self` borrows conflict with probe use — read probe fields first.
        let ticks_per_sec = self.probe.clock_ticks_per_sec();
        let tree = self.probe.sample_tree(root)?;
        let now = Instant::now();
        let cpu_percent = match self.last.insert(root, (tree.total_ticks, now)) {
            None => None,
            Some((prev_ticks, prev_time)) => {
                let dt = now.duration_since(prev_time).as_secs_f64();
                if dt <= 0.0 {
                    None
                } else {
                    let dticks = tree.total_ticks.saturating_sub(prev_ticks) as f64;
                    Some(dticks / (dt * ticks_per_sec as f64) * 100.0)
                }
            }
        };
        Ok(ProcessSample {
            pids: tree.pids,
            rss_bytes: tree.rss_bytes,
            cgroup_bytes: tree.cgroup_bytes,
            cpu_percent,
        })
    }

    /// Idle backoff: panes with no output for N consecutive ticks sample
    /// slower (exponential: 1 s → 2 s → 4 s → 8 s cap). The daemon calls this
    /// with whether the pane produced output since the last tick.
    pub fn cadence(&mut self, root: u32, had_output: bool) -> Cadence {
        if had_output {
            self.idle_ticks.remove(&root);
            return Cadence::Fast;
        }
        let ticks = self.idle_ticks.entry(root).or_insert(0);
        *ticks += 1;
        if *ticks >= 8 {
            Cadence::Slow
        } else {
            Cadence::Fast
        }
    }
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}
