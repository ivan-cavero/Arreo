# ADR 0004: metrics — direct `/proc` + `rusqlite` bundled, cached membership

- Status: accepted (2026-09-10, T-0006)
- Context: Pillar P3 needs per-agent RSS/CPU with ≤ 1% sampler overhead at
  30 panes, cgroup v2 awareness, 10 s SQLite rollups (ROADMAP §3.1, §5).
- Decision: hand-parsed `/proc/<pid>/stat|statm` (last-`)` comm handling) +
  ppid→children BFS with a 1 s TTL membership cache; CPU% from tick deltas
  (`top` convention: % of one core); cgroup `memory.current` read when the
  v2 path exists, else `None`; `rusqlite` 0.31 with `bundled` feature; WAL;
  versioned schema (`meta.schema_version`) from day one.
- Why this one:
  - Direct `/proc` (not `sysinfo`/`procfs` crates): zero new runtime deps
    beyond rusqlite, page-size/RSS semantics explicit and tested, cgroup
    awareness is hand work regardless — a crate would hide, not remove it.
  - `bundled` SQLite: this box has the shared lib but no headers; bundled
    builds everywhere identically (CI windows/macOS included) — one less
    system dependency for contributors.
  - 1 s TTL cache: full `/proc` scan is ~15 ms on a busy box; uncached, a
    30-pane sweep cost 467 ms (test caught it); cached, well under budget.
    Newborns appear within 1 s (tested) — fine for 1 s cadence sampling.
- Alternatives rejected:
  - `sysinfo` crate: coarser API, still no cgroup-v2-first story, extra dep
    for work we wrote in ~150 lines (rejected: dependency philosophy).
  - Forking `ps`: process spawn per sample per pane — overhead obscenity at
    30 panes (rejected: performance).
  - Uncached full scan: measured 467 ms/30-pane sweep, blows the 1% budget
    headroom (rejected: measured, not theorized).
- Consequences: page size assumed 4096 (documented in code; 64K-page ARM
  kernels scale RSS linearly — revisit if T-0008 bench disagrees);
  Windows/macOS probes are honest `Unimplemented` stubs owned by T-0019.
