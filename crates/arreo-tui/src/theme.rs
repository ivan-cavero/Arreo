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
}

impl Default for ThemeState {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemeState {
    /// Load the catalog from the standard hierarchy and select the `arreo`
    /// built-in at the detected terminal depth. A catalog that cannot even
    /// list its directories still has the built-ins, so this only fails if a
    /// user theme shadows `arreo` with something broken — and then the error
    /// is worth showing instead of hiding.
    #[must_use]
    pub fn new() -> Self {
        Self::with_depth(Depth::detect(), Variant::Dark)
    }

    #[must_use]
    pub fn with_depth(depth: Depth, variant: Variant) -> Self {
        let catalog =
            Catalog::discover(&Catalog::default_dirs()).unwrap_or_else(|_| Catalog::builtin());
        let theme = catalog
            .theme_with_depth("arreo", variant, depth)
            .unwrap_or_else(|_| Theme::arreo(depth));
        Self {
            catalog,
            theme,
            variant,
            depth,
        }
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
