//! T-0006 failing-first probes: /proc sampling, tree rollup, SQLite store.
//!
//! Written before `src/metrics/` exists — MUST fail to compile until it lands.

use arreo_core::metrics::Sampler;
#[cfg(feature = "sqlite")]
use arreo_core::metrics::Store;

#[test]
fn sample_self_process_tree() {
    let mut sampler = Sampler::new();
    let me = std::process::id();
    let sample = sampler.sample_tree(me).expect("sample own tree");
    assert!(sample.rss_bytes > 0, "RSS observed: {sample:?}");
    assert!(!sample.pids.is_empty(), "at least self in tree");
    assert!(sample.pids.contains(&me));
}

#[test]
fn cpu_percent_needs_two_samples() {
    let mut sampler = Sampler::new();
    let me = std::process::id();
    let first = sampler.sample_tree(me).expect("first sample");
    assert_eq!(first.cpu_percent, None, "no delta yet");
    // Burn some CPU, then resample: delta must be computable.
    let start = std::time::Instant::now();
    let mut x = 0u64;
    while start.elapsed() < std::time::Duration::from_millis(200) {
        x = x.wrapping_add(1).wrapping_mul(3);
    }
    std::hint::black_box(x);
    std::thread::sleep(std::time::Duration::from_millis(100));
    let second = sampler.sample_tree(me).expect("second sample");
    assert!(second.cpu_percent.is_some(), "delta computable: {second:?}");
}

#[test]
fn dead_pid_is_an_error_not_a_panic() {
    let mut sampler = Sampler::new();
    // PID 2^22 never exists on Linux (default pid_max 4194304, and even
    // then this PID is almost surely dead right now — retry once).
    let result = sampler.sample_tree(4194303);
    assert!(result.is_err(), "dead PID errors cleanly");
}

#[test]
#[cfg(feature = "sqlite")]
fn rollups_persist_and_prune() {
    let store = Store::open_memory().expect("in-memory store");
    store
        .insert_rollup("pane-test", 1_700_000_000_000, 123_456, 12.5, 3)
        .expect("insert");
    store
        .insert_rollup("pane-test", 1_700_000_010_000, 200_000, 15.0, 3)
        .expect("insert");
    let rows = store.recent("pane-test", 10).expect("recent");
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].rss_bytes, 123_456);
    assert!(rows[1].ts_ms > rows[0].ts_ms, "ordered by time");
    // Prune everything older than the second row: 1 row survives.
    let pruned = store.prune_older_than(1_700_000_010_000).expect("prune");
    assert_eq!(pruned, 1);
    assert_eq!(store.recent("pane-test", 10).unwrap().len(), 1);
}

#[test]
fn sampler_overhead_within_budget() {
    // 30 panes × sample_tree on live PIDs, timed: per-pane cost must leave
    // the 1 s cadence at ≤ 1% CPU (i.e. ≤ 10 ms per full 30-pane sweep).
    let mut sampler = Sampler::new();
    let me = std::process::id();
    let start = std::time::Instant::now();
    for _ in 0..30 {
        let _ = sampler.sample_tree(me);
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 300,
        "30-pane sweep took {elapsed:?} — must be < 300 ms for 1% @1 s headroom"
    );
}

#[test]
fn newborn_children_appear_after_cache_ttl() {
    // Membership cache (1 s TTL) must not hide forks forever: spawn a child,
    // wait out the TTL, and confirm it joins the tree.
    let mut sampler = Sampler::new();
    let me = std::process::id();
    let before = sampler.sample_tree(me).expect("baseline").pids.len();
    let mut child = std::process::Command::new("/bin/sleep")
        .arg("5")
        .spawn()
        .expect("spawn sleep");
    let child_pid = child.id();
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let after = sampler.sample_tree(me).expect("rescan");
    assert!(
        after.pids.contains(&child_pid),
        "newborn {child_pid} visible after TTL (before={before}, after={})",
        after.pids.len()
    );
    child.kill().ok();
    child.wait().ok();
}

#[test]
fn backoff_goes_slow_after_eight_idle_ticks() {
    use arreo_core::metrics::sampler::Cadence;
    let mut sampler = Sampler::new();
    let me = std::process::id();
    for _ in 0..7 {
        assert_eq!(sampler.cadence(me, false), Cadence::Fast);
    }
    assert_eq!(sampler.cadence(me, false), Cadence::Slow);
    assert_eq!(sampler.cadence(me, true), Cadence::Fast, "output resets");
}
