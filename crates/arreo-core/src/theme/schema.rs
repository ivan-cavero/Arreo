//! Theme file schema (T-0016): the opencode-compatible JSON shape.
//!
//! One sentence: `defs` holds reusable colors, `theme` maps semantic tokens
//! to a color (or to `{"dark": …, "light": …}`), and nothing is assumed —
//! unknown tokens and dangling references are load errors, never silence.

use crate::theme::color::{Color, ColorError};
use serde::Deserialize;
use std::collections::BTreeMap;

/// Every token an opencode-shaped theme may define, plus Arreo's own state
/// tokens (§3.12: state colors are the product's visual language).
pub const COLOR_TOKENS: &[&str] = &[
    // Core semantics.
    "primary",
    "secondary",
    "accent",
    "error",
    "warning",
    "success",
    "info",
    "text",
    "textMuted",
    "selectedListItemText",
    "background",
    "backgroundPanel",
    "backgroundElement",
    "backgroundMenu",
    "border",
    "borderActive",
    "borderSubtle",
    // Diffs.
    "diffAdded",
    "diffRemoved",
    "diffContext",
    "diffHunkHeader",
    "diffHighlightAdded",
    "diffHighlightRemoved",
    "diffAddedBg",
    "diffRemovedBg",
    "diffContextBg",
    "diffLineNumber",
    "diffAddedLineNumberBg",
    "diffRemovedLineNumberBg",
    // Markdown.
    "markdownText",
    "markdownHeading",
    "markdownLink",
    "markdownLinkText",
    "markdownCode",
    "markdownBlockQuote",
    "markdownEmph",
    "markdownStrong",
    "markdownHorizontalRule",
    "markdownListItem",
    "markdownListEnumeration",
    "markdownImage",
    "markdownImageText",
    "markdownCodeBlock",
    // Syntax.
    "syntaxComment",
    "syntaxKeyword",
    "syntaxFunction",
    "syntaxVariable",
    "syntaxString",
    "syntaxNumber",
    "syntaxType",
    "syntaxOperator",
    "syntaxPunctuation",
    // Arreo: the state board (TUI sidebar, status board, phone).
    "working",
    "blocked",
    "done",
    "idle",
    "question",
];

/// Non-color tokens (opencode carries these beside the colors).
pub const NUMERIC_TOKENS: &[&str] = &["thinkingOpacity"];

/// The two theme variants every token may carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Variant {
    #[default]
    Dark,
    Light,
}

impl Variant {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

/// A token value as written in the file: a bare value or variant pair.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum RawValue {
    /// One value for both variants (`"#7dcfff"`, `"none"`, `"13"`).
    Plain(String),
    /// Explicit variants — the shape opencode themes use.
    Variants {
        dark: Option<String>,
        light: Option<String>,
    },
}

impl RawValue {
    /// The string for `variant`, falling back to the other variant when only
    /// one side is specified (a dark-only token still renders in light mode
    /// rather than vanishing).
    #[must_use]
    pub fn get(&self, variant: Variant) -> Option<&str> {
        match self {
            Self::Plain(value) => Some(value.as_str()),
            Self::Variants { dark, light } => match variant {
                Variant::Dark => dark.as_deref().or(light.as_deref()),
                Variant::Light => light.as_deref().or(dark.as_deref()),
            },
        }
    }
}

/// The on-disk theme file.
#[derive(Debug, Clone, Deserialize)]
pub struct RawTheme {
    /// `"$schema"` — accepted and ignored (it is a declaration, not data).
    #[serde(rename = "$schema", default)]
    pub schema: Option<String>,
    #[serde(default)]
    pub defs: BTreeMap<String, String>,
    #[serde(default)]
    pub theme: BTreeMap<String, RawValue>,
}

/// A theme file that failed to load, with the reason a human needs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    #[error("theme {theme:?} is not valid JSON: {detail}")]
    Json { theme: String, detail: String },
    #[error("theme {theme:?} could not be read: {detail}")]
    Unreadable { theme: String, detail: String },
    #[error("theme {theme:?} defines unknown token {token:?} (known: {hint})")]
    UnknownToken {
        theme: String,
        token: String,
        hint: String,
    },
    #[error("theme {theme:?} token {token:?} refers to undefined def {reference:?}")]
    UnknownDef {
        theme: String,
        token: String,
        reference: String,
    },
    #[error("theme {theme:?} token {token:?} is invalid: {source}")]
    BadColor {
        theme: String,
        token: String,
        source: ColorError,
    },
    #[error("theme {theme:?} token {token:?} has an unexpected value {value:?}")]
    BadValue {
        theme: String,
        token: String,
        value: String,
    },
    #[error("theme {theme:?} has no variant data for {variant} variant")]
    MissingVariant { theme: String, variant: String },
}

/// Validate the token names, then resolve one variant of this theme.
///
/// Resolution order per token: a `defs` reference, then a literal color.
/// `defs` are themselves validated (a def nobody uses is still a typo to fix,
/// and a def that is not a color breaks every theme that references it).
pub fn resolve(
    name: &str,
    raw: &RawTheme,
    variant: Variant,
) -> Result<BTreeMap<String, Color>, SchemaError> {
    let mut colors = BTreeMap::new();
    for (key, value) in &raw.theme {
        if NUMERIC_TOKENS.contains(&key.as_str()) {
            // Present in real opencode themes; not a color. Validated for
            // shape, kept out of the color map.
            let candidate = value.get(variant).unwrap_or_default();
            if candidate.parse::<f32>().is_err() {
                return Err(SchemaError::BadValue {
                    theme: name.to_string(),
                    token: key.clone(),
                    value: candidate.to_string(),
                });
            }
            continue;
        }
        if !COLOR_TOKENS.contains(&key.as_str()) {
            return Err(SchemaError::UnknownToken {
                theme: name.to_string(),
                token: key.clone(),
                hint: nearest_token(key),
            });
        }
        let Some(candidate) = value.get(variant) else {
            return Err(SchemaError::MissingVariant {
                theme: name.to_string(),
                variant: variant.as_str().to_string(),
            });
        };
        // A value that is neither a defined def nor a literal color is almost
        // always a typo: report the missing def by name instead of the less
        // useful "malformed color".
        if !raw.defs.contains_key(candidate)
            && Color::parse(candidate).is_err()
            && looks_like_reference(candidate)
        {
            return Err(SchemaError::UnknownDef {
                theme: name.to_string(),
                token: key.clone(),
                reference: candidate.to_string(),
            });
        }
        let color = match raw.defs.get(candidate) {
            Some(def) => Color::parse(def).map_err(|source| SchemaError::BadColor {
                theme: name.to_string(),
                token: key.clone(),
                source,
            })?,
            None => Color::parse(candidate).map_err(|source| SchemaError::BadColor {
                theme: name.to_string(),
                token: key.clone(),
                source,
            })?,
        };
        colors.insert(key.clone(), color);
    }
    if colors.is_empty() {
        return Err(SchemaError::MissingVariant {
            theme: name.to_string(),
            variant: variant.as_str().to_string(),
        });
    }
    Ok(colors)
}

/// A def reference is a bare word (`darkStep9`); a literal is a hex color,
/// an index, or `none`.
fn looks_like_reference(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.starts_with('#')
        && !trimmed.eq_ignore_ascii_case("none")
        && trimmed.parse::<u16>().is_err()
}

/// Closest known token by edit distance — a typo should cost one line, not a
/// trip to the docs.
fn nearest_token(token: &str) -> String {
    let mut best = None;
    let mut best_distance = usize::MAX;
    for candidate in COLOR_TOKENS.iter().chain(NUMERIC_TOKENS) {
        let d = edit_distance(&token.to_lowercase(), &candidate.to_lowercase());
        if d < best_distance {
            best_distance = d;
            best = Some(*candidate);
        }
    }
    match best {
        Some(candidate) if best_distance <= 4 => {
            format!("did you mean {candidate:?}? open the TUI theme picker for the full set")
        }
        _ => "run the TUI theme picker for the full set".to_string(),
    }
}

fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            current[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(current[j] + 1);
        }
        std::mem::swap(&mut prev, &mut current);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme_from(json: &str) -> RawTheme {
        serde_json::from_str(json).expect("test JSON parses")
    }

    #[test]
    fn resolves_defs_and_variants() {
        let raw = theme_from(
            r##"{
                "defs": { "ink": "#7dcfff", "bg": "#0e1116" },
                "theme": {
                    "primary": { "dark": "ink", "light": "#1f6feb" },
                    "background": "bg",
                    "border": "none"
                }
            }"##,
        );
        let dark = resolve("t", &raw, Variant::Dark).expect("dark resolves");
        assert_eq!(dark["primary"], Color::Rgb(0x7d, 0xcf, 0xff));
        assert_eq!(dark["background"], Color::Rgb(0x0e, 0x11, 0x16));
        assert_eq!(dark["border"], Color::None);
        let light = resolve("t", &raw, Variant::Light).expect("light resolves");
        assert_eq!(light["primary"], Color::Rgb(0x1f, 0x6f, 0xeb));
        // A dark-only token still has a light value (no vanishing tokens).
        assert_eq!(light["background"], Color::Rgb(0x0e, 0x11, 0x16));
    }

    #[test]
    fn unknown_token_is_an_error_with_a_hint() {
        let raw = theme_from(r##"{ "theme": { "primry": "#fff" } }"##);
        match resolve("t", &raw, Variant::Dark) {
            Err(SchemaError::UnknownToken { hint, token, .. }) => {
                assert_eq!(token, "primry");
                assert!(hint.contains("primary"), "{hint}");
            }
            other => panic!("expected UnknownToken, got {other:?}"),
        }
    }

    #[test]
    fn dangling_def_reference_names_the_missing_def() {
        let raw = theme_from(r##"{ "defs": { "ink": "#fff" }, "theme": { "primary": "nope" } }"##);
        match resolve("t", &raw, Variant::Dark) {
            Err(SchemaError::UnknownDef { reference, .. }) => assert_eq!(reference, "nope"),
            other => panic!("expected UnknownDef, got {other:?}"),
        }
    }

    #[test]
    fn bad_color_reports_the_token() {
        let raw = theme_from(r##"{ "theme": { "primary": "#zzz" } }"##);
        match resolve("t", &raw, Variant::Dark) {
            Err(SchemaError::BadColor { token, .. }) => assert_eq!(token, "primary"),
            other => panic!("expected BadColor, got {other:?}"),
        }
    }

    #[test]
    fn opacity_token_is_accepted_but_kept_out_of_the_palette() {
        let raw = theme_from(
            r##"{ "theme": { "primary": "#fff", "thinkingOpacity": { "dark": "0.5" } } }"##,
        );
        let colors = resolve("t", &raw, Variant::Dark).expect("resolves");
        assert_eq!(colors.len(), 1);
        let bad = theme_from(r##"{ "theme": { "thinkingOpacity": { "dark": "opaque" } } }"##);
        assert!(matches!(
            resolve("t", &bad, Variant::Dark),
            Err(SchemaError::BadValue { .. })
        ));
    }

    #[test]
    fn an_empty_theme_file_is_rejected() {
        let raw = theme_from("{}");
        assert!(resolve("t", &raw, Variant::Dark).is_err());
    }
}
