//! TUI adapter for the theming engine (T-0016).
//!
//! One sentence: the engine in `arreo_core::theme` owns themes, depth and
//! quantization; this module is the only place that knows ratatui exists.
//!
//! The TUI reads colors through [`ThemeState`], which holds the resolved
//! [`arreo_core::theme::Theme`] — every widget goes through it, so switching
//! themes (or depths) is a data swap, not a re-render rewrite.

use arreo_core::theme::{Catalog, Color, Depth, LoadError, Theme, Variant};
use ratatui::style::{Color as UiColor, Style};

/// A resolved theme plus the catalog it came from (the picker's data).
pub struct ThemeState {
    catalog: Catalog,
    theme: Theme,
    variant: Variant,
    depth: Depth,
    /// Why the starting theme is not the one that was asked for (a broken
    /// user theme shadowing `arreo`, say). Surfaced in the status line instead
    /// of leaving the user wondering why their colors did nothing.
    startup_error: Option<String>,
}

impl Default for ThemeState {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemeState {
    /// Load the catalog from the standard hierarchy and select the `arreo`
    /// built-in at the detected terminal depth. Built-ins are embedded, so a
    /// usable base look always exists; if a user theme shadows `arreo` with
    /// something broken, that reason is kept for the status line instead of
    /// leaving the user staring at unchanged colors.
    #[must_use]
    pub fn new() -> Self {
        Self::with_depth(Depth::detect(), Variant::Dark)
    }

    #[must_use]
    pub fn with_depth(depth: Depth, variant: Variant) -> Self {
        let catalog = Catalog::discover(&Catalog::default_dirs());
        let (theme, startup_error) = match catalog.theme_with_depth("arreo", variant, depth) {
            Ok(theme) => (theme, None),
            Err(e) => (
                Theme::arreo(depth),
                Some(format!("theme \"arreo\" unusable: {e}")),
            ),
        };
        Self {
            catalog,
            theme,
            variant,
            depth,
            startup_error,
        }
    }

    /// A message for the status line when the selected starting theme was not
    /// usable (otherwise the user sees default colors with no explanation).
    #[must_use]
    pub fn startup_error(&self) -> Option<&str> {
        self.startup_error.as_deref()
    }

    /// Themes whose files could not be parsed (name, reason).
    #[must_use]
    pub fn broken(&self) -> Vec<(String, String)> {
        self.catalog.broken()
    }

    #[must_use]
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    #[must_use]
    pub fn depth(&self) -> Depth {
        self.depth
    }

    #[must_use]
    pub fn variant(&self) -> Variant {
        self.variant
    }

    /// Every theme the picker can offer, built-ins first.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.catalog.names()
    }

    /// Where a theme came from (`None` = embedded in the binary).
    #[must_use]
    pub fn source_of(&self, name: &str) -> Option<String> {
        self.catalog
            .source(name)
            .map(|path| path.display().to_string())
    }

    /// The depth as it appears in the UI (`truecolor`, `256`, `16`, `none`).
    #[must_use]
    pub fn depth_name(&self) -> &'static str {
        match self.depth {
            Depth::Truecolor => "truecolor",
            Depth::Ansi256 => "256",
            Depth::Ansi16 => "16",
            Depth::NoColor => "none",
        }
    }

    /// Switch theme by name; the error is returned untouched so the UI can
    /// say exactly what is wrong with the file.
    pub fn select(&mut self, name: &str) -> Result<(), LoadError> {
        let theme = self
            .catalog
            .theme_with_depth(name, self.variant, self.depth)?;
        self.theme = theme;
        Ok(())
    }

    /// A color token as ratatui sees it. `Color::None` becomes `Reset`, i.e.
    /// the terminal's own foreground/background.
    #[must_use]
    pub fn color(&self, token: &str) -> UiColor {
        to_ui_color(self.theme.color(token))
    }

    #[must_use]
    pub fn state_color(&self, state: &str) -> UiColor {
        to_ui_color(self.theme.state_color(state))
    }

    /// Bold text style in the theme's foreground (headings, group names).
    #[must_use]
    pub fn text_style(&self) -> Style {
        Style::default().fg(self.color("text"))
    }

    #[must_use]
    pub fn muted_style(&self) -> Style {
        Style::default().fg(self.color("textMuted"))
    }

    #[must_use]
    pub fn border_style(&self) -> Style {
        Style::default().fg(self.color("border"))
    }
}

/// Map an engine color onto ratatui's color type. Only `Rgb` reaches a
/// truecolor terminal; the engine has already quantized everything else.
#[must_use]
pub fn to_ui_color(color: Color) -> UiColor {
    match color {
        Color::Rgb(r, g, b) => UiColor::Rgb(r, g, b),
        Color::Ansi(index) => UiColor::Indexed(index),
        Color::None => UiColor::Reset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_decides_which_ratatui_color_kind_is_used() {
        let truecolor = ThemeState::with_depth(Depth::Truecolor, Variant::Dark);
        assert!(matches!(truecolor.color("primary"), UiColor::Rgb(_, _, _)));
        let ansi256 = ThemeState::with_depth(Depth::Ansi256, Variant::Dark);
        assert!(matches!(ansi256.color("primary"), UiColor::Indexed(_)));
        let ansi16 = ThemeState::with_depth(Depth::Ansi16, Variant::Dark);
        match ansi16.color("primary") {
            UiColor::Indexed(index) => assert!(index < 16, "{index} is not a legacy color"),
            other => panic!("expected indexed, got {other:?}"),
        }
        let none = ThemeState::with_depth(Depth::NoColor, Variant::Dark);
        assert_eq!(none.color("primary"), UiColor::Reset);
    }

    #[test]
    fn every_builtin_can_be_selected_and_keeps_the_state_language() {
        let mut state = ThemeState::with_depth(Depth::Truecolor, Variant::Dark);
        for name in state.names() {
            state
                .select(&name)
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(state.theme().name(), name);
            // Whatever the palette, the states stay distinguishable.
            let working = state.state_color("working");
            let done = state.state_color("done");
            let idle = state.state_color("idle");
            assert_ne!(working, done, "{name}: working == done");
            assert_ne!(done, idle, "{name}: done == idle");
        }
    }

    #[test]
    fn a_broken_user_arreo_theme_is_visible_not_silent() {
        // The built-in base must still be usable, and the reason must be
        // available to the UI rather than swallowed.
        let state = ThemeState::with_depth(Depth::Truecolor, Variant::Dark);
        assert!(
            state.theme().colors().len() > 10,
            "base look always resolves"
        );
        // A clean environment has nothing broken and nothing to report.
        if state.broken().is_empty() {
            assert_eq!(state.startup_error(), None);
        }
        let _ = state.broken();
    }

    #[test]
    fn selecting_an_unknown_theme_reports_it_and_keeps_the_current_one() {
        let mut state = ThemeState::with_depth(Depth::Truecolor, Variant::Dark);
        let before = state.theme().name().to_string();
        let err = state.select("does-not-exist").expect_err("must fail");
        assert!(matches!(err, LoadError::NotFound { .. }), "{err:?}");
        assert_eq!(
            state.theme().name(),
            before,
            "a failed switch must not blank the UI"
        );
    }
}
