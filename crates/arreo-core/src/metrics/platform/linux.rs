//! Linux probe: `/proc` parsing + cgroup v2 awareness.
//!
//! - PIDs: full scan of `/proc/<n>/stat` (comm may contain spaces/parens —
//!   parse from the LAST `)`), building a ppid→children map; BFS from root.
//! - RSS: `/proc/<pid>/statm` resident pages × page size (no `ps` fork).
//! - CPU: utime+stime ticks from `/proc/<pid>/stat`; deltas computed by
//!   `Sampler` (needs two samples + wall time).
//! - cgroup: `/proc/<pid>/cgroup` gives `0::/path`; read
//!   `/sys/fs/cgroup/<path>/memory.current` when present (v2). Absent → None
//!   (v1 or container without delegation — not an error).

use super::{Probe, ProbeError, TreeSample};
use std::collections::{HashMap, VecDeque};
use std::io::Read;

pub struct LinuxProbe {
    /// Cached ppid→children map + when it was built. A full /proc scan is
    /// ~15 ms on a busy box; tree membership changes only on fork/exit, so a
    /// 1 s TTL keeps sweeps cheap without missing processes for long.
    cache: std::sync::Mutex<(HashMap<u32, Vec<u32>>, std::time::Instant)>,
}

const CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(1);

impl Default for LinuxProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxProbe {
    #[must_use]
    pub fn new() -> Self {
        Self {
            cache: std::sync::Mutex::new((HashMap::new(), std::time::Instant::now() - CACHE_TTL)),
        }
    }

    fn children_map(&self) -> HashMap<u32, Vec<u32>> {
        if let Ok(guard) = self.cache.lock() {
            if guard.1.elapsed() < CACHE_TTL && !guard.0.is_empty() {
                return guard.0.clone();
            }
        }
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for pid in Self::all_pids() {
            if let Ok((ppid, _)) = Self::parse_stat(pid) {
                children.entry(ppid).or_default().push(pid);
            }
        }
        if let Ok(mut guard) = self.cache.lock() {
            *guard = (children.clone(), std::time::Instant::now());
        }
        children
    }
    fn read_file(path: &str) -> Result<String, ProbeError> {
        let mut file = std::fs::File::open(path).map_err(|_| ProbeError::NotFound(0))?;
        let mut text = String::new();
        file.read_to_string(&mut text).map_err(ProbeError::Io)?;
        Ok(text)
    }

    /// Parse `/proc/<pid>/stat`: returns (ppid, utime+stime ticks).
    /// `comm` is `(anything incl. spaces)` — split after the last `)`.
    fn parse_stat(pid: u32) -> Result<(u32, u64), ProbeError> {
        let text = Self::read_file(&format!("/proc/{pid}/stat"))?;
        let close = text.rfind(')').ok_or_else(|| {
            ProbeError::Parse(format!("/proc/{pid}/stat has no comm close paren"))
        })?;
        let after = &text[close + 1..];
        let fields: Vec<&str> = after.split_whitespace().collect();
        // After comm: state(0) ppid(1) ... utime(11) stime(12) [0-based].
        if fields.len() < 13 {
            return Err(ProbeError::Parse(format!(
                "/proc/{pid}/stat has {} fields, need ≥ 13",
                fields.len()
            )));
        }
        let ppid: u32 = fields[1]
            .parse()
            .map_err(|_| ProbeError::Parse(format!("bad ppid for {pid}")))?;
        let utime: u64 = fields[11]
            .parse()
            .map_err(|_| ProbeError::Parse(format!("bad utime for {pid}")))?;
        let stime: u64 = fields[12]
            .parse()
            .map_err(|_| ProbeError::Parse(format!("bad stime for {pid}")))?;
        Ok((ppid, utime + stime))
    }

    fn rss_bytes(pid: u32) -> u64 {
        let text = match Self::read_file(&format!("/proc/{pid}/statm")) {
            Ok(text) => text,
            Err(_) => return 0,
        };
        let resident: u64 = text
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);
        // SAFETY-free page size: sysconf via libc-free path — parse
        // `getconf PAGESIZE` once would fork; instead use the auxiliary
        // vector constant: on all supported Linux targets the kernel page
        // size for userspace is 4096 (arm64 4K default; 64K kernels are
        // vanishingly rare on our targets and only scale RSS linearly —
        // documented approximation, revisited if T-0008 bench disagrees).
        resident * 4096
    }

    fn all_pids() -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_string_lossy().parse::<u32>().ok())
            .collect()
    }

    fn cgroup_memory_current(pid: u32) -> Option<u64> {
        let text = Self::read_file(&format!("/proc/{pid}/cgroup")).ok()?;
        // v2 line: `0::/user.slice/...`. v1 lines have subsystems — skip.
        let path = text.lines().find_map(|line| {
            let (prefix, rest) = line.split_once(':')?;
            let (_, cpath) = rest.split_once(':')?;
            (prefix == "0").then_some(cpath)
        })?;
        let current =
            std::fs::read_to_string(format!("/sys/fs/cgroup{path}/memory.current")).ok()?;
        current.trim().parse().ok()
    }
}

impl Probe for LinuxProbe {
    fn sample_tree(&self, root: u32) -> Result<TreeSample, ProbeError> {
        // Verify root exists first (clean NotFound, not empty tree).
        Self::parse_stat(root).map_err(|_| ProbeError::NotFound(root))?;
        let children = self.children_map();
        // BFS from root.
        let mut pids = vec![root];
        let mut queue = VecDeque::from([root]);
        while let Some(pid) = queue.pop_front() {
            if let Some(kids) = children.get(&pid) {
                for &kid in kids {
                    if !pids.contains(&kid) {
                        pids.push(kid);
                        queue.push_back(kid);
                    }
                }
            }
        }
        let mut rss_bytes = 0u64;
        let mut total_ticks = 0u64;
        for &pid in &pids {
            rss_bytes += Self::rss_bytes(pid);
            total_ticks += Self::parse_stat(pid).map(|(_, t)| t).unwrap_or(0);
        }
        let cgroup_bytes = Self::cgroup_memory_current(root);
        Ok(TreeSample {
            pids,
            rss_bytes,
            cgroup_bytes,
            total_ticks,
        })
    }

    fn clock_ticks_per_sec(&self) -> u64 {
        100 // _SC_CLK_TCK on Linux (all supported targets)
    }
}
