//! Linux guard: child cgroup under our own scope + limit files + events.
//!
//! Placement: `<own-scope>/arreo-<name>-<pid>/`. Our scope is writable by us
//! (verified on this box: mkdir + cgroup.procs succeed), so v0 needs no
//! root and no systemd-run. `Drop` removes the group (members must be gone —
//! the daemon kills panes before dropping guards; `rmdir` failure is logged,
//! never fatal).
//!
//! Limits: `memory.max` (bytes or `max`), `pids.max` (number or `max`).
//! Breach: `memory.events` (`max N` counter) and `pids.events` — kernel
//! truth, read on poll. Note `memory.events:max` fires when the ceiling is
//! HIT (throttle/reclaim), not only on OOM-kill — exactly the "tell me
//! before it dies" signal the criterion wants.

use super::{Breach, Budget, EnforceError, Pressure};
use std::path::PathBuf;

pub struct Guard {
    path: PathBuf,
}

impl Guard {
    /// Our cgroup scope, from `/proc/self/cgroup` (`0::/path` v2 line).
    fn own_scope() -> Result<PathBuf, EnforceError> {
        let text = std::fs::read_to_string("/proc/self/cgroup")
            .map_err(|e| EnforceError::Unavailable(format!("no /proc/self/cgroup: {e}")))?;
        let path = text
            .lines()
            .find_map(|line| {
                let (prefix, rest) = line.split_once(':')?;
                let (_, cpath) = rest.split_once(':')?;
                (prefix == "0").then_some(cpath)
            })
            .ok_or_else(|| EnforceError::Unavailable("no cgroup v2 scope".to_string()))?;
        Ok(PathBuf::from(format!("/sys/fs/cgroup{path}")))
    }

    /// Create `arreo-<name>-<pid>` under our scope with the budget applied.
    pub fn create(name: &str, budget: Budget) -> Result<Self, EnforceError> {
        let scope = Self::own_scope()?;
        let path = scope.join(format!("arreo-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&path)
            .map_err(|e| EnforceError::Unavailable(format!("mkdir {}: {e}", path.display())))?;
        let guard = Self { path };
        // Enable controllers on the group (harmless if already enabled).
        guard.write(
            "memory.max",
            &budget
                .memory_max
                .map(|b| b.to_string())
                .unwrap_or_else(|| "max".to_string()),
        )?;
        guard.write(
            "pids.max",
            &budget
                .pids_max
                .map(|p| p.to_string())
                .unwrap_or_else(|| "max".to_string()),
        )?;
        Ok(guard)
    }

    /// Adopt an existing group by path (T-0038 stage 2: the daemon handoff).
    ///
    /// The incoming daemon inherits panes whose cgroups already exist — a cgroup
    /// is a *named* kernel object, so it crosses a handoff as a path rather than
    /// as a descriptor, and this is how the new daemon takes ownership of one.
    ///
    /// ## Call this only once you own the pane
    ///
    /// [`Drop`] removes the group, so adopting one **before** the handoff commits
    /// is a way to destroy a running agent's enforcement: the outgoing daemon is
    /// still serving that pane, and an aborted handoff that dropped this value
    /// would `rmdir` the live pane's group out from under it, silently removing
    /// the ceiling it was running under. The incoming daemon therefore re-opens
    /// guards **after** it has committed to serving, where taking ownership (and
    /// the removal that comes with it) is exactly right.
    ///
    /// Nothing removes the group on the *outgoing* side: the daemon exits via
    /// `std::process::exit`, which runs no destructors, so the group outlives it
    /// by construction and the adopting daemon becomes its owner.
    ///
    /// Refuses a path that is not an existing directory: a pane that arrived with
    /// a guard path that cannot be re-opened must be reported, never served
    /// unprotected while the handoff claims success.
    pub fn reopen(path: PathBuf) -> Result<Self, EnforceError> {
        match std::fs::metadata(&path) {
            Ok(meta) if meta.is_dir() => Ok(Self { path }),
            Ok(_) => Err(EnforceError::Unavailable(format!(
                "{} is not a cgroup directory",
                path.display()
            ))),
            Err(e) => Err(EnforceError::Unavailable(format!(
                "cannot re-open cgroup {}: {e}",
                path.display()
            ))),
        }
    }

    fn write(&self, file: &str, value: &str) -> Result<(), EnforceError> {
        std::fs::write(self.path.join(file), value).map_err(|e| {
            EnforceError::Unavailable(format!("{}: {e}", self.path.join(file).display()))
        })
    }

    fn read(&self, file: &str) -> Result<String, EnforceError> {
        std::fs::read_to_string(self.path.join(file)).map_err(|e| {
            EnforceError::Unavailable(format!("{}: {e}", self.path.join(file).display()))
        })
    }

    /// Move `pid` (and its future children) into this group.
    pub fn attach(&self, pid: u32) -> Result<(), EnforceError> {
        self.write("cgroup.procs", &pid.to_string())
    }

    /// Group path (for Drop checks + debugging).
    #[must_use]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// Effective `memory.max` in bytes (`None` = unlimited).
    pub fn memory_max(&self) -> Result<Option<u64>, EnforceError> {
        Ok(self.read("memory.max")?.trim().parse().ok())
    }

    /// Effective `pids.max` (`None` = unlimited).
    pub fn pids_max(&self) -> Result<Option<u32>, EnforceError> {
        Ok(self.read("pids.max")?.trim().parse().ok())
    }

    /// Current `memory.current` bytes (None when unreadable).
    #[must_use]
    pub fn memory_current(&self) -> Option<u64> {
        self.read("memory.current").ok()?.trim().parse().ok()
    }

    /// Cgroup pressure snapshot: the ceiling, the total (children included),
    /// and the kernel's OOM counters (T-0041).
    ///
    /// `memory.current` includes descendants (that is what makes it the
    /// kernel's own answer where a process-tree RSS misses grandchildren that
    /// moved trees); `memory.events` carries `max` (ceiling hits) and
    /// `oom_kill` (members the kernel killed). `pids.current` rides along so
    /// one graph shows all three lines. Every field is `Option` because a
    /// group can vanish mid-read (member reaped, guard dropped) — a missing
    /// counter is a gap in the series, never an error.
    #[must_use]
    pub fn pressure(&self) -> Pressure {
        let max = self.memory_max().ok().flatten();
        Pressure {
            current: self.memory_current(),
            max,
            pids_current: self
                .read("pids.current")
                .ok()
                .and_then(|text| text.trim().parse().ok()),
            oom_kill: self
                .read("memory.events")
                .ok()
                .and_then(|text| parse_events_counter(&text, "oom_kill")),
            max_events: self
                .read("memory.events")
                .ok()
                .and_then(|text| parse_events_counter(&text, "max")),
        }
    }

    /// Live member count (from `cgroup.procs` lines).
    pub fn member_count(&self) -> Result<usize, EnforceError> {
        Ok(self
            .read("cgroup.procs")?
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count())
    }

    /// Kernel-counted breach, if any. Checks memory first (the headline
    /// budget), then pids. `memory.events` `max` counter > 0 = ceiling hit.
    pub fn breached(&self) -> Result<Option<Breach>, EnforceError> {
        let memory = self.read("memory.events").ok();
        let pids = self.read("pids.events").ok();
        Ok(parse_breach(memory.as_deref(), pids.as_deref()))
    }
}

/// Parse one `name value` counter out of a `memory.events`-shaped text.
/// `None` when the line is absent or unparsable — a missing counter is a gap,
/// never a zero that would claim "no OOMs" about a file never read.
fn parse_events_counter(text: &str, name: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(' ')?;
        (key == name).then(|| value.trim().parse::<u64>().ok())?
    })
}

/// Pure breach parser (unit-tested everywhere, no cgroupfs needed):
/// `max N` counter > 0 in either events file = ceiling hit.
fn parse_breach(memory_events: Option<&str>, pids_events: Option<&str>) -> Option<Breach> {
    for (events, breach) in [(memory_events, Breach::Memory), (pids_events, Breach::Pids)] {
        if let Some(events) = events {
            for line in events.lines() {
                if let Some(count) = line.strip_prefix("max ") {
                    if count.trim().parse::<u64>().unwrap_or(0) > 0 {
                        return Some(breach);
                    }
                }
            }
        }
    }
    None
}

impl Drop for Guard {
    fn drop(&mut self) {
        // Members block rmdir (EBUSY) — the daemon kills panes first; a
        // lingering member just leaves the group behind (logged, retried
        // never — next create uses a fresh pid-suffixed name).
        if let Err(e) = std::fs::remove_dir(&self.path) {
            eprintln!(
                "enforce: rmdir {} failed (members linger?): {e}",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_breach;
    use crate::enforce::Breach;

    #[test]
    fn breach_parser_reads_kernel_counters() {
        assert_eq!(
            parse_breach(Some("low 0\nhigh 0\nmax 0\noom 0\noom_kill 0\n"), None),
            None
        );
        assert_eq!(
            parse_breach(Some("low 5\nhigh 3\nmax 1\noom 0\noom_kill 0\n"), None),
            Some(Breach::Memory)
        );
        assert_eq!(parse_breach(None, Some("max 0\n")), None);
        assert_eq!(
            parse_breach(None, Some("current 7\nmax 2\n")),
            Some(Breach::Pids)
        );
        // Memory wins ties (headline budget first).
        assert_eq!(
            parse_breach(Some("max 1\n"), Some("max 1\n")),
            Some(Breach::Memory)
        );
    }
}
