//! Minimal theme surface (T-0015): semantic colors for the TUI today,
//! full JSON engine in T-0016 (same call sites, richer loader).
//!
//! One sentence: every color in the UI comes from here — no hardcoded
//! ANSI anywhere else — so T-0016 only replaces the loader, not the UI.

use ratatui::style::Color;

/// Semantic palette (dark-first; matches the `arreo` built-in of T-0016).
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub primary: Color,
    pub working: Color,
    pub blocked: Color,
    pub done: Color,
    pub idle: Color,
    pub question: Color,
    pub error: Color,
    pub muted: Color,
    pub background: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::arreo()
    }
}

impl Theme {
    /// The `arreo` look (proposal, ROADMAP §3.12): near-black bg, soft cyan
    /// primary; state colors are the product's visual language.
    #[must_use]
    pub const fn arreo() -> Self {
        Self {
            primary: Color::Rgb(125, 207, 255),
            working: Color::Rgb(125, 207, 255),
            blocked: Color::Rgb(224, 175, 104),
            done: Color::Rgb(158, 206, 106),
            idle: Color::Rgb(120, 120, 120),
            question: Color::Rgb(224, 175, 104),
            error: Color::Rgb(247, 118, 142),
            muted: Color::Rgb(90, 90, 90),
            background: Color::Rgb(14, 17, 22),
        }
    }

    /// State dot color (sidebar + status board language, shared with phone).
    #[must_use]
    pub fn state_color(&self, state: &str) -> Color {
        match state {
            "working" => self.working,
            "blocked" => self.blocked,
            "done" => self.done,
            "idle" => self.idle,
            "question" => self.question,
            _ => self.muted,
        }
    }
}
