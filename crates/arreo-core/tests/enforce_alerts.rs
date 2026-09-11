//! T-0041 acceptance tests: graded cgroup alerts with hysteresis.
//!
//! Pure-state tests (no cgroupfs needed): the thresholds, the re-arm rule, and
//! the episode memory. The kernel counters they read are parsed by
//! `parse_events_counter`-shaped helpers; the live path is proven by the
//! enforcement slice (T-0019's 4 GB scenario extended).

use arreo_core::enforce::{AlertLevel, AlertState, Pressure};

/// Thresholds are the stated numbers: 80% warn, 95% critical, 100% breach.
#[test]
fn thresholds_are_the_stated_numbers() {
    assert_eq!(AlertLevel::Warn.threshold(), 0.80);
    assert_eq!(AlertLevel::Critical.threshold(), 0.95);
    assert_eq!(AlertLevel::Breach.threshold(), 1.0);
    assert_eq!(AlertLevel::REARM, 0.70);
    assert!(AlertLevel::Warn < AlertLevel::Critical);
    assert!(AlertLevel::Critical < AlertLevel::Breach);
}

/// Each level fires on the first tick that crosses it — once per episode, in
/// order, no storm from a hovering reading.
#[test]
fn levels_fire_once_each_going_up() {
    let mut state = AlertState::default();
    assert_eq!(state.check(Some(0.10)), None);
    assert_eq!(state.check(Some(0.79)), None, "below warn: nothing");
    assert_eq!(
        state.check(Some(0.80)),
        Some(AlertLevel::Warn),
        "warn fires"
    );
    assert_eq!(state.check(Some(0.81)), None, "warn does not refire");
    assert_eq!(state.check(Some(0.90)), None, "between levels: nothing new");
    assert_eq!(
        state.check(Some(0.95)),
        Some(AlertLevel::Critical),
        "critical fires"
    );
    assert_eq!(state.check(Some(0.96)), None, "critical does not refire");
    assert_eq!(
        state.check(Some(1.00)),
        Some(AlertLevel::Breach),
        "breach fires"
    );
    assert_eq!(state.check(Some(1.50)), None, "breach does not refire");
}

/// A level re-arms only after the reading falls below warn − 10% (70%): a dip
/// to 75% does not re-arm warn, a dip to 69% does.
#[test]
fn rearm_needs_below_seventy_percent() {
    let mut state = AlertState::default();
    assert_eq!(state.check(Some(0.85)), Some(AlertLevel::Warn));
    assert_eq!(state.check(Some(0.75)), None, "75% does not re-arm");
    assert_eq!(
        state.check(Some(0.85)),
        None,
        "still armed-out: no second warn"
    );
    assert_eq!(state.check(Some(0.69)), None, "69% re-arms, firing nothing");
    assert_eq!(
        state.check(Some(0.85)),
        Some(AlertLevel::Warn),
        "warn fires again"
    );
}

/// No ratio, no alert and no re-arm: an unlimited budget (or an unreadable
/// group) fires nothing — and, crucially, does not clear an armed episode by
/// accident.
#[test]
fn no_ratio_fires_nothing_and_keeps_episode_memory() {
    let mut state = AlertState::default();
    assert_eq!(state.check(Some(0.85)), Some(AlertLevel::Warn));
    assert_eq!(state.check(None), None, "a gap fires nothing");
    assert_eq!(
        state.check(Some(0.96)),
        Some(AlertLevel::Critical),
        "the episode survived the gap"
    );
}

/// The ratio is current/max when both are known, None otherwise.
#[test]
fn pressure_ratio_needs_both_sides() {
    let full = Pressure {
        current: Some(80),
        max: Some(100),
        ..Pressure::default()
    };
    assert_eq!(full.ratio(), Some(0.8));
    assert_eq!(
        Pressure { max: None, ..full }.ratio(),
        None,
        "unlimited budget: no ratio, no alert"
    );
    assert_eq!(
        Pressure {
            current: None,
            ..full
        }
        .ratio(),
        None,
        "unreadable group: no ratio, no alert"
    );
    assert_eq!(
        Pressure {
            max: Some(0),
            ..full
        }
        .ratio(),
        None,
        "zero max: no division by zero"
    );
}
