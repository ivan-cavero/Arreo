//! Colors and terminal capability (T-0016).
//!
//! One sentence: a theme color is either 24-bit RGB, an ANSI index, or "the
//! terminal's default" — and every render path quantizes it to what the
//! terminal can actually show before a byte reaches the tty.

use std::fmt;

/// A theme color, before capability quantization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Color {
    /// 24-bit color — what the theme files carry (truecolor-first).
    Rgb(u8, u8, u8),
    /// ANSI palette index (0–255). The `system` theme and terminals that
    /// cannot do better land here.
    Ansi(u8),
    /// `"none"` in a theme file: inherit whatever the terminal already uses.
    None,
}

impl Color {
    /// Parse a theme-file color: `#rgb`, `#rrggbb`, a bare 0–255 index, or
    /// `"none"`. `defs` references are resolved by the loader, not here.
    pub fn parse(raw: &str) -> Result<Self, ColorError> {
        let text = raw.trim();
        if text.eq_ignore_ascii_case("none") {
            return Ok(Self::None);
        }
        if let Some(hex) = text.strip_prefix('#') {
            return Self::parse_hex(hex);
        }
        if let Ok(index) = text.parse::<u16>() {
            if index <= 255 {
                return Ok(Self::Ansi(index as u8));
            }
            return Err(ColorError::OutOfRange(text.to_string()));
        }
        Err(ColorError::Malformed(text.to_string()))
    }

    fn parse_hex(hex: &str) -> Result<Self, ColorError> {
        let expand = |c: char, at: usize| -> Result<u8, ColorError> {
            c.to_digit(16)
                .map(|d| d as u8)
                .ok_or_else(|| ColorError::Malformed(format!("#{hex} (digit {at})")))
        };
        let chars: Vec<char> = hex.chars().collect();
        let (r, g, b) = match chars.len() {
            3 => {
                let v: Vec<u8> = chars
                    .iter()
                    .enumerate()
                    .map(|(i, c)| expand(*c, i).map(|d| d * 17))
                    .collect::<Result<_, _>>()?;
                (v[0], v[1], v[2])
            }
            6 => (
                expand(chars[0], 0)? * 16 + expand(chars[1], 1)?,
                expand(chars[2], 2)? * 16 + expand(chars[3], 3)?,
                expand(chars[4], 4)? * 16 + expand(chars[5], 5)?,
            ),
            _ => return Err(ColorError::Malformed(format!("#{hex}"))),
        };
        Ok(Self::Rgb(r, g, b))
    }

    /// WCAG 2.x relative luminance (0 = black, 1 = white).
    ///
    /// Only a 24-bit color has a luminance we can know. An ANSI index names a
    /// slot in *someone else's* palette — what it paints depends on the user's
    /// terminal, so claiming a number for it would be a measurement of our own
    /// guess. `None` for both, deliberately.
    #[must_use]
    pub fn relative_luminance(self) -> Option<f64> {
        let Self::Rgb(r, g, b) = self else {
            return None;
        };
        Some(0.2126 * linearize(r) + 0.7152 * linearize(g) + 0.0722 * linearize(b))
    }

    /// WCAG 2.x contrast ratio between two colors, 1.0 (identical) to 21.0
    /// (black on white). `None` when either side has no knowable luminance.
    ///
    /// This is the number the acceptance criteria and the state-label rule are
    /// judged by, so it lives in the color model rather than in a test: the
    /// code that decides what is readable and the test that checks it use the
    /// same arithmetic.
    #[must_use]
    pub fn contrast_ratio(self, other: Self) -> Option<f64> {
        let (a, b) = (self.relative_luminance()?, other.relative_luminance()?);
        let (lighter, darker) = if a >= b { (a, b) } else { (b, a) };
        Some((lighter + 0.05) / (darker + 0.05))
    }

    /// The same color mixed toward black by `factor` (0.0 leaves it alone,
    /// 1.0 is black) — BRAND §2's "states shift one step darker" for the light
    /// variant, in one place so the step is one number and not a habit.
    #[must_use]
    pub fn darkened(self, factor: f64) -> Self {
        let Self::Rgb(r, g, b) = self else {
            return self;
        };
        let step = |v: u8| ((f64::from(v) * (1.0 - factor)).round()).clamp(0.0, 255.0) as u8;
        Self::Rgb(step(r), step(g), step(b))
    }

    /// The ANSI index this color becomes when only `depth` is available.
    /// `None` in, `None` out: an unset token stays unset at every depth.
    #[must_use]
    pub fn quantize(self, depth: Depth) -> Self {
        match (self, depth) {
            (Self::None, _) | (_, Depth::NoColor) => Self::None,
            (Self::Rgb(r, g, b), Depth::Truecolor) => Self::Rgb(r, g, b),
            (Self::Rgb(r, g, b), Depth::Ansi256) => Self::Ansi(rgb_to_256(r, g, b)),
            (Self::Rgb(r, g, b), Depth::Ansi16) => Self::Ansi(rgb_to_16(r, g, b)),
            (ansi @ Self::Ansi(_), Depth::Truecolor | Depth::Ansi256) => ansi,
            (Self::Ansi(index), Depth::Ansi16) => Self::Ansi(ansi_256_to_16(index)),
        }
    }

    /// The bytes a terminal needs to set this color as a foreground.
    ///
    /// This is the contract the compatibility shim is judged by. Indexed
    /// colors go out as `38;5;N` (what crossterm emits, and what every
    /// terminal that understands indexed color parses), so the depth decides
    /// the *index range*, not the escape shape:
    ///
    /// - `Truecolor` → `38;2;R;G;B` (and nothing else ever uses this form)
    /// - `Ansi256` → `38;5;N`, `N ≥ 16` (cube/ramp, never the user's palette)
    /// - `Ansi16` → `38;5;N`, `N ≤ 15` (only the terminal's own palette)
    /// - `NoColor` → an empty string: no sequence leaves the process at all
    #[must_use]
    pub fn fg_sequence(self, depth: Depth) -> String {
        match self.quantize(depth) {
            Self::None => String::new(),
            Self::Rgb(r, g, b) => format!("\u{1b}[38;2;{r};{g};{b}m"),
            Self::Ansi(index) => format!("\u{1b}[38;5;{index}m"),
        }
    }
}

impl fmt::Display for Color {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rgb(r, g, b) => write!(f, "#{r:02x}{g:02x}{b:02x}"),
            Self::Ansi(index) => write!(f, "{index}"),
            Self::None => f.write_str("none"),
        }
    }
}

/// What the terminal on the other end can render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Truecolor,
    Ansi256,
    Ansi16,
    /// `NO_COLOR` was set (or the terminal is `dumb`): styles carry no color.
    NoColor,
}

impl Depth {
    /// Detect from the process environment.
    #[must_use]
    pub fn detect() -> Self {
        Self::detect_from(|key| std::env::var(key).ok())
    }

    /// Detection over an injectable environment (so the matrix is testable
    /// without mutating the test process's own env).
    #[must_use]
    pub fn detect_from<F>(env: F) -> Self
    where
        F: Fn(&str) -> Option<String>,
    {
        // NO_COLOR wins outright (no-color.org: presence, even empty).
        if env("NO_COLOR").is_some() {
            return Self::NoColor;
        }
        let term = env("TERM").unwrap_or_default();
        if term == "dumb" {
            return Self::NoColor;
        }
        if let Some(colorterm) = env("COLORTERM") {
            let colorterm = colorterm.to_ascii_lowercase();
            if colorterm.contains("truecolor") || colorterm.contains("24bit") {
                return Self::Truecolor;
            }
        }
        if term.contains("direct") || term.contains("truecolor") {
            return Self::Truecolor;
        }
        if term.contains("256") {
            return Self::Ansi256;
        }
        // Legacy terminals that lie about 256: Apple's Terminal.app reports
        // `xterm-256color` only when it can actually do 256.
        if term.is_empty() {
            return Self::Ansi16;
        }
        Self::Ansi16
    }

    /// True when this depth can carry a 24-bit value through untouched.
    #[must_use]
    pub fn is_truecolor(self) -> bool {
        self == Self::Truecolor
    }
}

/// Color parsing failure (a theme file with a bad value).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ColorError {
    #[error("malformed color {0:?} (want #rgb, #rrggbb, a 0-255 index, or \"none\")")]
    Malformed(String),
    #[error("color index out of range: {0}")]
    OutOfRange(String),
}

/// One sRGB channel, linearized for luminance maths (WCAG 2.x §relative
/// luminance: the piecewise transfer function, not a plain `v / 255`).
fn linearize(channel: u8) -> f64 {
    let v = f64::from(channel) / 255.0;
    if v <= 0.040_45 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// The 16 ANSI colors as RGB, i.e. what a 16-color terminal paints. Legacy
/// Terminal.app and conhost land here; the values are the standard xterm set
/// these terminals approximate.
const ANSI16: [(u8, u8, u8); 16] = [
    (0x00, 0x00, 0x00),
    (0x80, 0x00, 0x00),
    (0x00, 0x80, 0x00),
    (0x80, 0x80, 0x00),
    (0x00, 0x00, 0x80),
    (0x80, 0x00, 0x80),
    (0x00, 0x80, 0x80),
    (0xc0, 0xc0, 0xc0),
    (0x80, 0x80, 0x80),
    (0xff, 0x00, 0x00),
    (0x00, 0xff, 0x00),
    (0xff, 0xff, 0x00),
    (0x00, 0x00, 0xff),
    (0xff, 0x00, 0xff),
    (0x00, 0xff, 0xff),
    (0xff, 0xff, 0xff),
];

/// 6×6×6 color cube levels used by every 256-color terminal.
const CUBE: [u8; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];

fn distance(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    // Weighted Euclidean (redmean) — closer to perceived difference than
    // plain RGB distance, and cheap enough to run in a render loop.
    let (r1, g1, b1) = (i32::from(a.0), i32::from(a.1), i32::from(a.2));
    let (r2, g2, b2) = (i32::from(b.0), i32::from(b.1), i32::from(b.2));
    let rmean = (r1 + r2) / 2;
    let dr = r1 - r2;
    let dg = g1 - g2;
    let db = b1 - b2;
    let weighted =
        (((512 + rmean) * dr * dr) >> 8) + 4 * dg * dg + (((767 - rmean) * db * db) >> 8);
    weighted.max(0) as u32
}

/// Nearest xterm-256 index for an RGB value (cube + grayscale ramp).
#[must_use]
fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    let cube_index = |v: u8| -> u8 {
        // The cube uses non-linear levels; pick the closest level index.
        let mut best = 0usize;
        let mut best_d = u32::MAX;
        for (i, level) in CUBE.iter().enumerate() {
            let d = u32::from(v).abs_diff(u32::from(*level));
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best as u8
    };
    let (ri, gi, bi) = (cube_index(r), cube_index(g), cube_index(b));
    let cube_rgb = (CUBE[ri as usize], CUBE[gi as usize], CUBE[bi as usize]);
    let cube_256 = 16 + 36 * ri + 6 * gi + bi;

    // Grayscale ramp: 232..255 → 8, 18, ..., 238.
    let avg = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    let gray_step = ((avg.saturating_sub(8) + 5) / 10).min(23) as u8;
    let gray_value = 8 + 10 * u16::from(gray_step);
    let gray_256 = 232 + gray_step;

    let cube_d = distance((r, g, b), cube_rgb);
    let gray_d = distance(
        (r, g, b),
        (
            gray_value.min(255) as u8,
            gray_value.min(255) as u8,
            gray_value.min(255) as u8,
        ),
    );
    // The first 16 are the terminal's palette — never chosen for RGB input
    // (a theme's truecolor value must not silently become "the user's red").
    if gray_d < cube_d {
        gray_256
    } else {
        cube_256
    }
}

/// Nearest legacy-16 index for an RGB value.
#[must_use]
fn rgb_to_16(r: u8, g: u8, b: u8) -> u8 {
    let mut best = 0u8;
    let mut best_d = u32::MAX;
    for (i, candidate) in ANSI16.iter().enumerate() {
        let d = distance((r, g, b), *candidate);
        if d < best_d {
            best_d = d;
            best = i as u8;
        }
    }
    best
}

/// Nearest legacy-16 index for any 256-color index.
#[must_use]
fn ansi_256_to_16(index: u8) -> u8 {
    if index < 16 {
        return index;
    }
    let (r, g, b) = if index < 232 {
        let i = index - 16;
        (
            CUBE[(i / 36) as usize],
            CUBE[((i % 36) / 6) as usize],
            CUBE[(i % 6) as usize],
        )
    } else {
        let level = 8 + 10 * (index - 232);
        (level, level, level)
    };
    rgb_to_16(r, g, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn parses_every_supported_color_form() {
        assert_eq!(Color::parse("#7dcfff"), Ok(Color::Rgb(0x7d, 0xcf, 0xff)));
        assert_eq!(Color::parse("#fff"), Ok(Color::Rgb(255, 255, 255)));
        assert_eq!(Color::parse("none"), Ok(Color::None));
        assert_eq!(Color::parse("NONE"), Ok(Color::None));
        assert_eq!(Color::parse("13"), Ok(Color::Ansi(13)));
        assert!(matches!(
            Color::parse("#12345"),
            Err(ColorError::Malformed(_))
        ));
        assert!(matches!(
            Color::parse("300"),
            Err(ColorError::OutOfRange(_))
        ));
        assert!(matches!(
            Color::parse("chartreuse"),
            Err(ColorError::Malformed(_))
        ));
    }

    #[test]
    fn detection_matrix_matches_the_roadmap() {
        let cases: &[(&[(&str, &str)], Depth)] = &[
            (
                &[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")],
                Depth::Truecolor,
            ),
            (
                &[("COLORTERM", "24bit"), ("TERM", "xterm")],
                Depth::Truecolor,
            ),
            (&[("TERM", "xterm-direct")], Depth::Truecolor),
            (&[("TERM", "xterm-256color")], Depth::Ansi256),
            (&[("TERM", "screen-256color")], Depth::Ansi256),
            (&[("TERM", "xterm")], Depth::Ansi16),
            (&[("TERM", "linux")], Depth::Ansi16),
            // Legacy Terminal.app shape: no COLORTERM, plain xterm.
            (&[("TERM", "xterm-16color")], Depth::Ansi16),
            // conhost shape: TERM unset entirely.
            (&[], Depth::Ansi16),
            // NO_COLOR beats everything, even a truecolor hint.
            (
                &[("NO_COLOR", ""), ("COLORTERM", "truecolor")],
                Depth::NoColor,
            ),
            (&[("TERM", "dumb")], Depth::NoColor),
        ];
        for (pairs, want) in cases {
            assert_eq!(Depth::detect_from(env_of(pairs)), *want, "env {pairs:?}");
        }
    }

    #[test]
    fn quantizes_to_256_within_one_step_of_the_cube() {
        // Known xterm values: these are the classic nearest-cube answers.
        assert_eq!(
            Color::Rgb(0x00, 0x00, 0x00).quantize(Depth::Ansi256),
            Color::Ansi(16)
        );
        assert_eq!(
            Color::Rgb(0xff, 0xff, 0xff).quantize(Depth::Ansi256),
            Color::Ansi(231)
        );
        assert_eq!(
            Color::Rgb(0xff, 0x00, 0x00).quantize(Depth::Ansi256),
            Color::Ansi(196)
        );
        // Cube levels (0, 1, 2) -> 16 + 36*0 + 6*1 + 2.
        assert_eq!(
            Color::Rgb(0x00, 0x5f, 0x87).quantize(Depth::Ansi256),
            Color::Ansi(24)
        );
        // Grays prefer the ramp over the cube.
        assert_eq!(
            Color::Rgb(0x80, 0x80, 0x80).quantize(Depth::Ansi256),
            Color::Ansi(244)
        );
        // Every 24-bit value must land inside the 256 range, never on the
        // first 16 (those are the user's palette, not the theme's).
        for r in (0..=255).step_by(17) {
            for g in (0..=255).step_by(51) {
                for b in (0..=255).step_by(85) {
                    let q = Color::Rgb(r, g, b).quantize(Depth::Ansi256);
                    match q {
                        Color::Ansi(index) => assert!(index >= 16, "{r},{g},{b} -> {index}"),
                        other => panic!("unexpected {other:?}"),
                    }
                }
            }
        }
    }

    #[test]
    fn quantizes_to_16_without_glitched_escapes() {
        assert_eq!(
            Color::Rgb(0xff, 0x00, 0x00).quantize(Depth::Ansi16),
            Color::Ansi(9)
        );
        assert_eq!(
            Color::Rgb(0x00, 0x00, 0x00).quantize(Depth::Ansi16),
            Color::Ansi(0)
        );
        assert_eq!(
            Color::Rgb(0xff, 0xff, 0xff).quantize(Depth::Ansi16),
            Color::Ansi(15)
        );
        // 256-index input still has to survive a 16-color terminal.
        assert_eq!(Color::Ansi(196).quantize(Depth::Ansi16), Color::Ansi(9));
        assert_eq!(Color::Ansi(244).quantize(Depth::Ansi16), Color::Ansi(8));
        assert_eq!(Color::Ansi(13).quantize(Depth::Ansi16), Color::Ansi(13));
    }

    #[test]
    fn sequences_only_use_what_the_depth_supports() {
        let brand = Color::Rgb(0x7d, 0xcf, 0xff);
        assert_eq!(
            brand.fg_sequence(Depth::Truecolor),
            "\u{1b}[38;2;125;207;255m"
        );
        let at_256 = brand.fg_sequence(Depth::Ansi256);
        assert!(at_256.starts_with("\u{1b}[38;5;"), "{at_256:?}");
        assert!(
            !at_256.contains("38;2;"),
            "truecolor leaked to a 256 terminal"
        );
        let at_16 = brand.fg_sequence(Depth::Ansi16);
        assert!(
            at_16.starts_with("\u{1b}[3") || at_16.starts_with("\u{1b}[9"),
            "{at_16:?} is not a legacy SGR"
        );
        assert_eq!(brand.fg_sequence(Depth::NoColor), "");
        assert_eq!(Color::None.fg_sequence(Depth::Truecolor), "");
    }
}
