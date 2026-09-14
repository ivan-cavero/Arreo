//! Quiet hours (T-0093): a local wall-clock window in which notifications are held.
//!
//! ## Why local time is an offset and not a time zone
//!
//! `std` has no local-time conversion, and the obvious answer — the `time`
//! crate's `local_offset` — is a trap in exactly this program: it returns an
//! error in a multi-threaded process unless the `unsound_local_offset` feature is
//! enabled, and this daemon is multi-threaded by construction. The alternatives
//! are a time-zone database dependency or reading `/etc/localtime` by hand;
//! neither is worth it for a window that is a *policy*, not a timestamp.
//!
//! So a window is **local wall-clock minutes plus an explicit
//! `utc_offset_minutes`** from the same configuration file. It is mean solar
//! offset, not a time zone: **a machine that observes DST will be an hour out for
//! half the year**, and that is written down here and in `docs/notifications.md`
//! rather than hidden. An operator whose quiet hours matter to the hour can set
//! the offset by hand twice a year, or leave notifications off during their
//! working day and rely on `arreo notify --why` when they wonder.

use std::fmt;

/// A daily window, in minutes since local midnight, half-open: `[start, end)`.
///
/// Half-open on purpose, and asserted at both ends by the tests: a window of
/// `22:00-07:00` includes 22:00 and excludes 07:00, so two adjacent windows
/// (`00:00-07:00` and `07:00-12:00`) tile the day without a minute belonging to
/// both — which is the property a policy window needs and a closed interval
/// cannot have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuietHours {
    pub start_min: u32,
    pub end_min: u32,
}

/// What can be wrong with a window specification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum QuietError {
    #[error("{0:?} is not a quiet-hours window: expected `HH:MM-HH:MM`, e.g. `22:00-07:00`")]
    Shape(String),
    #[error("{0:?} is not an hour of the day (00-23)")]
    Hour(String),
    #[error("{0:?} is not a minute of the hour (00-59)")]
    Minute(String),
}

impl QuietHours {
    /// Parse `HH:MM-HH:MM`. Flexible about the one-digit hour and the spaces a
    /// human writes (`7:00 - 22:00`), strict about the shape and the ranges.
    pub fn parse(spec: &str) -> Result<Self, QuietError> {
        let (start, end) = spec
            .split_once('-')
            .ok_or_else(|| QuietError::Shape(spec.to_string()))?;
        Ok(Self {
            start_min: parse_clock(start.trim(), spec)?,
            end_min: parse_clock(end.trim(), spec)?,
        })
    }

    /// Whether `at_ms` (a UTC epoch time) falls in the window.
    ///
    /// A window that **wraps midnight** (`22:00-07:00`) is the common case, and
    /// it is why this is not a single comparison: the window is two ranges of the
    /// day, and the operator wrote one.
    ///
    /// **Equal bounds mean an empty window**, not a full day: `22:00-22:00` is a
    /// zero-length interval, and reading it as "quiet for 24 hours" would be a
    /// trap — an operator who wants no notifications at all turns them off, which
    /// is one configuration decision rather than a clever reading of a window.
    #[must_use]
    pub fn contains(&self, at_ms: u64, utc_offset_minutes: i32) -> bool {
        let now = local_minutes(at_ms, utc_offset_minutes);
        if self.start_min == self.end_min {
            return false;
        }
        if self.start_min < self.end_min {
            (self.start_min..self.end_min).contains(&now)
        } else {
            now >= self.start_min || now < self.end_min
        }
    }

    /// The window spelled back the way the operator wrote it, for a refusal's
    /// text: "why was I not told?" is answered with the window that held it.
    #[must_use]
    pub fn window(&self) -> String {
        format!(
            "{:02}:{:02}-{:02}:{:02}",
            self.start_min / 60,
            self.start_min % 60,
            self.end_min / 60,
            self.end_min % 60
        )
    }
}

impl fmt::Display for QuietHours {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.window())
    }
}

/// The local time of day, in minutes since local midnight.
///
/// The offset is applied in **minutes on a signed integer** and folded into a
/// day at the end, so a negative offset works the way a human means it:
/// `utc_offset_minutes = -300` at 02:00 UTC is 21:00 the *previous* local day,
/// and `21 * 60` is what a quiet window must be compared against.
#[must_use]
pub fn local_minutes(at_ms: u64, utc_offset_minutes: i32) -> u32 {
    const DAY: i64 = 24 * 60;
    let utc_minutes = (at_ms / 60_000) as i64;
    let local = utc_minutes + i64::from(utc_offset_minutes);
    local.rem_euclid(DAY) as u32
}

/// `HH:MM` → minutes since midnight.
fn parse_clock(text: &str, whole: &str) -> Result<u32, QuietError> {
    let (hour, minute) = text
        .split_once(':')
        .ok_or_else(|| QuietError::Shape(whole.to_string()))?;
    let hour: u32 = hour
        .trim()
        .parse()
        .map_err(|_| QuietError::Hour(text.to_string()))?;
    let minute: u32 = minute
        .trim()
        .parse()
        .map_err(|_| QuietError::Minute(text.to_string()))?;
    if hour > 23 {
        return Err(QuietError::Hour(text.to_string()));
    }
    if minute > 59 {
        return Err(QuietError::Minute(text.to_string()));
    }
    Ok(hour * 60 + minute)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One UTC timestamp, so every case below is arithmetic a reader can check.
    /// 2026-01-15T02:00:00Z, and the same instant in a few offsets.
    const AT_0200_UTC: u64 = 1_768_442_400_000;

    /// The local-minute arithmetic, including the case that makes the offset a
    /// signed value: going *backwards* across midnight.
    #[test]
    fn local_minutes_folds_a_signed_offset_into_the_day() {
        assert_eq!(local_minutes(AT_0200_UTC, 0), 2 * 60);
        assert_eq!(local_minutes(AT_0200_UTC, 60), 3 * 60);
        assert_eq!(local_minutes(AT_0200_UTC, 120), 4 * 60);
        // 02:00 UTC in New York (UTC-5) is 21:00 the previous day.
        assert_eq!(local_minutes(AT_0200_UTC, -300), 21 * 60);
        // …and in Auckland (UTC+13) it is 15:00 the same day.
        assert_eq!(local_minutes(AT_0200_UTC, 780), 15 * 60);
    }

    /// **The window's two edges, asserted rather than sampled.** The start is
    /// inside, the end is outside — that is what makes adjacent windows tile the
    /// day without overlapping.
    #[test]
    fn the_window_is_half_open_at_both_edges() {
        let night = QuietHours::parse("22:00-07:00").expect("a window");
        // Exactly at the start: quiet.
        let at_2200 = AT_0200_UTC - 4 * 3_600_000; // 22:00 UTC the previous day
        assert!(night.contains(at_2200, 0), "22:00 is inside");
        // One minute before the start: not quiet.
        assert!(!night.contains(at_2200 - 60_000, 0), "21:59 is outside");
        // One minute before the end (07:00): quiet.
        let at_0700 = AT_0200_UTC + 5 * 3_600_000;
        assert!(night.contains(at_0700 - 60_000, 0), "06:59 is inside");
        // Exactly at the end: not quiet.
        assert!(!night.contains(at_0700, 0), "07:00 is outside");

        // And a daytime window is the same shape.
        let day = QuietHours::parse("09:00-17:00").expect("a window");
        let at_0900 = AT_0200_UTC + 7 * 3_600_000;
        let at_1700 = AT_0200_UTC + 15 * 3_600_000;
        assert!(day.contains(at_0900, 0), "09:00 is inside");
        assert!(!day.contains(at_0900 - 60_000, 0), "08:59 is outside");
        assert!(!day.contains(at_1700, 0), "17:00 is outside");
        assert!(day.contains(at_1700 - 60_000, 0), "16:59 is inside");
    }

    /// The offset is part of the question: the same instant can be quiet for one
    /// operator and not for another.
    #[test]
    fn the_offset_moves_the_window_with_the_clock() {
        let night = QuietHours::parse("22:00-07:00").expect("a window");
        // 02:00 UTC is inside the window at UTC…
        assert!(night.contains(AT_0200_UTC, 0));
        // …and outside it for an operator five hours west, whose local time is
        // 21:00 — the hour before their quiet hours start.
        assert!(!night.contains(AT_0200_UTC, -300));
        // …and inside for one nine hours east, whose 22:00 is 13:00 UTC — the
        // moment their window opens. (12:00 UTC would be 21:00 there: an hour
        // before, which is the assertion one line down.)
        let at_1200 = AT_0200_UTC + 10 * 3_600_000;
        let at_1300 = AT_0200_UTC + 11 * 3_600_000;
        assert!(night.contains(at_1300, 540));
        assert!(!night.contains(at_1200, 0));
    }

    /// Equal bounds are an empty window, not a day-long one — the trap this
    /// reading exists to avoid.
    #[test]
    fn equal_bounds_are_an_empty_window() {
        let none = QuietHours::parse("22:00-22:00").expect("a window");
        for hour in 0..24 {
            assert!(
                !none.contains(AT_0200_UTC + hour * 3_600_000, 0),
                "{hour}:00 must not be quiet"
            );
        }
    }

    /// The parse is flexible about what a human types and strict about the shape
    /// and the ranges — a typo is refused with the text, not silently ignored.
    #[test]
    fn parsing_is_flexible_about_spacing_and_strict_about_ranges() {
        assert_eq!(
            QuietHours::parse("7:00 - 22:30").expect("spaces"),
            QuietHours {
                start_min: 7 * 60,
                end_min: 22 * 60 + 30
            }
        );
        assert_eq!(
            QuietHours::parse("22:00-07:00").expect("a window").window(),
            "22:00-07:00",
            "and it spells itself back the way it was written"
        );
        for (bad, want) in [
            ("2200-0700", "HH:MM-HH:MM"),
            ("25:00-07:00", "hour"),
            ("22:60-07:00", "minute"),
            ("-", "HH:MM-HH:MM"),
        ] {
            let err = QuietHours::parse(bad).expect_err(bad);
            assert!(
                err.to_string().contains(want),
                "{bad}: {err} should mention {want}"
            );
        }
    }
}
