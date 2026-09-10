//! TUI rendering + event loop (T-0015): sidebar + focused pane, mouse-first.
//!
//! Layout: left sidebar (state groups with dots + RAM bars) | right region.
//! The right region is either the focused pane (scrollback text) or the pane
//! wall (every pane tiled, focused one highlighted).
//! Keys: j/k or arrows (move), Enter (attach/focus), / (search), q (quit),
//! w (wall ↔ focus), [ / ] (sidebar narrower/wider), Tab (cycle panes).
//! Mouse: click a sidebar row or a wall tile to focus it, drag the sidebar
//! border to resize the split.
//! Rendering is delta-driven: only changed lines re-render (the model's
//! dirty cache); steady state redraws chrome only.

use crate::model::{Focus, Model};
use crate::theme::Theme;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;

/// Which surface fills the region right of the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// One pane at a time, full height (the reading view).
    Focus,
    /// Every pane tiled — the "what is everyone doing" view.
    Wall,
}

/// Sidebar split bounds (columns, borders included).
pub const SIDEBAR_MIN: u16 = 18;
/// Upper bound keeps the sidebar from swallowing the pane region.
pub const SIDEBAR_MAX: u16 = 60;
pub const SIDEBAR_DEFAULT: u16 = 28;

pub struct App {
    pub model: Model,
    pub theme: Theme,
    pub search: String,
    pub searching: bool,
    pub status: String,
    pub view: ViewMode,
    /// Sidebar width in columns, drag- or key-resizable.
    pub sidebar_width: u16,
    /// True while the user holds the mouse on the sidebar border.
    pub dragging: bool,
    /// Last painted frame size; wall hit-testing uses it (kept in sync by
    /// `render`, so a click maps onto the tile the user sees).
    pub screen_rows: u16,
    pub screen_cols: u16,
    /// Scrollback offset for the focused pane, in lines back from the tail
    /// (0 = follow live output).
    pub scroll: usize,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    #[must_use]
    pub fn new() -> Self {
        Self {
            model: Model::new(),
            theme: Theme::default(),
            search: String::new(),
            searching: false,
            status: "connecting…".to_string(),
            view: ViewMode::Focus,
            sidebar_width: SIDEBAR_DEFAULT,
            dragging: false,
            screen_rows: 30,
            screen_cols: 120,
            scroll: 0,
        }
    }

    /// Render one frame: sidebar | (focused pane or pane wall) | status line.
    pub fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        self.screen_rows = area.height;
        self.screen_cols = area.width;
        // The status line spans the full width and owns the last row.
        let body = Rect {
            height: area.height.saturating_sub(1),
            ..area
        };
        let sidebar = self.sidebar_width.min(area.width.saturating_sub(10));
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(sidebar), Constraint::Min(10)])
            .split(body);
        self.render_sidebar(frame, chunks[0]);
        match self.view {
            ViewMode::Focus => self.render_pane(frame, chunks[1]),
            ViewMode::Wall => self.render_wall(frame, chunks[1]),
        }
        self.render_status(frame, area);
    }

    /// Pane wall: every pane tiled in a near-square grid, focused highlighted.
    fn render_wall(&self, frame: &mut Frame, area: Rect) {
        let panes = self.model.panes();
        if panes.is_empty() {
            let empty = Paragraph::new("no panes")
                .block(Block::default().borders(Borders::ALL).title("wall"));
            frame.render_widget(empty, area);
            return;
        }
        let (rows, cols) = wall_grid(panes.len());
        let row_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(vec![Constraint::Ratio(1, rows as u32); rows])
            .split(area);
        for (r, row_area) in row_chunks.iter().enumerate() {
            let col_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(vec![Constraint::Ratio(1, cols as u32); cols])
                .split(*row_area);
            for (c, cell) in col_chunks.iter().enumerate() {
                let Some(pane) = panes.get(r * cols + c) else {
                    continue;
                };
                let focused = self.model.focused_id() == Some(pane.id.as_str());
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title(format!("{} [{}]", pane.id, pane.state));
                let block = if focused {
                    block.border_style(
                        Style::default()
                            .fg(self.theme.state_color(pane.state))
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    block
                };
                // Tail of the scrollback: the wall shows what just happened.
                let visible = wall_tail(&pane.lines, cell.height.saturating_sub(2) as usize);
                let text = visible
                    .iter()
                    .map(|l| Line::from(l.as_str()))
                    .collect::<Vec<_>>();
                frame.render_widget(Paragraph::new(text).block(block), *cell);
            }
        }
    }

    fn render_sidebar(&self, frame: &mut Frame, area: Rect) {
        let mut items = Vec::new();
        for (state, ids) in self.model.sidebar_groups() {
            let dot = state_dot(state);
            let color = self.theme.state_color(state);
            // A waiting agent pulses: the terminal's own slow blink, so the
            // attention cue costs no frames and survives a static screenshot.
            let mut style = Style::default().fg(color).add_modifier(Modifier::BOLD);
            if state == "question" {
                style = style.add_modifier(Modifier::SLOW_BLINK);
            }
            items.push(ListItem::new(Line::from(vec![Span::styled(
                format!("{dot} {state}"),
                style,
            )])));
            for id in ids {
                let pane = self.model.panes().iter().find(|p| p.id == id);
                let (ram, marker) = match pane {
                    Some(p) => (
                        p.ram_kb,
                        if self.model.focused_id() == Some(&id) {
                            "▸"
                        } else {
                            " "
                        },
                    ),
                    None => (0, " "),
                };
                let bar = ram_bar(ram);
                let human = human_ram(ram);
                items.push(ListItem::new(Line::from(vec![
                    Span::raw(format!("{marker} {id:<9}{human:>6}")),
                    Span::styled(bar, Style::default().fg(color)),
                ])));
            }
        }
        let list = List::new(items).block(Block::default().borders(Borders::ALL).title("agents"));
        frame.render_widget(list, area);
    }

    fn render_pane(&self, frame: &mut Frame, area: Rect) {
        let (title, lines) = match self.model.focused_id() {
            Some(id) => match self.model.panes().iter().find(|p| p.id == id) {
                Some(pane) => {
                    let visible = if self.searching || !self.search.is_empty() {
                        let hits = self.model.search(&self.search);
                        pane.lines
                            .iter()
                            .enumerate()
                            .filter(|(i, _)| hits.contains(i) || self.search.is_empty())
                            .map(|(_, l)| l.clone())
                            .collect::<Vec<_>>()
                    } else {
                        // Scrollback: the tail end of the buffer, offset by
                        // however far the user has scrolled back.
                        let end = pane.lines.len().saturating_sub(self.scroll);
                        pane.lines[..end].to_vec()
                    };
                    let title = if self.scroll > 0 {
                        format!("{} [{}] ↑{}", pane.id, pane.state, self.scroll)
                    } else {
                        format!("{} [{}]", pane.id, pane.state)
                    };
                    (title, visible)
                }
                None => ("(no pane)".to_string(), Vec::new()),
            },
            None => (
                "(no focus — Enter on a sidebar row)".to_string(),
                Vec::new(),
            ),
        };
        let text = lines
            .iter()
            .map(|l| Line::from(l.as_str()))
            .collect::<Vec<_>>();
        let paragraph = Paragraph::new(text)
            .block(Block::default().borders(Borders::ALL).title(title.as_str()));
        frame.render_widget(paragraph, area);
    }

    fn render_status(&self, frame: &mut Frame, area: Rect) {
        let bar = Rect {
            x: area.x,
            y: area.height.saturating_sub(1),
            width: area.width,
            height: 1,
        };
        // Search owns the status line while active — a 1 Hz daemon poll
        // overwriting the prompt would hide what the user is typing.
        let text = if self.searching {
            format!("/{}▏  Enter apply · Esc cancel", self.search)
        } else if !self.search.is_empty() {
            let hits = self.model.search(&self.search);
            format!(
                "/{}  {} matching lines · Esc clears",
                self.search,
                hits.len()
            )
        } else {
            self.status.clone()
        };
        let status = Paragraph::new(text);
        frame.render_widget(status, bar);
    }

    /// Keyboard input. Returns false when the app should quit.
    pub fn on_key(&mut self, code: crossterm::event::KeyCode) -> bool {
        use crossterm::event::KeyCode as K;
        if self.searching {
            match code {
                K::Esc => {
                    self.search.clear();
                    self.searching = false;
                }
                K::Enter => self.searching = false,
                K::Backspace => {
                    self.search.pop();
                }
                K::Char(c) => self.search.push(c),
                _ => {}
            }
            return true;
        }
        match code {
            K::Char('q') | K::Esc => false,
            K::Char('w') => {
                self.view = match self.view {
                    ViewMode::Focus => ViewMode::Wall,
                    ViewMode::Wall => ViewMode::Focus,
                };
                true
            }
            K::PageUp => {
                self.on_scroll(10);
                true
            }
            K::PageDown => {
                self.on_scroll(-10);
                true
            }
            K::Home => {
                self.scroll = usize::MAX;
                true
            }
            K::End => {
                self.scroll = 0;
                true
            }
            K::Char('[') => {
                self.sidebar_width = self.sidebar_width.saturating_sub(2).max(SIDEBAR_MIN);
                true
            }
            K::Char(']') => {
                self.sidebar_width = (self.sidebar_width + 2).min(SIDEBAR_MAX);
                true
            }
            K::Char('j') | K::Down => {
                self.model.focus_next();
                self.scroll = 0;
                true
            }
            K::Char('k') | K::Up => {
                self.model.focus_prev();
                self.scroll = 0;
                true
            }
            K::Enter => {
                if let Focus::Sidebar(i) = self.model.focus() {
                    let ids: Vec<String> =
                        self.model.panes().iter().map(|p| p.id.clone()).collect();
                    if let Some(id) = ids.get(i) {
                        self.model.focus_pane(id);
                        self.scroll = 0;
                    }
                }
                true
            }
            K::Char('/') => {
                self.searching = true;
                self.search.clear();
                true
            }
            K::Tab => {
                self.model.focus_next();
                self.scroll = 0;
                true
            }
            _ => true,
        }
    }

    /// Mouse click at (col, row). `crossterm` reports 0-based coordinates.
    ///
    /// The sidebar border is a drag handle; sidebar rows focus their pane;
    /// in wall mode a click inside a tile focuses that tile's pane.
    pub fn on_click(&mut self, col: u16, row: u16) {
        if self.is_border(col) {
            self.dragging = true;
            return;
        }
        if col < self.sidebar_width {
            self.click_sidebar_row(row);
            return;
        }
        if self.view == ViewMode::Wall {
            self.click_wall(col, row);
        }
    }

    /// Wheel/keyboard scrollback: positive `delta` moves back in history.
    /// Clamped to the focused pane's length, and a focus change resets it.
    pub fn on_scroll(&mut self, delta: i32) {
        let len = self
            .model
            .focused_id()
            .and_then(|id| self.model.panes().iter().find(|p| p.id == id))
            .map_or(0, |p| p.lines.len());
        let next = if delta >= 0 {
            self.scroll.saturating_add(delta as usize)
        } else {
            self.scroll.saturating_sub(delta.unsigned_abs() as usize)
        };
        self.scroll = next.min(len);
    }

    /// Mouse drag: while the border is held, the sidebar follows the cursor.
    pub fn on_drag(&mut self, col: u16) {
        if self.dragging {
            self.sidebar_width = (col + 1).clamp(SIDEBAR_MIN, SIDEBAR_MAX);
        }
    }

    /// Mouse release ends a border drag (idempotent).
    pub fn on_drag_end(&mut self) {
        self.dragging = false;
    }

    fn is_border(&self, col: u16) -> bool {
        col + 1 == self.sidebar_width || col == self.sidebar_width
    }

    fn click_sidebar_row(&mut self, row: u16) {
        let mut current_row: u16 = 0;
        for (_, ids) in self.model.sidebar_groups() {
            current_row += 1; // group header
            for id in ids {
                if current_row == row {
                    self.model.focus_pane(&id);
                    self.scroll = 0;
                    return;
                }
                current_row += 1;
            }
        }
    }

    /// Wall geometry is recomputed from the same helper the renderer uses, so
    /// a click lands on the tile the user sees (body = every row but status).
    fn click_wall(&mut self, col: u16, row: u16) {
        let body_rows = self.screen_rows.saturating_sub(1);
        let region_cols = self.screen_cols.saturating_sub(self.sidebar_width);
        if row >= body_rows || col < self.sidebar_width || region_cols == 0 {
            return;
        }
        let (rows, cols) = wall_grid(self.model.panes().len());
        let cell_row = (u32::from(row) * rows as u32 / u32::from(body_rows)) as usize;
        let cell_col =
            (u32::from(col - self.sidebar_width) * cols as u32 / u32::from(region_cols)) as usize;
        let index = cell_row * cols + cell_col;
        if let Some(id) = self.model.panes().get(index).map(|p| p.id.clone()) {
            self.model.focus_pane(&id);
            self.scroll = 0;
        }
    }
}

/// Near-square wall grid: `cols` columns of `rows` tiles hold `n` panes.
#[must_use]
pub fn wall_grid(n: usize) -> (usize, usize) {
    if n <= 1 {
        return (1, 1);
    }
    let cols = (n as f64).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    (rows.max(1), cols.max(1))
}

/// Last `max` lines of a pane, for a wall tile of bounded height.
fn wall_tail(lines: &[String], max: usize) -> &[String] {
    if lines.len() <= max {
        lines
    } else {
        &lines[lines.len() - max..]
    }
}

/// State dot glyph (Nerd-Font-free: plain ●, pulse via bold on question).
fn state_dot(state: &str) -> &'static str {
    match state {
        "question" => "◉",
        "blocked" => "⬢",
        "working" => "●",
        "done" => "✓",
        "idle" => "○",
        _ => "?",
    }
}

/// RAM bar: 10 cells, █ per 100 MB. Any nonzero RSS paints at least one cell
/// (an all-░ bar next to a live pane reads as a bug).
fn ram_bar(ram_kb: u64) -> String {
    let filled = if ram_kb == 0 {
        0
    } else {
        (ram_kb / (100 * 1024)).clamp(1, 10) as usize
    };
    format!("{}{}", "█".repeat(filled), "░".repeat(10 - filled))
}

/// Human RAM: shells report KiB-scale — show K below 1 MB so the sidebar is
/// never a wall of zeros (honest units, not fake precision).
fn human_ram(ram_kb: u64) -> String {
    if ram_kb == 0 {
        "     —".to_string()
    } else if ram_kb < 1024 {
        format!("{ram_kb:>4}K ")
    } else {
        format!("{:>4}M ", ram_kb / 1024)
    }
}
