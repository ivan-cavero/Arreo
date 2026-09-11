//! Relay presence (T-0031): online/offline/last-seen from one stated staleness rule.
//!
//! One sentence: the relay already sees connect and disconnect, so presence is
//! an indexed write per device per heartbeat — not a probe, which a NAT'd phone
//! could never answer.
//!
//! **One rule, owned here.** The *thresholds* (how long "recently" is) are the
//! product contract and live in `arreo_core::mesh::directory` (`ONLINE_WINDOW_SECS`,
//! `STALE_AFTER_SECS`), where the directory consumes them too. What lives here is
//! the relay-side *lifecycle*: when `last_seen_ms` is written (connect, heartbeat,
//! disconnect), how a reader lists it, and the rendering a human reads. No
//! consumer re-derives a window — `Presence::of` is the only place the comparison
//! happens, and this module calls it rather than repeating it.

use arreo_core::mesh::directory::{age_ms, presence_of, Presence, ONLINE_WINDOW_SECS};
use std::time::Duration;

/// How often a connected device refreshes its `last_seen_ms`.
pub const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// The heartbeat is jittered so a fleet that connected together does not write
/// in lockstep. The spread is ±20% of the interval — enough to flatten the
/// write spike, small enough that the 90 s window is never at risk.
pub const HEARTBEAT_JITTER: Duration = Duration::from_secs(6);

/// One device's presence as the relay reports it: metadata only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevicePresence {
    pub device_id: String,
    pub account_id: String,
    pub presence: Presence,
    pub last_seen_ms: i64,
}

impl DevicePresence {
    /// Human rendering: `online`, `offline (last seen 15 d)` — never an
    /// error for a machine that has simply been off. This is the formatting
    /// `arreo machines` (T-0044) and remote attach (T-0032) render, so it is
    /// asserted here rather than reimplemented in each.
    #[must_use]
    pub fn display(&self, now_ms: i64) -> String {
        match self.presence {
            Presence::Online => "online".to_string(),
            Presence::Offline => format!(
                "offline (last seen {})",
                format_age(age_ms(self.last_seen_ms, now_ms))
            ),
            Presence::Stale => format!(
                "stale (last seen {})",
                format_age(age_ms(self.last_seen_ms, now_ms))
            ),
        }
    }
}

/// The single presence rule, exported so the relay's lifecycle and its tests
/// share it: `online` while `last_seen` is within the window (3 missed beats),
/// `offline` beyond that, `stale` past the tombstone signal the directory
/// consumes. A future `last_seen` clamps to `online` — a clock that stepped
/// backwards is "just now", never a panic or a negative age.
#[must_use]
pub fn presence_at(last_seen_ms: i64, now_ms: i64) -> Presence {
    presence_of(last_seen_ms, now_ms)
}

/// How long until the next heartbeat, jittered.
///
/// A thin alias over `arreo_core::relay::session::heartbeat_delay`, kept so the
/// relay's own code reads from its own module while the arithmetic lives in
/// exactly one place. The values are asserted to agree by test (see
/// `crates/arreo-relay/tests/presence.rs`), because two spellings of one fact
/// would be a drift risk and the dependency rule forbids core from importing
/// the relay.
#[must_use]
pub fn next_heartbeat_delay(jitter_fraction: f64) -> Duration {
    arreo_core::relay::session::heartbeat_delay(jitter_fraction)
}

/// The online window, in the units the criterion states it: 3 missed beats.
#[must_use]
pub fn missed_beats() -> u64 {
    ONLINE_WINDOW_SECS / HEARTBEAT_INTERVAL.as_secs()
}

/// A human age: `0 s`, `5 m`, `3 h`, `15 d`, `2 mo`. Shapes asserted by test —
/// the 15-day case must read `15 d`, never an error.
#[must_use]
pub fn format_age(age_ms: i64) -> String {
    let age_ms = age_ms.max(0);
    let secs = age_ms / 1000;
    if secs < 60 {
        return format!("{} s", secs);
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{} m", mins);
    }
    let hours = mins / 60;
    if hours < 48 {
        return format!("{} h", hours);
    }
    let days = hours / 24;
    if days < 60 {
        return format!("{} d", days);
    }
    format!("{} mo", days / 30)
}
