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

#[derive(Debug)]
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

    /// The one rule for what a guard path may be — the single definition of "a
    /// valid guard path", shared by [`Guard::create`] (which builds under our
    /// scope) and [`Guard::reopen`] (which adopts a peer-supplied one). One
    /// rule, one home (F2 of the stage-2 review).
    ///
    /// A guard path must be:
    ///
    /// - **a direct child of this machine's cgroup scope** — the same
    ///   [`own_scope`] `create` builds under. A hostile outgoing daemon used to
    ///   name any empty directory it liked in the manifest, and `Guard::drop`'s
    ///   `rmdir` removed it when the pane died — worse, the pane reported
    ///   `has_guard() == true` while `breached()`/`pressure()` read nothing, so
    ///   agents served without their ceiling, silently. A path that is not
    ///   directly under the scope is either someone else's cgroup or not a
    ///   cgroup at all, and neither is adoptable.
    /// - **a directory that reads like a cgroup** — at least one of
    ///   `memory.max`/`pids.max` must be readable, because a guard whose budget
    ///   files cannot be read enforces nothing.
    ///
    /// Refuses naming the path and the scope.
    fn valid_guard_path(path: &std::path::Path) -> Result<(), EnforceError> {
        let scope = Self::own_scope()?;
        if path.parent() != Some(scope.as_path()) {
            return Err(EnforceError::Unavailable(format!(
                "{} is not a direct child of this daemon's cgroup scope {}",
                path.display(),
                scope.display()
            )));
        }
        let memory_readable = std::fs::read_to_string(path.join("memory.max")).is_ok();
        let pids_readable = std::fs::read_to_string(path.join("pids.max")).is_ok();
        if !memory_readable && !pids_readable {
            return Err(EnforceError::Unavailable(format!(
                "{} does not read like a cgroup (neither memory.max nor pids.max is readable)",
                path.display()
            )));
        }
        Ok(())
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
        // The same rule `reopen` will apply to this very path after a handoff:
        // a guard `create` just built must be a path `reopen` accepts, or the
        // next daemon would refuse the enforcement this one attached (F2's
        // shared rule, checked on both sides of the cut).
        Self::valid_guard_path(&guard.path)?;
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
    /// Refuses a path that is not a valid guard path ([`Guard::valid_guard_path`]
    /// — a direct child of this machine's cgroup scope that reads like a cgroup),
    /// or not an existing directory. A pane that arrived with a guard path that
    /// cannot be re-opened must be reported, never served unprotected while the
    /// handoff claims success.
    pub fn reopen(path: PathBuf) -> Result<Self, EnforceError> {
        Self::valid_guard_path(&path)?;
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

    /// The validate-only half of [`Guard::reopen`]: is `path` a path `reopen`
    /// would adopt?
    ///
    /// The incoming daemon calls this **before the cut**. Adopting — and the
    /// `Drop`-time `rmdir` that comes with a `Guard` — before the handoff
    /// commits could remove the group of a pane the outgoing daemon still
    /// serves if the handoff then aborted, so the pre-commit decision has to be
    /// a question, never an adoption. [`Guard::reopen`] runs the same rule
    /// again, which is what makes the two answers agree.
    pub fn validate(path: &std::path::Path) -> Result<(), EnforceError> {
        Self::valid_guard_path(path)
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

    /// The scope this box runs in, for tests that need a real child of it.
    fn scope_or_skip() -> Option<std::path::PathBuf> {
        let scope = super::Guard::own_scope().ok()?;
        if !std::fs::metadata(&scope)
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            return None;
        }
        Some(scope)
    }

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

    /// F2 (stage-2 review, reproduced): `reopen` refuses a path that is not a
    /// **direct child of this machine's cgroup scope**, naming the path and
    /// the scope. The old `reopen` accepted any directory, and `Drop`'s
    /// `rmdir` removed it when the pane died — a hostile outgoing daemon could
    /// name an arbitrary empty directory and the pane would report itself
    /// guarded while enforcement read nothing.
    ///
    /// What removal turns red: dropping the scope check — an empty `/tmp`
    /// directory is adopted, and this `expect_err` sees `Ok`.
    #[test]
    fn reopen_refuses_a_path_outside_the_cgroup_scope() {
        // Skipped when the scope itself cannot be named: then no guard can
        // exist anywhere and the rule's answer everywhere is the same refusal.
        let Some(scope) = scope_or_skip() else {
            eprintln!("no cgroup v2 scope on this box: skipping");
            return;
        };
        let foreign = std::env::temp_dir().join(format!(
            "arreo-guard-foreign-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&foreign);
        std::fs::create_dir_all(&foreign).expect("an arbitrary empty directory");
        let err = super::Guard::reopen(foreign.clone())
            .expect_err("a directory that is not under the scope is not a guard");
        let text = err.to_string();
        assert!(
            text.contains(&foreign.display().to_string()),
            "the refusal names the path: {text}"
        );
        assert!(
            text.contains(&scope.display().to_string()),
            "and the scope it must live under: {text}"
        );
        let _ = std::fs::remove_dir_all(&foreign);
    }

    /// F2: a **direct child of the scope** that does not read like a cgroup is
    /// refused too — at least one of `memory.max`/`pids.max` must be readable,
    /// or the guard enforces nothing while the pane claims it does.
    #[test]
    fn reopen_refuses_a_child_of_the_scope_that_does_not_read_like_a_cgroup() {
        let Some(scope) = scope_or_skip() else {
            eprintln!("no cgroup v2 scope on this box: skipping");
            return;
        };
        let dir = scope.join(format!(
            "arreo-guard-not-cgroup-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        if let Err(e) = std::fs::create_dir(&dir) {
            eprintln!(
                "the scope is not writable ({}): skipping ({e})",
                scope.display()
            );
            return;
        }
        // No memory.max/pids.max: not a cgroup, by the rule's own test.
        let err = super::Guard::reopen(dir.clone())
            .expect_err("a directory with no budget files does not read like a cgroup");
        assert!(
            err.to_string().contains("does not read like a cgroup"),
            "the refusal names the missing evidence: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F2: `reopen` on a real child of the scope that reads like a cgroup
    /// succeeds — the success half of the rule, and the one `Guard::create`
    /// itself must agree with.
    ///
    /// The child is `Guard::create`'s own: a real cgroup directly under the scope
    /// whose `memory.max`/`pids.max` the kernel made readable. That requires
    /// cgroup delegation (the harness scope here exposes controllers but denies
    /// writes, so `create` fails and the test skips, exactly like
    /// `cgroup_v2_live()` in `tests/enforce.rs` — skip, never fake).
    ///
    /// What removal turns red: `reopen` going back to a bare is-directory check —
    /// the `/tmp` directory from the refusal test would be adopted.
    #[test]
    fn reopen_accepts_a_real_child_of_the_scope_that_reads_like_a_cgroup() {
        let made = match super::Guard::create("f2-reopen", crate::enforce::Budget::unlimited()) {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("SKIP: no cgroup v2 delegation ({e})");
                return;
            }
        };
        let path = made.path().to_path_buf();
        let reopened = super::Guard::reopen(path.clone())
            .expect("a direct child of the scope that reads like a cgroup is adopted");
        assert_eq!(reopened.path(), path.as_path());
        // Both handles own the same group; the second rmdir finds nothing and
        // merely logs (like any double-drop of one cgroup).
        drop(reopened);
        drop(made);
    }

    /// `create` and `reopen` agree about what a valid guard path is (F2's
    /// shared rule): a guard `create` builds is directly under the scope and
    /// reads like a cgroup, so `validate` — the pre-commit question the
    /// incoming daemon asks — accepts what `create` built. Skipped when the
    /// scope is not writable (then `create` fails on this box and there is
    /// nothing to hand over, which is exactly what the runtime reports).
    #[test]
    fn create_and_validate_agree_on_a_valid_guard_path() {
        let guard = match super::Guard::create("f2-agree", crate::enforce::Budget::unlimited()) {
            Ok(guard) => guard,
            Err(e) => {
                eprintln!("no writable delegated scope ({e}): skipping");
                return;
            }
        };
        let path = guard.path().to_path_buf();
        assert!(
            super::Guard::validate(&path).is_ok(),
            "create built a path its own reopen rule accepts"
        );
        // `create`'s dir has real kernel controller files (memory.max/pids.max
        // were written into it), so Drop's rmdir may need the members gone —
        // there are none here.
        drop(guard);
    }
}
