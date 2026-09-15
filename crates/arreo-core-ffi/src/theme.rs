//! The theme engine's tokens (T-0104).
//!
//! One sentence: a theme resolves into quantized colors, and the phone reads the
//! **same token table** the TUI, the CLI and the reference HTML read — so the
//! sidebar and the phone cannot drift apart.
//!
//! **No filesystem, and that is why the built-in is what is exposed.** The
//! loader's job is to *find* theme files (built-in → user → project → cwd), which
//! is a directory walk; a phone has no such hierarchy. What a phone needs is the
//! shipped `arreo` theme's resolved table, and that is in the binary already
//! (`Catalog::builtin()` is `include_str!`-embedded), so
//! [`theme_builtin`] parses from memory and nothing here touches a path.
//! A theme the user *authored* arrives as a token table the UI passes back in —
//! `Theme::new` takes exactly that, and there is no reader for it here on
//! purpose.
//!
//! **Depth is the caller's to state, not to detect.** `Depth::detect` reads
//! `TERM`/`COLORTERM`/`NO_COLOR` from the process environment — a terminal
//! question, and a phone has no terminal. So the depth is a parameter: an iOS or
//! Android UI passes the one its own surface can render, which is a fact it
//! knows and this crate does not.

use std::collections::BTreeMap;
use std::sync::Arc;

use arreo_core::theme::{Color, Theme, Variant};

use arreo_core::theme::Depth;

use crate::errors::ColorFfiError;

/// Which of the brand's two palettes a theme resolves against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiVariant {
    Dark,
    Light,
}

impl From<Variant> for FfiVariant {
    fn from(variant: Variant) -> Self {
        match variant {
            Variant::Dark => Self::Dark,
            Variant::Light => Self::Light,
        }
    }
}

impl From<FfiVariant> for Variant {
    fn from(variant: FfiVariant) -> Self {
        match variant {
            FfiVariant::Dark => Self::Dark,
            FfiVariant::Light => Self::Light,
        }
    }
}

/// What the surface on the other end can render.
///
/// The same four the core has, with the caller stating which one applies — see
/// the module docs for why this is not detected here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDepth {
    /// 24-bit color, untouched.
    Truecolor,
    /// The 256-color palette.
    Ansi256,
    /// The 16-color palette.
    Ansi16,
    /// No color at all: styles carry weight and shape only.
    NoColor,
}

impl From<Depth> for FfiDepth {
    fn from(depth: Depth) -> Self {
        match depth {
            Depth::Truecolor => Self::Truecolor,
            Depth::Ansi256 => Self::Ansi256,
            Depth::Ansi16 => Self::Ansi16,
            Depth::NoColor => Self::NoColor,
        }
    }
}

impl From<FfiDepth> for Depth {
    fn from(depth: FfiDepth) -> Self {
        match depth {
            FfiDepth::Truecolor => Self::Truecolor,
            FfiDepth::Ansi256 => Self::Ansi256,
            FfiDepth::Ansi16 => Self::Ansi16,
            FfiDepth::NoColor => Self::NoColor,
        }
    }
}

/// A theme color, before or after capability quantization.
///
/// The core's third case is `Color::None` — "inherit whatever the terminal
/// already uses" — and it is spelled `TerminalDefault` here because `None` is a
/// reserved word in Swift's `Optional` namespace and a variant that collides
/// with it would be the kind of bug that only shows up in an Xcode build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiColor {
    Rgb { r: u8, g: u8, b: u8 },
    Ansi { index: u8 },
    TerminalDefault,
}

impl From<Color> for FfiColor {
    fn from(color: Color) -> Self {
        match color {
            Color::Rgb(r, g, b) => Self::Rgb { r, g, b },
            Color::Ansi(index) => Self::Ansi { index },
            Color::None => Self::TerminalDefault,
        }
    }
}

impl From<FfiColor> for Color {
    fn from(color: FfiColor) -> Self {
        match color {
            FfiColor::Rgb { r, g, b } => Self::Rgb(r, g, b),
            FfiColor::Ansi { index } => Self::Ansi(index),
            FfiColor::TerminalDefault => Self::None,
        }
    }
}

/// One resolved token: its name and the color this theme gives it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ThemeToken {
    /// The semantic name (`background`, `working`, `textMuted`, …).
    pub name: String,
    pub color: FfiColor,
}

/// Parse a theme-file color: `#rgb`, `#rrggbb`, a bare 0–255 index, or `"none"`.
#[uniffi::export]
pub fn color_parse(text: String) -> Result<FfiColor, ColorFfiError> {
    Ok(FfiColor::from(Color::parse(&text)?))
}

/// Quantize a color to what a surface can render.
#[uniffi::export]
#[must_use]
pub fn color_quantize(color: FfiColor, depth: FfiDepth) -> FfiColor {
    FfiColor::from(Color::from(color).quantize(Depth::from(depth)))
}

/// The ANSI escape sequence that paints with this color at this depth.
///
/// `TerminalDefault` yields the reset sequence, which is what "inherit" means
/// once a sequence has to be emitted at all.
#[uniffi::export]
#[must_use]
pub fn color_fg_sequence(color: FfiColor, depth: FfiDepth) -> String {
    Color::from(color).fg_sequence(Depth::from(depth))
}

/// The WCAG contrast ratio between two colors, or nothing when either is
/// `TerminalDefault` — a terminal's own palette is not knowable, and inventing a
/// ratio for it would be a number that means nothing.
#[uniffi::export]
#[must_use]
pub fn color_contrast_ratio(a: FfiColor, b: FfiColor) -> Option<f64> {
    Color::from(a).contrast_ratio(Color::from(b))
}

/// A resolved, capability-quantized theme — the table a UI reads.
#[derive(uniffi::Object)]
pub struct ThemeHandle {
    theme: Theme,
}

/// The `arreo` built-in at a variant and a depth.
///
/// The base every theme starts from, and the look that ships in the binary:
/// `Theme::arreo_variant` reads the embedded theme file from memory, so this
/// needs no filesystem and cannot fail.
#[uniffi::export]
#[must_use]
pub fn theme_builtin(variant: FfiVariant, depth: FfiDepth) -> Arc<ThemeHandle> {
    Arc::new(ThemeHandle {
        theme: Theme::arreo_variant(Variant::from(variant), Depth::from(depth)),
    })
}

/// Build a theme from an already-resolved token table.
///
/// The path for a theme the *user* authored: the UI reads its own file (it has
/// the platform's file APIs and this crate deliberately does not) and hands the
/// resolved tokens in. A token the table does not carry reads back as
/// `TerminalDefault`, exactly as the core's `Theme::color` does.
#[uniffi::export]
#[must_use]
pub fn theme_from_tokens(
    name: String,
    variant: FfiVariant,
    depth: FfiDepth,
    tokens: Vec<ThemeToken>,
) -> Arc<ThemeHandle> {
    let mut colors: BTreeMap<String, Color> = BTreeMap::new();
    for token in tokens {
        colors.insert(token.name, Color::from(token.color));
    }
    Arc::new(ThemeHandle {
        theme: Theme::new(name, Variant::from(variant), Depth::from(depth), colors),
    })
}

#[uniffi::export]
impl ThemeHandle {
    #[must_use]
    pub fn name(&self) -> String {
        self.theme.name().to_string()
    }

    #[must_use]
    pub fn variant(&self) -> FfiVariant {
        FfiVariant::from(self.theme.variant())
    }

    #[must_use]
    pub fn depth(&self) -> FfiDepth {
        FfiDepth::from(self.theme.depth())
    }

    /// Every resolved token, sorted by name — the shared table the HTML
    /// reference renders from the same source.
    #[must_use]
    pub fn tokens(&self) -> Vec<ThemeToken> {
        self.theme
            .colors()
            .iter()
            .map(|(name, color)| ThemeToken {
                name: name.clone(),
                color: FfiColor::from(color.quantize(self.theme.depth())),
            })
            .collect()
    }

    /// A token's color, quantized for this theme's depth.
    ///
    /// An unknown name yields `TerminalDefault` rather than an invented color —
    /// tokens are validated at load, so an unknown name here is a caller's bug
    /// and the core's answer is "inherit", not a guess.
    #[must_use]
    pub fn color(&self, token: String) -> FfiColor {
        FfiColor::from(self.theme.color(&token))
    }

    /// The state board's language: the same hues the TUI sidebar and the status
    /// board paint.
    #[must_use]
    pub fn state_color(&self, state: String) -> FfiColor {
        FfiColor::from(self.theme.state_color(&state))
    }

    /// The color a state's **label** is painted in: the state's own hue when it
    /// carries WCAG AA as text on this theme's background, `textMuted` when it
    /// does not.
    ///
    /// A state dot is a graphic, judged at 3:1; a label is text, judged at
    /// 4.5:1. The core makes that distinction once, and a phone that painted the
    /// label with `state_color` would be the one surface where the brand's muted
    /// `idle` hue fails the contrast rule.
    #[must_use]
    pub fn state_label_color(&self, state: String) -> FfiColor {
        FfiColor::from(self.theme.state_label_color(&state))
    }

    /// Re-quantize for a different surface: a theme is data, and depth is not
    /// baked into it.
    #[must_use]
    pub fn with_depth(&self, depth: FfiDepth) -> Arc<ThemeHandle> {
        Arc::new(ThemeHandle {
            theme: self.theme.with_depth(Depth::from(depth)),
        })
    }
}
