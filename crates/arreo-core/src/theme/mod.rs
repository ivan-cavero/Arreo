//! Theming engine (T-0016): opencode-compatible JSON themes, capability
//! fallback, and one token table shared by every surface.
//!
//! One sentence: a theme resolves `defs` + semantic tokens into quantized
//! colors, and the TUI, the CLI and the reference HTML all read that same
//! table — so the sidebar and the docs cannot drift apart.
//!
//! Layout: [`color`] owns the color model and terminal capability detection,
//! [`schema`] owns the file format and its validation, [`loader`] finds and
//! merges theme files (built-in → user → project → cwd), [`Theme`] is what the
//! UI consumes, and [`ThemeTokens`] is the resolved table that crosses the wire
//! so a surface with no theme directory renders the machine's theme (T-0116).

pub mod brand;
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
        Self::arreo_variant(Variant::Dark, depth)
    }

    /// The same, in an explicit variant (the brand palette carries both from
    /// BRAND §2's derivation rule).
    #[must_use]
    pub fn arreo_variant(variant: Variant, depth: Depth) -> Self {
        let catalog = Catalog::builtin();
        catalog
            .theme_with_depth(BASE_THEME, variant, depth)
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
            // BRAND §2 lists `error` as a state color, so the board's language
            // includes it: an error message is a state, and painting it muted
            // would be the same gray as `unknown`.
            "error" => self.color("error"),
            _ => self.color("textMuted"),
        }
    }

    /// The color a state's **label** is painted in (T-0076): the state's own
    /// hue when that hue carries WCAG AA as text on this theme's background,
    /// `textMuted` when it does not.
    ///
    /// A state dot may be any hue the brand likes — it is a graphic, judged at
    /// 3:1 (§1.4.11). A state *label* is text, judged at 4.5:1, and one state
    /// hue (BRAND §2's muted `idle`, 3.5:1 on the dark page) cannot carry it.
    /// Rather than repaint the brand's color, the label steps to the neutral
    /// that can: the state stays readable and the dot keeps its meaning.
    #[must_use]
    pub fn state_label_color(&self, state: &str) -> Color {
        let own = self.state_color(state);
        match own.contrast_ratio(self.color("background")) {
            Some(ratio) if ratio < brand::AA_TEXT => self.color("textMuted"),
            _ => own,
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

/// One theme's resolved token map: the shape that crosses the wire (T-0116).
///
/// **Resolved, not raw, and that is the whole point.** The theme engine has two
/// JSON shapes and neither is what a client should receive. `schema::RawTheme`
/// is the *on-disk* file: `defs` (named color references), per-token per-variant
/// values, and no serializer at all. `brand` parses the brand document, which is
/// a design artifact rather than a user theme. Sending either would push the
/// `defs` resolution, the token-name validation and the variant unwrapping onto
/// every surface — a phone and a browser would each carry the resolver, and each
/// would be a place the resolution could diverge, which is the opposite of "one
/// theme, every surface". `schema::resolve` already produces exactly the flat
/// table `Theme::new` takes, so **that table is what travels**: a surface builds
/// its theme from the reply with no resolver of its own.
///
/// **Depth is not on the wire.** The tokens are the theme file's own colors,
/// unquantized; [`ThemeTokens::to_theme`] is where a surface applies *its*
/// capability. That is what makes one document render correctly on a 16-colour
/// terminal and a truecolor one (T-0016's property, asserted at both depths) —
/// a document quantized at the sender would show the 16-colour approximation on
/// the truecolor surface, one theme with two looks.
///
/// Each color travels as the spelling the theme files already use
/// (`#rrggbb`, a palette index, or `none`), which `Color::parse` reads back
/// exactly — so the wire is inspectable by a human and by `arreo theme export`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ThemeTokens {
    pub name: String,
    pub variant: Variant,
    /// `token -> color`, every value a literal: `Catalog::tokens` resolves the
    /// file before anything is sent.
    pub tokens: BTreeMap<String, String>,
}

impl ThemeTokens {
    /// An already-resolved theme's tokens, as the wire spells them.
    #[must_use]
    pub fn from_theme(theme: &Theme) -> Self {
        Self {
            name: theme.name.clone(),
            variant: theme.variant,
            tokens: theme
                .colors
                .iter()
                .map(|(token, color)| (token.clone(), color.to_string()))
                .collect(),
        }
    }

    /// The theme these tokens describe, quantized for `depth` — the client half
    /// of the verb.
    ///
    /// No token-name validation happens here, deliberately: the server resolved
    /// and validated the file, and a client re-validating would be a second
    /// place the rule could live. What *is* checked is the one thing a client
    /// can check on its own: that each value is a color. A `defs` reference
    /// arriving here (which is what a raw document would produce) is refused by
    /// name rather than painted as a guess.
    pub fn to_theme(&self, depth: Depth) -> Result<Theme, TokenError> {
        let mut colors = BTreeMap::new();
        for (token, value) in &self.tokens {
            let color = Color::parse(value).map_err(|_| TokenError::BadColor {
                theme: self.name.clone(),
                token: token.clone(),
                value: value.clone(),
            })?;
            colors.insert(token.clone(), color);
        }
        Ok(Theme::new(self.name.clone(), self.variant, depth, colors))
    }

    /// One token's color as the wire spells it.
    #[must_use]
    pub fn color(&self, token: &str) -> Option<&str> {
        self.tokens.get(token).map(String::as_str)
    }
}

/// A received token map that cannot become a theme.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("theme {theme:?} token {token:?} is not a color: {value:?}")]
    BadColor {
        theme: String,
        token: String,
        value: String,
    },
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
        assert_eq!(theme.color("primary"), Color::Rgb(0x6f, 0xd3, 0xe8));
        assert_eq!(theme.state_color("question"), theme.color("question"));
        assert_eq!(theme.state_color("working"), Color::Rgb(0x6f, 0xd3, 0xe8));
        assert_eq!(theme.state_color("question"), Color::Rgb(0xe8, 0xb4, 0x5a));
        assert_eq!(theme.state_color("done"), Color::Rgb(0x8f, 0xd1, 0x9e));
        assert_eq!(theme.state_color("blocked"), Color::Rgb(0xff, 0x9e, 0x64));
        // An unknown state is muted, not a panic.
        assert_eq!(theme.state_color("nonsense"), theme.color("textMuted"));
        // Every state BRAND §2 names is a distinct hue — the state colors are
        // the product's language, so two of them sharing a value would make
        // the board unreadable at a glance.
        let states = ["working", "question", "blocked", "done", "error", "idle"];
        for (i, a) in states.iter().enumerate() {
            for b in &states[i + 1..] {
                assert_ne!(theme.state_color(a), theme.state_color(b), "{a} == {b}");
            }
        }
    }

    #[test]
    fn quantization_is_applied_at_read_time() {
        let theme = Theme::arreo(Depth::Ansi256);
        assert!(matches!(theme.color("primary"), Color::Ansi(_)));
        let truecolor = theme.with_depth(Depth::Truecolor);
        assert_eq!(truecolor.color("primary"), Color::Rgb(0x6f, 0xd3, 0xe8));
        let no_color = theme.with_depth(Depth::NoColor);
        assert_eq!(no_color.color("primary"), Color::None);
    }

    /// **The wire carries the resolution, not the document** (T-0116).
    ///
    /// `RawTheme` is the on-disk shape: `defs` plus per-token, per-variant
    /// values that may be *references* into `defs`. A client handed that would
    /// need the resolver, the token-name validation and the variant unwrapping
    /// — three places per surface for the resolution to diverge, which is the
    /// opposite of one theme on every surface. What [`Catalog::tokens`]
    /// produces is the flat table `Theme::new` already takes, so a client
    /// builds its theme from the reply with no resolver at all.
    #[test]
    fn the_wire_carries_resolved_tokens_not_the_document() {
        let tokens = Catalog::builtin()
            .tokens("arreo", Variant::Dark)
            .expect("arreo resolves");
        let raw: crate::theme::schema::RawTheme =
            serde_json::from_str(include_str!("../../themes/arreo.json")).expect("built-in JSON");
        assert!(
            !raw.defs.is_empty(),
            "the fixture proves nothing without defs"
        );
        // Every value is a literal color a client can parse — no references.
        for (token, value) in &tokens.tokens {
            Color::parse(value).unwrap_or_else(|e| panic!("{token}={value:?}: {e}"));
        }
        // No `defs` name crossed the wire: the resolution happened here.
        for def in raw.defs.keys() {
            assert!(
                !tokens.tokens.values().any(|value| value == def),
                "def {def:?} travelled unresolved"
            );
        }
        // And it is the file's own resolution, not a re-derived approximation.
        assert_eq!(tokens.color("question"), Some("#e8b45a"));
    }

    /// **One document, both depths** (T-0116: T-0016's property over the wire).
    ///
    /// The tokens are the theme file's own colors and the *receiving surface*
    /// applies its depth, so the same received document is correct on a
    /// 16-colour terminal and on a truecolor one. A server that quantized
    /// before sending would make the truecolor surface show the 16-colour
    /// approximation — one theme, two looks — and this test red.
    #[test]
    fn one_received_document_renders_at_both_depths() {
        let tokens = Catalog::builtin()
            .tokens("arreo", Variant::Dark)
            .expect("arreo resolves");
        let authored = Color::Rgb(0xe8, 0xb4, 0x5a);
        assert_eq!(
            tokens.color("question"),
            Some("#e8b45a"),
            "the wire carries the authored color, unquantized"
        );

        let truecolor = tokens.to_theme(Depth::Truecolor).expect("client builds");
        assert_eq!(truecolor.color("question"), authored);

        let sixteen = tokens.to_theme(Depth::Ansi16).expect("client builds");
        let Color::Ansi(index) = sixteen.color("question") else {
            panic!(
                "a 16-colour client must get a palette index, got {:?}",
                sixteen.color("question")
            );
        };
        assert!(index < 16, "outside the terminal's own palette: {index}");
        assert_eq!(sixteen.color("question"), authored.quantize(Depth::Ansi16));

        // Depth is the surface's, never the document's: the same reply holds
        // the same colors on both.
        assert_eq!(
            truecolor.colors(),
            sixteen.colors(),
            "depth must not change the received document"
        );
    }

    /// A token value that is not a color is refused by the **client**, naming
    /// the token — which is exactly what a raw document would produce: a `defs`
    /// reference is a bare word, and a client with no resolver must say so
    /// rather than paint a guess.
    #[test]
    fn a_received_token_that_is_not_a_color_is_refused() {
        let tokens = ThemeTokens {
            name: "half".to_string(),
            variant: Variant::Dark,
            tokens: BTreeMap::from([
                ("question".to_string(), "darkQuestion".to_string()),
                ("text".to_string(), "#ffffff".to_string()),
            ]),
        };
        let error = tokens
            .to_theme(Depth::Truecolor)
            .expect_err("a def name is not a color");
        assert!(error.to_string().contains("darkQuestion"), "{error}");
        assert!(error.to_string().contains("question"), "{error}");
    }

    #[test]
    fn a_theme_round_trips_through_the_wire_shape() {
        let theme = Theme::arreo(Depth::Truecolor);
        let tokens = ThemeTokens::from_theme(&theme);
        assert_eq!(tokens.name, "arreo");
        assert_eq!(tokens.variant, Variant::Dark);
        assert_eq!(tokens.tokens.len(), theme.colors().len());
        let rebuilt = tokens.to_theme(Depth::Truecolor).expect("rebuild");
        assert_eq!(rebuilt.colors(), theme.colors());
        assert_eq!(rebuilt.color("primary"), theme.color("primary"));
        assert_eq!(rebuilt.name(), theme.name());
    }

    #[test]
    fn reference_html_carries_the_same_tokens() {
        let theme = Theme::arreo(Depth::Truecolor);
        let html = reference_html(&theme);
        assert!(html.contains("data-theme=\"arreo\""));
        assert!(html.contains("data-token=\"primary\" data-color=\"#6fd3e8\""));
        for token in theme.colors().keys() {
            assert!(
                html.contains(&format!("data-token=\"{token}\"")),
                "{token} missing"
            );
        }
        // The 256-color quantization shows up in the HTML too (shared table).
        let quantized = reference_html(&Theme::arreo(Depth::Ansi256));
        assert!(
            !quantized.contains("#6fd3e8"),
            "truecolor leaked into 256 HTML"
        );
        assert!(quantized.contains("ansi "), "no quantized labels in HTML");
    }
}
