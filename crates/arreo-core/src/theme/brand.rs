//! `design/BRAND.md`, machine-checked (T-0076).
//!
//! One sentence: the brand document is the source of truth for the palette, so
//! the built-in `arreo` theme is compared *against the document* instead of
//! against a copy of it that would rot on the next edit.
//!
//! This module holds the two things that make that possible: a parser for §2's
//! token table (and for the light-variant rule in the same section), and the
//! correspondence between the document's token names and the theme schema's.
//! It is deliberately small and free of policy — the assertions live in the
//! tests, so a reader can see exactly what is enforced and what is not.
//!
//! The document is the *user's*; when the two disagree, the fix is either in
//! `themes/arreo.json` or in a conversation about the document — never an edit
//! here that makes the test agree with the code.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// The document, relative to this crate's manifest.
pub const BRAND_DOC: &str = "design/BRAND.md";

/// §2's palette table heading, as written in the document.
const PALETTE_HEADING: &str = "## 2. Palette";

/// The sentence in §2 that derives the light variant.
const LIGHT_RULE: &str = "light variants derived";

/// BRAND §2 token name → theme token name, one entry per row of the palette
/// table. Every §2 row must appear here (asserted below), and every theme
/// token here must exist in [`super::schema::COLOR_TOKENS`].
pub const TOKEN_MAP: &[(&str, &str)] = &[
    ("bg", "background"),
    ("bg-elevated", "backgroundPanel"),
    ("bg-inset", "backgroundElement"),
    ("border", "border"),
    ("border-active", "borderActive"),
    ("text", "text"),
    ("text-muted", "textMuted"),
    ("primary", "primary"),
    ("primary-strong", "primaryStrong"),
    ("accent-sand", "accent"),
    ("working", "working"),
    ("question", "question"),
    ("blocked", "blocked"),
    ("done", "done"),
    ("error", "error"),
    ("idle", "idle"),
];

/// BRAND §2's state tokens, i.e. the colors that are never decorative.
pub const STATE_TOKENS: &[&str] = &["working", "question", "blocked", "done", "error", "idle"];

/// The neutral/action tokens the UI paints *text* with (BRAND §2's `text`,
/// `text-muted`, `primary`, `primary-strong`, `accent-sand`).
pub const TEXT_TOKENS: &[&str] = &["text", "textMuted", "primary", "primaryStrong", "accent"];

/// WCAG AA for text (2.x §1.4.3).
pub const AA_TEXT: f64 = 4.5;

/// WCAG AA for non-text UI colors/graphics (§1.4.11) — what a state *dot* is.
pub const AA_NON_TEXT: f64 = 3.0;

/// BRAND §2's light-variant surfaces: the page and the paper panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightSurfaces {
    /// The light page background (the document's `bg` for light mode).
    pub background: String,
    /// The light elevated surface ("cream becomes paper").
    pub paper: String,
}

/// Where the brand document lives, from this crate's manifest (works from a
/// checkout; a published crate has no `design/` and every caller here is a
/// test or dev tooling).
#[must_use]
pub fn doc_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(BRAND_DOC)
}

/// Read the brand document from the checkout.
///
/// # Errors
/// Whatever the filesystem says — a missing document is a real failure for a
/// caller that is checking the palette against it.
pub fn read_doc() -> std::io::Result<String> {
    std::fs::read_to_string(doc_path())
}

/// §2's palette table: brand token → `#rrggbb`, lowercased.
///
/// Bold markers and backticks are stripped, cell notes (`(cyan)`) ignored, so
/// the table can be re-typeset without breaking the check. Rows whose value is
/// not a hex color are skipped rather than guessed at: §2's table is
/// colors-only, and a row that is not is a document change, not a palette one.
#[must_use]
pub fn palette(md: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(section) = section(md, PALETTE_HEADING) else {
        return out;
    };
    for line in section.lines() {
        let Some(cells) = table_row(line) else {
            continue;
        };
        let (Some(token), Some(value)) = (cells.first(), cells.get(1)) else {
            continue;
        };
        let token = clean(token);
        if token.is_empty() || token.eq_ignore_ascii_case("token") {
            continue;
        }
        if let Some(hex) = first_hex(value) {
            out.insert(token, hex);
        }
    }
    out
}

/// §2's light-variant rule: the two surfaces it names, in the order it names
/// them ("cream becomes paper `#…`, bg `#…`").
#[must_use]
pub fn light_surfaces(md: &str) -> Option<LightSurfaces> {
    let section = section(md, PALETTE_HEADING)?;
    let at = section.find(LIGHT_RULE)?;
    // The paragraph (or bullet) the sentence sits in: up to the next bullet or
    // blank line, so a later section's colors cannot be picked up by accident.
    let rest = &section[at + LIGHT_RULE.len()..];
    let end = [rest.find("\n\n"), rest.find("\n- "), rest.find("\n## ")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(rest.len());
    let mut hexes = hexes(&rest[..end]).into_iter();
    let paper = hexes.next()?;
    let background = hexes.next()?;
    Some(LightSurfaces { background, paper })
}

/// One markdown table row, split into trimmed cells (the leading `|` dropped).
fn table_row(line: &str) -> Option<Vec<&str>> {
    let line = line.trim();
    let body = line.strip_prefix('|')?;
    let cells: Vec<&str> = body.split('|').map(str::trim).collect();
    // `| --- | --- | --- |` is the header rule, not a row.
    if cells
        .iter()
        .all(|cell| cell.chars().all(|c| c == '-' || c == ':'))
    {
        return None;
    }
    Some(cells)
}

/// The text of a `## …` section, up to the next `## ` heading.
fn section<'a>(md: &'a str, heading: &str) -> Option<&'a str> {
    let start = md.find(heading)?;
    let rest = &md[start + heading.len()..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Strip the table's emphasis and code markers from a cell.
fn clean(cell: &str) -> String {
    cell.trim_matches(|c: char| c == '*' || c == '`' || c.is_whitespace())
        .trim()
        .to_string()
}

/// The first `#rrggbb` in a cell, lowercased.
fn first_hex(cell: &str) -> Option<String> {
    hexes(cell).into_iter().next()
}

/// Every `#rrggbb` in a string, in order.
fn hexes(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '#' && i + 6 < chars.len() {
            let digits: String = chars[i + 1..i + 7].iter().collect();
            if digits.chars().all(|c| c.is_ascii_hexdigit()) {
                out.push(format!("#{}", digits.to_ascii_lowercase()));
                i += 7;
                continue;
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::schema::{self, RawTheme, Variant};
    use crate::theme::{Color, Depth, Theme};

    fn doc() -> String {
        read_doc().unwrap_or_else(|e| {
            panic!(
                "the brand document is the source of truth and must be readable at {}: {e}",
                doc_path().display()
            )
        })
    }

    fn hex(color: Color) -> String {
        match color {
            Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
            other => panic!("expected a 24-bit brand color, got {other:?}"),
        }
    }

    fn theme(variant: Variant) -> Theme {
        Theme::arreo_variant(variant, Depth::Truecolor)
    }

    /// **Criterion 1.** The `arreo` built-in at truecolor depth is the brand
    /// document's §2 table, token for token. Editing either file without the
    /// other turns this red — which is the point: the document is the machine
    /// -checked source of truth, not a copy that rots.
    #[test]
    fn the_arreo_builtin_is_the_brand_palette() {
        let brand = palette(&doc());
        assert!(
            brand.len() >= TOKEN_MAP.len(),
            "parsed only {} §2 tokens: {brand:?}",
            brand.len()
        );
        let arreo = Theme::arreo(Depth::Truecolor);
        for (brand_token, theme_token) in TOKEN_MAP {
            let want = brand
                .get(*brand_token)
                .unwrap_or_else(|| panic!("BRAND §2 has no row for {brand_token:?}"));
            assert_eq!(
                &hex(arreo.color(theme_token)),
                want,
                "{theme_token} drifted from BRAND §2's {brand_token}"
            );
        }
        // Every §2 row is mapped: a new brand token cannot be silently ignored.
        for token in brand.keys() {
            assert!(
                TOKEN_MAP
                    .iter()
                    .any(|(brand_token, _)| brand_token == token),
                "BRAND §2 token {token:?} has no theme counterpart in TOKEN_MAP"
            );
        }
    }

    /// The shipped theme *is* `themes/arreo.json`: no second copy of the
    /// palette can hide in code and drift from the file a human edits.
    #[test]
    fn the_shipped_theme_is_the_theme_file() {
        let text = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("themes")
                .join("arreo.json"),
        )
        .expect("themes/arreo.json is part of the crate");
        let raw: RawTheme = serde_json::from_str(&text).expect("valid JSON");
        let from_file = schema::resolve("arreo", &raw, Variant::Dark).expect("resolves");
        assert_eq!(from_file, *Theme::arreo(Depth::Truecolor).colors());
    }

    /// **Criterion 1, the light half.** §2's derivation is a rule, not a
    /// second palette: the light surfaces are the ones the document names, and
    /// every state/accent shifts one step darker ([`Color::darkened`]) and
    /// still clears AA on both light surfaces.
    #[test]
    fn the_light_variant_follows_the_brand_derivation_rule() {
        let surfaces = light_surfaces(&doc()).expect("§2 states the light rule");
        let light = theme(Variant::Light);
        let dark = theme(Variant::Dark);
        assert_eq!(hex(light.color("background")), surfaces.background);
        assert_eq!(hex(light.color("backgroundPanel")), surfaces.paper);

        let page = light.color("background");
        let paper = light.color("backgroundPanel");
        for token in STATE_TOKENS.iter().chain(TEXT_TOKENS) {
            let d = dark.color(token);
            let l = light.color(token);
            assert!(
                l.relative_luminance() < d.relative_luminance(),
                "{token} did not shift darker for the light variant"
            );
            for surface in [page, paper] {
                let ratio = l.contrast_ratio(surface).expect("24-bit colors");
                assert!(
                    ratio >= AA_TEXT,
                    "{token} on {surface:?} is {ratio:.2}:1 in the light variant (< {AA_TEXT})"
                );
            }
        }
    }

    /// **Criterion 5.** Every text token and every state's *label* clears AA
    /// against both surfaces the sidebar paints on, in both variants — computed
    /// here, not eyeballed on a screenshot.
    #[test]
    fn text_and_state_labels_clear_wcag_aa() {
        for variant in [Variant::Dark, Variant::Light] {
            let theme = theme(variant);
            let surfaces = [theme.color("background"), theme.color("backgroundPanel")];
            for token in TEXT_TOKENS {
                for surface in surfaces {
                    let ratio = theme.color(token).contrast_ratio(surface).unwrap();
                    assert!(
                        ratio >= AA_TEXT,
                        "{} {token} on {surface:?} is {ratio:.2}:1",
                        variant.as_str()
                    );
                }
            }
            for state in STATE_TOKENS {
                let label = theme.state_label_color(state);
                for surface in surfaces {
                    let ratio = label.contrast_ratio(surface).unwrap();
                    assert!(
                        ratio >= AA_TEXT,
                        "{} {state}'s label on {surface:?} is {ratio:.2}:1",
                        variant.as_str()
                    );
                }
                // The dot is a graphic, not text: §1.4.11's 3:1 floor is what
                // it has to clear, and that is also what keeps a state visible
                // in the sidebar for someone who reads shapes, not hues.
                let dot = theme.state_color(state);
                for surface in surfaces {
                    let ratio = dot.contrast_ratio(surface).unwrap();
                    assert!(
                        ratio >= AA_NON_TEXT,
                        "{} {state}'s dot on {surface:?} is {ratio:.2}:1",
                        variant.as_str()
                    );
                }
            }
        }
    }

    /// Where a state's own hue cannot carry AA *text* (BRAND's muted `idle` is
    /// 3.5:1 on the dark page), the label is painted in `textMuted` while the
    /// dot keeps the state hue. Generic rule, so fixing the document later
    /// cannot make this test wrong.
    #[test]
    fn a_state_hue_that_cannot_carry_text_falls_back_to_muted() {
        let theme = theme(Variant::Dark);
        let background = theme.color("background");
        for state in STATE_TOKENS {
            let raw = theme.state_color(state);
            let label = theme.state_label_color(state);
            let raw_ratio = raw.contrast_ratio(background).unwrap();
            let label_ratio = label.contrast_ratio(background).unwrap();
            assert!(
                label_ratio >= AA_TEXT,
                "{state} label is {label_ratio:.2}:1"
            );
            if raw_ratio >= AA_TEXT {
                assert_eq!(label, raw, "{state} could keep its own hue as text");
            } else {
                assert_eq!(label, theme.color("textMuted"), "{state}");
            }
        }
    }

    /// The comparison is driven by the *document*, not by a copy of it: a
    /// drifted §2 row disagrees with the built-in whichever side moved. (The
    /// fixture edits the text in memory rather than the file — the document is
    /// the user's, and a test does not get to rewrite it.)
    #[test]
    fn drift_on_the_document_side_fails_the_comparison() {
        let doc = doc();
        let drifted = doc.replace("#E8B45A", "#E8B459");
        assert_ne!(
            drifted, doc,
            "the fixture must actually change the document"
        );
        let brand = palette(&drifted);
        let theme = Theme::arreo(Depth::Truecolor);
        let (brand_token, theme_token) = TOKEN_MAP
            .iter()
            .find(|(_, theme_token)| *theme_token == "question")
            .expect("question is mapped");
        assert_ne!(
            hex(theme.color(theme_token)),
            brand[*brand_token],
            "a drifted BRAND §2 row must disagree with the built-in"
        );
        assert_eq!(brand["question"], "#e8b459", "the parser read the drift");
    }

    /// The document's parse is not wishful: the hexes in §2 are the ones the
    /// table actually lists, so a re-typed table is caught here rather than
    /// silently passing through an empty map.
    #[test]
    fn the_document_parses_into_the_palette_it_claims() {
        let brand = palette(&doc());
        for (token, value) in [
            ("bg", "#121110"),
            ("text", "#ede7dc"),
            ("primary", "#6fd3e8"),
            ("accent-sand", "#d9c6a5"),
            ("question", "#e8b45a"),
            ("blocked", "#ff9e64"),
            ("done", "#8fd19e"),
            ("idle", "#6e6a64"),
        ] {
            assert_eq!(brand.get(token).map(String::as_str), Some(value), "{token}");
        }
        let surfaces = light_surfaces(&doc()).expect("§2 states the light rule");
        assert_eq!(surfaces.background, "#faf8f4");
        assert_eq!(surfaces.paper, "#f5f1e8");
    }

    /// Every mapped theme token is one the schema accepts, so the map cannot
    /// name a token a theme file is forbidden to define.
    #[test]
    fn every_mapped_token_exists_in_the_schema() {
        for (brand_token, theme_token) in TOKEN_MAP {
            assert!(
                schema::COLOR_TOKENS.contains(theme_token),
                "{brand_token} maps to unknown theme token {theme_token}"
            );
        }
    }

    /// `darkened` is the light variant's step; it must be a real step (never
    /// lighter) and leave `None`/indexed colors alone rather than inventing RGB.
    #[test]
    fn darkening_only_ever_darkens() {
        let base = Color::Rgb(0xff, 0x9e, 0x64);
        let stepped = base.darkened(0.5);
        assert_eq!(stepped, Color::Rgb(0x80, 0x4f, 0x32));
        assert!(stepped.relative_luminance() < base.relative_luminance());
        assert_eq!(base.darkened(0.0), base);
        assert_eq!(Color::Ansi(13).darkened(0.5), Color::Ansi(13));
        assert_eq!(Color::None.darkened(0.5), Color::None);
        assert_eq!(Color::Rgb(1, 2, 3).darkened(1.0), Color::Rgb(0, 0, 0));
    }

    /// The contrast maths itself: two anchors WCAG fixes by definition, so a
    /// broken luminance curve cannot quietly pass the palette checks.
    #[test]
    fn contrast_maths_matches_the_wcag_anchors() {
        let black = Color::Rgb(0, 0, 0);
        let white = Color::Rgb(0xff, 0xff, 0xff);
        assert!((black.contrast_ratio(white).unwrap() - 21.0).abs() < 1e-9);
        assert!((black.contrast_ratio(black).unwrap() - 1.0).abs() < 1e-9);
        // Both endpoints of the transfer function's knee, so the piecewise
        // curve is exercised rather than assumed.
        assert!((black.relative_luminance().unwrap()).abs() < 1e-9);
        assert!((white.relative_luminance().unwrap() - 1.0).abs() < 1e-9);
        // An indexed or unset color has no knowable luminance.
        assert_eq!(Color::Ansi(4).contrast_ratio(black), None);
        assert_eq!(Color::None.relative_luminance(), None);
    }
}
