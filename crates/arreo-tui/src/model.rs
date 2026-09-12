//! TUI model: pane list, focus, scrollback search, dirty tracking.
//!
//! One sentence: pure state for the sidebar + pane wall — no terminal, no
//! socket here, so every behavior is unit-testable and the interactive
//! evidence only has to prove rendering + input, not logic.

/// One pane as the TUI sees it (from Panes + Read + Metrics + Wait verbs).
#[derive(Debug, Clone, PartialEq)]
pub struct PaneView {
    pub id: String,
    /// Engine state name: working|idle|question|blocked|done|unknown.
    pub state: &'static str,
    pub ram_kb: u64,
    pub lines: Vec<String>,
    /// Recent peak RSS samples for the sparkline (T-0040): oldest first, KiB.
    /// Best-effort like `ram_kb` — empty when history is unavailable, and the
    /// view renders nothing rather than a lie.
    pub ram_history: Vec<u64>,
    /// What this pane is waiting for, when it is in `question` (T-0061): the last
    /// thing it said, which is what an agent's prompt is. `None` when it is not
    /// asking, or when it is asking by *silence* and there is no text to show.
    pub asking: Option<String>,
}

/// Keyboard/mouse focus target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar(usize),
    Pane,
}

/// Attention order for sidebar groups (most actionable first).
fn state_rank(state: &str) -> u8 {
    match state {
        "question" => 0,
        "blocked" => 1,
        "working" => 2,
        "done" => 3,
        "idle" => 4,
        _ => 5,
    }
}

pub struct Model {
    panes: Vec<PaneView>,
    focus: Focus,
    focused_id: Option<String>,
    /// Render cache: last-painted line count per pane (dirty tracking).
    painted: std::collections::HashMap<String, usize>,
}

impl Model {
    #[must_use]
    pub fn new() -> Self {
        Self {
            panes: Vec::new(),
            focus: Focus::Sidebar(0),
            focused_id: None,
            painted: std::collections::HashMap::new(),
        }
    }

    pub fn set_panes(&mut self, mut panes: Vec<PaneView>) {
        panes.sort_by_key(|p| (state_rank(p.state), p.id.clone()));
        self.panes = panes;
        // Clamp focus into range.
        if let Focus::Sidebar(i) = self.focus {
            let n = self.panes.len().max(1);
            self.focus = Focus::Sidebar(i.min(n - 1));
        }
    }

    /// Sidebar groups in attention order: (state, pane ids).
    #[must_use]
    pub fn sidebar_groups(&self) -> Vec<(&'static str, Vec<String>)> {
        let mut groups: Vec<(&'static str, Vec<String>)> = Vec::new();
        for pane in &self.panes {
            match groups.iter_mut().find(|(state, _)| *state == pane.state) {
                Some((_, ids)) => ids.push(pane.id.clone()),
                None => groups.push((pane.state, vec![pane.id.clone()])),
            }
        }
        groups
    }

    #[must_use]
    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// The pane the keyboard cursor is on, when the cursor is in the sidebar
    /// (T-0076). `None` while the cursor is in the pane region.
    ///
    /// This is what the sidebar paints as *focus*, and it is deliberately not
    /// the same fact as [`Self::focused_id`] (which pane is attached and
    /// streaming): one is where the keyboard is, the other is what is selected.
    /// Collapsing the two is how a UI ends up with no visible focus at all.
    #[must_use]
    pub fn cursor_id(&self) -> Option<&str> {
        match self.focus {
            Focus::Sidebar(i) => self.panes.get(i).map(|pane| pane.id.as_str()),
            Focus::Pane => None,
        }
    }

    /// True while the keyboard cursor is in the pane region (the attached pane
    /// is what a keypress acts on).
    #[must_use]
    pub fn focus_is_pane(&self) -> bool {
        self.focus == Focus::Pane
    }

    /// Focus a pane by id (attaching the main view); falls back to sidebar.
    pub fn focus_pane(&mut self, id: &str) {
        if self.panes.iter().any(|p| p.id == id) {
            self.focus = Focus::Pane;
            self.focused_id = Some(id.to_string());
        }
    }

    /// Put the keyboard cursor back in the pane region without changing which
    /// pane is attached — the wall's `Enter` (open the highlighted tile
    /// full-height) and the focus view's own return trip. A no-op when nothing
    /// is attached, because "focus the pane region" with no pane would leave
    /// every keypress with nowhere to go.
    pub fn focus_pane_view(&mut self) {
        if self.focused_id.is_some() {
            self.focus = Focus::Pane;
        }
    }

    /// Currently attached pane id, if any.
    #[must_use]
    pub fn focused_id(&self) -> Option<&str> {
        self.focused_id.as_deref()
    }

    pub fn focus_next(&mut self) {
        let n = self.panes.len().max(1);
        match self.focus {
            Focus::Sidebar(i) => self.focus = Focus::Sidebar((i + 1) % n),
            Focus::Pane => self.focus = Focus::Sidebar(0),
        }
    }

    pub fn focus_prev(&mut self) {
        let n = self.panes.len().max(1);
        match self.focus {
            Focus::Sidebar(i) => self.focus = Focus::Sidebar((i + n - 1) % n),
            Focus::Pane => self.focus = Focus::Sidebar(n - 1),
        }
    }

    /// Search the focused (or first) pane's scrollback; returns line indices.
    #[must_use]
    pub fn search(&self, needle: &str) -> Vec<usize> {
        let target = self
            .focused_id
            .as_deref()
            .and_then(|id| self.panes.iter().find(|p| p.id == id))
            .or(self.panes.first());
        match target {
            Some(pane) => pane
                .lines
                .iter()
                .enumerate()
                .filter(|(_, line)| line.contains(needle))
                .map(|(i, _)| i)
                .collect(),
            None => Vec::new(),
        }
    }

    /// Append live lines to a pane (delta path).
    pub fn push_lines(&mut self, id: &str, lines: &[String]) {
        if let Some(pane) = self.panes.iter_mut().find(|p| p.id == id) {
            pane.lines.extend_from_slice(lines);
        }
    }

    /// How many pane-views need repaint since the last call (dirty tracking:
    /// a pane is dirty when its line count differs from the painted count).
    /// Resets the cache — call once per frame.
    pub fn render_dirty(&mut self) -> usize {
        let mut dirty = 0;
        for pane in &self.panes {
            let painted = self.painted.get(&pane.id).copied().unwrap_or(usize::MAX);
            if painted != pane.lines.len() {
                dirty += 1;
                self.painted.insert(pane.id.clone(), pane.lines.len());
            }
        }
        dirty
    }

    /// All panes (sidebar rendering).
    #[must_use]
    pub fn panes(&self) -> &[PaneView] {
        &self.panes
    }

    /// Mutable panes (delta merge in the TUI loop).
    pub fn panes_mut(&mut self) -> &mut [PaneView] {
        &mut self.panes
    }
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}
