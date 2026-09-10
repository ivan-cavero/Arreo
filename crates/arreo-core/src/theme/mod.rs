//! Theming engine (T-0016): opencode-compatible JSON themes, capability
//! fallback, and one token table shared by every surface.
//!
//! One sentence: a theme resolves `defs` + semantic tokens into quantized
//! colors, and the TUI, the CLI and the reference HTML all read that same
//! table — so the sidebar and the docs cannot drift apart.
//!
//! Layout: [`color`] owns the color model and terminal capability detection,
//! [`schema`] owns the file format and its validation, [`loader`] finds and
//! merges theme files (built-in → user → project → cwd), and [`Theme`] is
//! what the UI consumes.

pub mod color;
pub mod loader;
pub mod schema;

pub use color::{Color, ColorError, Depth};
pub use loader::{default_dirs, Catalog, LoadError};
pub use schema::{SchemaError, Variant, COLOR_TOKENS, NUMERIC_TOKENS};

use std::collections::BTreeMap;

/// The shell an under-specified theme falls back to when a token is absent.
pub const BASE_THEME: &str = "arreo";

/// A resolved, capability-quantized theme: what the UI reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    name: String,
    variant: Variant,
    depth: Depth,
    colors: BTreeMap<String, Color>,
}

impl Theme {
    /// Build a theme from already-resolved token colors (the loader's path).
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        variant: Variant,
        depth: Depth,
        colors: BTreeMap<String, Color>,
    ) -> Self {
        Self {
            name: name.into(),
            variant,
            depth,
            colors,
        }
    }

    /// The `arreo` built-in at the given depth — the base every theme starts
    /// from, and the look that ships in the binary.
    #[must_use]
    pub fn arreo(depth: Depth) -> Self {
        let catalog = Catalog::builtin();
        catalog
            .theme_with_depth(BASE_THEME, Variant::Dark, depth)
            .expect("the arreo built-in always loads")
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn variant(&self) -> Variant {
        self.variant
    }

    #[must_use]
    pub fn depth(&self) -> Depth {
        self.depth
    }

    /// Every resolved token (the shared table the HTML reference renders).
    #[must_use]
    pub fn colors(&self) -> &BTreeMap<String, Color> {
        &self.colors
    }

    /// A token's color. Tokens are validated at load, so an unknown name here
    /// is a programmer error: it yields `None` (terminal default) rather than
    /// inventing a color.
    #[must_use]
    pub fn color(&self, token: &str) -> Color {
        match self.colors.get(token) {
            Some(color) => color.quantize(self.depth),
            None => Color::None,
        }
    }

    /// The state board's language: the same hues on the TUI sidebar, the
    /// status board and (later) the phone.
    #[must_use]
    pub fn state_color(&self, state: &str) -> Color {
        match state {
            "working" => self.color("working"),
            "blocked" => self.color("blocked"),
            "done" => self.color("done"),
            "idle" => self.color("idle"),
            "question" => self.color("question"),
            _ => self.color("textMuted"),
        }
    }

    /// Re-quantize for a different terminal (a theme is data; depth is not
    /// baked into the file).
    #[must_use]
    pub fn with_depth(&self, depth: Depth) -> Self {
        Self {
            depth,
            ..self.clone()
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::arreo(Depth::Truecolor)
    }
}

/// Render the theme as a standalone HTML reference from the same token table
/// the TUI reads. Used by `xtask e2e --slice theme` (shared-token check) and
/// by the docs build.
#[must_use]
pub fn reference_html(theme: &Theme) -> String {
    let mut swatches = String::new();
    for (token, color) in theme.colors() {
        let quantized = color.quantize(theme.depth());
        let (css, label) = match quantized {
            Color::Rgb(r, g, b) => (
                format!("#{r:02x}{g:02x}{b:02x}"),
                format!("#{r:02x}{g:02x}{b:02x}"),
            ),
            Color::Ansi(index) => (format!("var(--ansi-{index})"), format!("ansi {index}")),
            Color::None => ("transparent".to_string(), "none".to_string()),
        };
        swatches.push_str(&format!(
            "    <figure data-token=\"{token}\" data-color=\"{label}\">\n      \
             <div class=\"chip\" style=\"background:{css}\"></div>\n      \
             <figcaption><code>{token}</code><span>{label}</span></figcaption>\n    </figure>\n"
        ));
    }
    // The ANSI palette variables are declared so `var(--ansi-N)` swatches
    // render even in a browser with no terminal palette.
    let ansi_vars: String = (0..16)
        .map(|i| format!("  --ansi-{i}: {};\n", ansi_css(i)))
        .collect();
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>arreo theme: {name} ({variant})</title>\n<style>\n\
         :root {{\n{ansi_vars}}}\n\
         body {{ background: {bg}; color: {fg}; font-family: ui-monospace, monospace; margin: 2rem; }}\n\
         .grid {{ display: grid; grid-template-columns: repeat(auto-fill, minmax(11rem, 1fr)); gap: .75rem; }}\n\
         figure {{ margin: 0; }}\n\
         .chip {{ height: 2.5rem; border: 1px solid {border}; border-radius: 4px; }}\n\
         figcaption {{ display: flex; justify-content: space-between; font-size: .8rem; margin-top: .25rem; }}\n\
         </style>\n</head>\n<body data-theme=\"{name}\" data-variant=\"{variant}\" data-depth=\"{depth}\">\n\
         <h1>arreo theme: {name}</h1>\n\
         <p>variant <code>{variant}</code> · depth <code>{depth}</code> · \
         {count} tokens (same table the TUI renders)</p>\n\
         <div class=\"grid\">\n{swatches}</div>\n</body>\n</html>\n",
        name = theme.name,
        variant = theme.variant.as_str(),
        depth = depth_name(theme.depth),
        count = theme.colors.len(),
        bg = css_color(theme.color("background")),
        fg = css_color(theme.color("text")),
        border = css_color(theme.color("border")),
    )
}

fn depth_name(depth: Depth) -> &'static str {
    match depth {
        Depth::Truecolor => "truecolor",
        Depth::Ansi256 => "256",
        Depth::Ansi16 => "16",
        Depth::NoColor => "none",
    }
}

fn css_color(color: Color) -> String {
    match color {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Ansi(index) => format!("var(--ansi-{index})"),
        Color::None => "transparent".to_string(),
    }
}

/// The classic xterm palette, for HTML swatches of ANSI-indexed tokens.
fn ansi_css(index: u8) -> &'static str {
    const TABLE: [&str; 16] = [
        "#000000", "#800000", "#008000", "#808000", "#000080", "#800080", "#008080", "#c0c0c0",
        "#808080", "#ff0000", "#00ff00", "#ffff00", "#0000ff", "#ff00ff", "#00ffff", "#ffffff",
    ];
    TABLE[(index as usize) % 16]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arreo_theme_defines_the_state_language() {
        let theme = Theme::arreo(Depth::Truecolor);
        assert_eq!(theme.color("primary"), Color::Rgb(0x7d, 0xcf, 0xff));
        assert_eq!(theme.state_color("question"), theme.color("question"));
        assert_eq!(theme.state_color("working"), Color::Rgb(0x7d, 0xcf, 0xff));
        assert_eq!(theme.state_color("done"), Color::Rgb(0x9e, 0xce, 0x6a));
        assert_eq!(theme.state_color("blocked"), Color::Rgb(0xe0, 0xaf, 0x68));
        // An unknown state is muted, not a panic.
        assert_eq!(theme.state_color("nonsense"), theme.color("textMuted"));
    }

    #[test]
    fn quantization_is_applied_at_read_time() {
        let theme = Theme::arreo(Depth::Ansi256);
        assert!(matches!(theme.color("primary"), Color::Ansi(_)));
        let truecolor = theme.with_depth(Depth::Truecolor);
        assert_eq!(truecolor.color("primary"), Color::Rgb(0x7d, 0xcf, 0xff));
        let no_color = theme.with_depth(Depth::NoColor);
        assert_eq!(no_color.color("primary"), Color::None);
    }

    #[test]
    fn reference_html_carries_the_same_tokens() {
        let theme = Theme::arreo(Depth::Truecolor);
        let html = reference_html(&theme);
        assert!(html.contains("data-theme=\"arreo\""));
        assert!(html.contains("data-token=\"primary\" data-color=\"#7dcfff\""));
        for token in theme.colors().keys() {
            assert!(
                html.contains(&format!("data-token=\"{token}\"")),
                "{token} missing"
            );
        }
        // The 256-color quantization shows up in the HTML too (shared table).
        let quantized = reference_html(&Theme::arreo(Depth::Ansi256));
        assert!(
            !quantized.contains("#7dcfff"),
            "truecolor leaked into 256 HTML"
        );
        assert!(quantized.contains("ansi "), "no quantized labels in HTML");
    }
}
