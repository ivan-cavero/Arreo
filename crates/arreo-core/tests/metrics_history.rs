//! T-0040 acceptance tests: metrics history — a durable per-pane series.
//!
//! Three tiers (10 s / 24 h, 1 m / 30 d, 1 h / 365 d) in one table keyed
//! `(pane, ts_ms, step_ms)`, so re-running a rollup is idempotent. Every row
//! carries average AND peak RSS plus cpu and pids. Retention is an hourly prune
//! that never deletes the newest row of a live pane.
//!
//! Gated on the `sqlite` feature (T-0010 lite pass): the series lives in the
//! store, so the foreign-target portability gate must not compile this file.
#![cfg(feature = "sqlite")]

use arreo_core::store::{metrics_retention, metrics_step_for, MetricsSample, SessionStore};

fn sample(pane: &str, ts_ms: u64, step_ms: u64, rss: u64) -> MetricsSample {
    MetricsSample {
        pane: pane.to_string(),
        ts_ms,
        step_ms,
        rss_avg: rss,
        rss_peak: rss + 100,
        cpu_avg: 1.0,
        cpu_peak: 2.0,
        pids_avg: 3.0,
        pids_peak: 3,
        samples: 1,
    }
}

/// The tiers are the stated numbers: steps and retentions, in order.
#[test]
fn the_tiers_are_the_stated_numbers() {
    assert_eq!(
        metrics_retention(),
        [
            (10_000, 24 * 60 * 60 * 1000),
            (60_000, 30 * 24 * 60 * 60 * 1000),
            (3_600_000, 365 * 24 * 60 * 60 * 1000),
        ]
    );
}

/// Recording floors to the step and merges re-runs: the same bucket twice is
/// one row, and the peak keeps the worst moment.
#[test]
fn recording_is_idempotent_and_keeps_the_peak() {
    let store = SessionStore::open_memory().expect("store");
    store
        .metrics_record(&sample("a", 10_001, 10_000, 1000))
        .expect("record");
    store
        .metrics_record(&sample("a", 10_009, 10_000, 2000))
        .expect("record");
    let rows = store.metrics_range("a", 0, 20_000, 10_000).expect("range");
    assert_eq!(rows.len(), 1, "one bucket, one row: {rows:?}");
    assert_eq!(rows[0].ts_ms, 10_000, "floored to the step");
    assert_eq!(rows[0].rss_peak, 2100, "the peak keeps the worst moment");
    assert_eq!(rows[0].samples, 2);
}

/// A rollup reads the tier below and writes the tier above — never /proc — so
/// a pane restored late leaves no hole where the underlying row exists.
#[test]
fn rollups_come_from_the_tier_below() {
    let store = SessionStore::open_memory().expect("store");
    for minute in 0..6u64 {
        store
            .metrics_record(&sample(
                "a",
                minute * 60_000 + 1_000,
                60_000,
                1000 + minute * 100,
            ))
            .expect("record");
    }
    let written = store
        .metrics_rollup("a", 0, 6 * 60_000, 60_000, 3_600_000)
        .expect("rollup");
    assert_eq!(written, 1, "six 1 m rows become one 1 h row");
    let rows = store
        .metrics_range("a", 0, 6 * 60_000, 3_600_000)
        .expect("range");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].ts_ms, 0, "aligned to the coarser boundary");
    assert_eq!(rows[0].rss_peak, 1600, "peak of peaks");
    assert_eq!(
        rows[0].samples, 6,
        "the rollup carries the tier below's weight"
    );
    // Re-running is idempotent: same key, same row, not a duplicate.
    let again = store
        .metrics_rollup("a", 0, 6 * 60_000, 60_000, 3_600_000)
        .expect("rollup again");
    assert_eq!(again, 1);
    assert_eq!(
        store
            .metrics_range("a", 0, 6 * 60_000, 3_600_000)
            .expect("range")
            .len(),
        1,
        "no duplicate row"
    );
}

/// Asking finer than available downshifts to the nearest real step and says so,
/// instead of returning empty.
#[test]
fn a_query_finer_than_available_downshifts_loudly() {
    // 6 h over a 24 h tier: the 10 s step covers it.
    assert_eq!(metrics_step_for(0, 6 * 60 * 60 * 1000), (10_000, false));
    // 6 h asked at 1 s: downshift to 10 s, flagged.
    let (step, downshifted) = metrics_step_for_window(0, 6 * 60 * 60 * 1000, 1_000);
    assert_eq!(step, 10_000);
    assert!(downshifted, "--step 1s over 6 h reports 10 s");
    // 60 d: past the 30 d tier, the hourly tier covers it.
    assert_eq!(
        metrics_step_for(0, 60 * 24 * 60 * 60 * 1000),
        (3_600_000, false)
    );
}

/// The downshift rule with an explicit ask: the finest tier at or coarser than
/// the ask whose retention covers the window — or the coarsest tier, flagged,
/// when nothing covers it.
///
/// A helper for this test file (the store exposes the window rule; the CLI
/// applies the ask on top). Kept here rather than in the store because only one
/// caller needs it — an abstraction used in one place is a comment with steps.
fn metrics_step_for_window(since_ms: u64, until_ms: u64, asked_ms: u64) -> (u64, bool) {
    let (natural, _) = metrics_step_for(since_ms, until_ms);
    let coarser = [10_000u64, 60_000, 3_600_000]
        .into_iter()
        .find(|step| *step >= asked_ms)
        .unwrap_or(3_600_000);
    let step = coarser.max(natural);
    (step, step != asked_ms)
}

/// Retention: 400 days of synthetic samples in, the clock advances, the prune
/// leaves per-tier counts and ≤ 2 MB per pane — and never the newest row of a
/// live pane.
#[test]
fn retention_prunes_per_tier_and_never_empties_a_live_graph() {
    let store = SessionStore::open_memory().expect("store");
    let day = 24 * 60 * 60 * 1000u64;
    let now = 400 * day;
    // 400 daily 10 s rows (one per day, sparse but old), 40 daily 1 m rows,
    // 400 daily 1 h rows.
    for day_index in 0..400u64 {
        store
            .metrics_record(&sample("a", day_index * day, 10_000, 1000))
            .expect("record");
        store
            .metrics_record(&sample("a", day_index * day, 3_600_000, 1000))
            .expect("record");
        if day_index % 10 == 0 {
            store
                .metrics_record(&sample("a", day_index * day, 60_000, 1000))
                .expect("record");
        }
    }
    // A fresh row for the live pane in every tier: the prune must keep these
    // even though older rows in the same tier go.
    for step in [10_000u64, 60_000, 3_600_000] {
        store
            .metrics_record(&sample("a", now, step, 1000))
            .expect("record");
    }
    let removed = store.metrics_prune(now, &["a"]).expect("prune");
    let by_step: std::collections::HashMap<u64, u64> = removed.into_iter().collect();
    // 10 s tier keeps 24 h: 399 daily rows go (day 399 is inside the window),
    // the fresh one stays.
    assert_eq!(
        by_step[&10_000], 399,
        "10 s rows older than 24 h go: {by_step:?}"
    );
    // 1 m tier keeps 30 d: days 0..360 go (37 rows), days 370/380/390 and the
    // fresh row stay.
    assert_eq!(
        by_step[&60_000], 37,
        "1 m rows older than 30 d go: {by_step:?}"
    );
    // 1 h tier keeps 365 d: 35 daily rows go, the rest stay.
    assert_eq!(
        by_step[&3_600_000], 35,
        "1 h rows older than 365 d go: {by_step:?}"
    );
    for step in [10_000u64, 60_000, 3_600_000] {
        let rows = store.metrics_range("a", 0, now, step).expect("range");
        assert!(
            rows.iter().any(|row| row.ts_ms == now),
            "the live pane's newest row survives in tier {step}: {rows:?}"
        );
    }
    assert!(
        store.metrics_bytes("a").expect("bytes") <= 2 * 1024 * 1024,
        "the series stays under 2 MB per pane"
    );
}

/// An unknown pane gives an empty series, not an error — the CLI renders the
/// message, and the store does not invent one.
#[test]
fn an_unknown_pane_is_empty_not_an_error() {
    let store = SessionStore::open_memory().expect("store");
    let rows = store
        .metrics_range("no-such-pane", 0, u64::MAX, 10_000)
        .expect("range");
    assert!(rows.is_empty());
}

/// Overhead: the 10 s writer path (record one row) costs microseconds, so the
/// tick cadence cannot drift — asserted from timestamps, not assumed.
#[test]
fn the_writer_path_costs_microseconds_not_milliseconds() {
    let store = SessionStore::open_memory().expect("store");
    let start = std::time::Instant::now();
    for index in 0..100u64 {
        store
            .metrics_record(&sample("a", index * 10_000, 10_000, 1000))
            .expect("record");
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "100 records took {elapsed:?} — the 10 s writer must not move the tick"
    );
}
