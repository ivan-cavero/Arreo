//! TUI rendering + event loop (T-0015): sidebar + focused pane, mouse-first.
//!
//! Layout: left sidebar (state groups with dots + labels + RAM bars) | right
//! region. The right region is either the focused pane (scrollback text) or the
//! pane wall (every pane tiled, focused one highlighted).
//! Keys: j/k or arrows (move), Enter (attach/focus), / (search), q (quit),
//! w (wall ↔ focus), t (theme picker), ? (key list), [ / ] (sidebar
//! narrower/wider), Tab (cycle panes), PgUp/PgDn/Home/End (scrollback).
//! The `/theme` command in the search prompt opens the same picker.
//! Mouse: click a sidebar row or a wall tile to focus it, drag the sidebar
//! border to resize the split.
//! Rendering is delta-driven: only changed lines re-render (the model's
//! dirty cache); steady state redraws chrome only.
//!
//! **Two facts, two signals (T-0076).** The *focus* is where the keyboard is
//! (a sidebar row, or the pane region after `Enter`); the *selection* is which
//! pane is attached and streaming. They are drawn differently on purpose — the
//! cursor row carries `▶` and inverse video, the attached row carries `▸` in
//! `primary-strong` (BRAND §2's selection color), the focused region's border
//! is `primary` (BRAND §2: cyan is "active states") — because a UI where the
//! two look alike is a UI where the user cannot tell what a keypress will do.
//!
//! **State is never carried by color alone.** Every state renders a distinct
//! dot glyph *and* its name, and the state's label falls back to the neutral
//! when its hue cannot carry WCAG AA as text (see `Theme::state_label_color`).
//! A `NO_COLOR` frame is fully readable; the captures under
//! `.loop/evidence/T-0076/` are the proof, and the depth tests are the gate.
//!
//! **Motion is optional.** `question` pulses through the terminal's own slow
//! blink (free: no frames, and it survives a static screenshot as a style).
//! `tui.reduce_motion` — and `NO_COLOR`, which implies it — turns that off and
//! leaves the dot shape and the label, which is what makes the state readable
//! in the first place.

use crate::model::{Focus, Model};
use crate::settings::Settings;
use crate::theme::ThemeState;
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
/// Columns the pane region is guaranteed, whatever the sidebar wants (T-0076):
/// a terminal too narrow for both gets a proportionally smaller sidebar rather
/// than a clipped pane.
pub const PANE_MIN: u16 = 24;
/// The narrowest sidebar that still renders a legible row (a glyph, an id, a
/// RAM reading). Below this the sidebar is gone and there is nothing to show.
pub const SIDEBAR_FLOOR: u16 = 12;

/// The key list (`?`), as (keys, what they do). One place: the on-screen list
/// and the status line's legend are the same bindings the handler implements,
/// and a binding added without a line here is a binding nobody can find.
const KEY_LIST: &[(&str, &str)] = &[
    ("j/k ↑/↓", "move the cursor"),
    ("Enter", "attach the pane (or open the wall tile)"),
    ("Tab", "next pane"),
    ("w", "wall ↔ focus"),
    ("t", "theme picker"),
    ("/", "search the transcript"),
    ("[ ]", "sidebar narrower / wider"),
    ("PgUp/PgDn", "scroll the transcript"),
    ("Home/End", "oldest line / live output"),
    ("?", "this list"),
    ("q / Esc", "quit"),
];

/// The keyboard cursor's glyph (a sidebar row), and the attached pane's.
const FOCUS_CURSOR: &str = "▶";
const SELECTION_MARKER: &str = "▸";
/// Columns a pane id gets before it is cut. Long ids are still matched by
/// their prefix; the full id is in the pane's title when attached.
const ID_WIDTH: usize = 9;
/// Indent of a pane's question line: the same column the pane ids start at
/// (the two-glyph gutter plus a space), so the question reads as a line *of*
/// that pane rather than as a new one (T-0076).
const ASK_INDENT: &str = "   ";

/// How many columns the sidebar actually gets: what the user asked for,
/// clamped to what this terminal can pay for (T-0076).
///
/// A terminal too narrow for [`SIDEBAR_MIN`] plus [`PANE_MIN`] shrinks the
/// sidebar first and never below [`SIDEBAR_FLOOR`] — the alternative is a
/// sidebar that eats the pane region (the "glitch" this clamps out), and at
/// 80×24 there is room for both with columns to spare.
#[must_use]
pub fn sidebar_extent(width: u16, desired: u16) -> u16 {
    let cap = width
        .saturating_sub(PANE_MIN)
        .max(SIDEBAR_FLOOR.min(width.saturating_sub(1)));
    desired.clamp(SIDEBAR_MIN, SIDEBAR_MAX).min(cap)
}

/// The status bar's key legend, in as much detail as the terminal can pay for.
///
/// A status line that clips mid-word teaches nothing, so the legend steps down
/// with the width instead of being truncated (T-0076).
#[must_use]
pub fn key_hints(columns: u16) -> &'static str {
    if columns >= 96 {
        "j/k move · Enter attach · Tab cycle · w wall · t theme · / search · ? keys · q quit"
    } else if columns >= 64 {
        "j/k · Enter · Tab · w wall · t theme · / find · ? keys · q quit"
    } else if columns >= 40 {
        "j/k · Enter · w · t · / · ? · q"
    } else {
        "? keys · q quit"
    }
}

pub struct App {
    pub model: Model,
    /// Which machine, and over what — "workbox · relay", "this machine · socket"
    /// (T-0061). Rendered once, in the sidebar's title: one sidebar is one
    /// machine's panes, so a per-row column would be the same fact N times.
    pub session: String,
    pub theme: ThemeState,
    /// `[tui]` settings for this run (T-0076): today, whether motion is off.
    pub settings: Settings,
    pub search: String,
    pub searching: bool,
    pub status: String,
    pub view: ViewMode,
    /// The key list is up (`?`). It consumes input until dismissed.
    pub help: bool,
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
    /// Open theme picker, if any (opened by `t` or the `/theme` command).
    pub picker: Option<Picker>,
}

/// The `/theme` picker: a list of every loaded theme, cursor included.
#[derive(Debug, Clone)]
pub struct Picker {
    pub items: Vec<String>,
    pub index: usize,
    /// The theme to restore if the user cancels.
    original: String,
    /// Set when applying the highlighted theme failed (shown in the list).
    pub error: Option<String>,
}

impl Picker {
    #[must_use]
    pub fn new(items: Vec<String>, current: &str) -> Self {
        let index = items.iter().position(|name| name == current).unwrap_or(0);
        Self {
            items,
            index,
            original: current.to_string(),
            error: None,
        }
    }

    #[must_use]
    pub fn selected(&self) -> Option<&str> {
        self.items.get(self.index).map(String::as_str)
    }

    pub fn next(&mut self) {
        if !self.items.is_empty() {
            self.index = (self.index + 1) % self.items.len();
        }
    }

    pub fn prev(&mut self) {
        if !self.items.is_empty() {
            self.index = (self.index + self.items.len() - 1) % self.items.len();
        }
    }
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
            session: "this machine · socket".to_string(),
            theme: ThemeState::new(),
            settings: Settings::default(),
            search: String::new(),
            searching: false,
            status: "connecting…".to_string(),
            view: ViewMode::Focus,
            help: false,
            sidebar_width: SIDEBAR_DEFAULT,
            dragging: false,
            screen_rows: 30,
            screen_cols: 120,
            scroll: 0,
            picker: None,
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
        // The sidebar degrades by shrinking, never by eating the pane region
        // (T-0076): `sidebar_extent` clamps what the user asked for to what
        // this terminal can pay for.
        let sidebar = sidebar_extent(area.width, self.sidebar_width);
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(sidebar),
                Constraint::Min(area.width.saturating_sub(sidebar)),
            ])
            .split(body);
        self.render_sidebar(frame, chunks[0]);
        match self.view {
            ViewMode::Focus => self.render_pane(frame, chunks[1]),
            ViewMode::Wall => self.render_wall(frame, chunks[1]),
        }
        if let Some(picker) = self.picker.clone() {
            self.render_picker(frame, area, &picker);
        }
        if self.help {
            self.render_help(frame, area);
        }
        self.render_status(frame, area);
    }

    /// The key list (`?`): every binding, on screen, so the TUI is complete
    /// without the docs (T-0076). Rendered over a cleared box so the frame
    /// behind it cannot be mistaken for the list.
    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let lines: Vec<Line> = KEY_LIST
            .iter()
            .map(|(keys, what)| {
                Line::from(vec![
                    Span::styled(format!(" {keys:<11}"), self.theme.primary_style()),
                    Span::styled(*what, self.theme.text_style()),
                ])
            })
            .collect();
        let width = 46.min(area.width.saturating_sub(4));
        let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
        let overlay = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };
        frame.render_widget(ratatui::widgets::Clear, overlay);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.color("borderActive")))
            .title("keys")
            .title_style(self.theme.text_style())
            .style(Style::default().bg(self.theme.color("backgroundPanel")));
        let paragraph = Paragraph::new(lines)
            .block(block)
            .style(self.theme.text_style());
        frame.render_widget(paragraph, overlay);
    }

    /// Theme picker overlay: names, the current marker, and any load error.
    fn render_picker(&self, frame: &mut Frame, area: Rect, picker: &Picker) {
        let width = 44.min(area.width.saturating_sub(4));
        let height = (picker.items.len() as u16 + 4).min(area.height.saturating_sub(2));
        let overlay = Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        };
        frame.render_widget(ratatui::widgets::Clear, overlay);
        let items: Vec<ListItem> = picker
            .items
            .iter()
            .map(|name| {
                let marker = if Some(name.as_str()) == picker.selected() {
                    "▸"
                } else if name == self.theme.theme().name() {
                    "•"
                } else {
                    " "
                };
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{marker} {name}"), self.theme.text_style()),
                    Span::styled(
                        match self.theme.source_of(name) {
                            Some(path) => format!("  {path}"),
                            None => "  built-in".to_string(),
                        },
                        self.theme.muted_style(),
                    ),
                ]))
            })
            .collect();
        let title = format!(
            "theme: {} ({})",
            self.theme.theme().name(),
            self.theme.depth_name()
        );
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.color("borderActive")))
            .title(title)
            .title_style(self.theme.text_style())
            .style(Style::default().bg(self.theme.color("backgroundPanel")));
        if let Some(error) = &picker.error {
            block = block.title_bottom(Line::from(Span::styled(
                format!(" {error}"),
                Style::default().fg(self.theme.color("error")),
            )));
        }
        let list = List::new(items).block(block);
        frame.render_widget(list, overlay);
    }

    /// Pane wall: every pane tiled in a near-square grid, the cursor tile
    /// highlighted.
    ///
    /// The wall's keyboard cursor *is* the attached pane (there is one
    /// highlighted tile, and j/k/Tab move it), so the marker is the same
    /// `primary` border the focus view uses for "the keyboard is here" — with
    /// the `▶` glyph in the title as the shape that survives NO_COLOR.
    fn render_wall(&self, frame: &mut Frame, area: Rect) {
        let panes = self.model.panes();
        if panes.is_empty() {
            let empty = Paragraph::new("no panes")
                .style(self.theme.muted_style())
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(self.theme.border_style())
                        .title("wall")
                        .title_style(self.theme.text_style()),
                );
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
                let cursor = self.model.focused_id() == Some(pane.id.as_str());
                // Title = dot shape + id + the state's own name: the tile says
                // what it is with no color at all, and the cursor glyph is a
                // shape rather than a hue.
                let title = format!(
                    "{} {} [{}]{}",
                    state_dot(pane.state),
                    pane.id,
                    pane.state,
                    if cursor {
                        format!(" {FOCUS_CURSOR}")
                    } else {
                        String::new()
                    }
                );
                let block = Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .title_style(self.theme.state_label_style(pane.state));
                let block = if cursor {
                    block.border_style(
                        Style::default()
                            .fg(self.theme.color("primary"))
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    block.border_style(self.theme.border_style())
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
        let cursor = self.model.cursor_id();
        for (state, ids) in self.model.sidebar_groups() {
            // The group header is the state's *label*: a distinct dot shape and
            // the state's own name, so the row says what it is with no color at
            // all (T-0076, criterion 2). The hue is a second channel, not the
            // only one — and the label steps to the neutral when the hue cannot
            // carry AA as text (`state_label_color`).
            // A waiting agent pulses — the terminal's own slow blink, so the
            // attention cue costs no frames and survives a static screenshot as
            // a style. `tui.reduce_motion` (and NO_COLOR, which implies it)
            // turns it off; the dot shape and the label stay, which is what
            // makes the state readable at all (T-0076).
            let label = if state == "question" && !self.settings.reduce_motion {
                self.theme
                    .state_label_style(state)
                    .add_modifier(Modifier::SLOW_BLINK)
            } else {
                self.theme.state_label_style(state)
            };
            items.push(ListItem::new(Line::from(vec![
                Span::styled(
                    format!("  {} ", state_dot(state)),
                    Style::default().fg(self.theme.state_color(state)),
                ),
                Span::styled(state, label),
            ])));
            for id in ids {
                let pane = self.model.panes().iter().find(|p| p.id == id);
                let ram = pane.map_or(0, |p| p.ram_kb);
                // Two independent facts, two glyphs (T-0076): `▶` is where the
                // keyboard is, `▸` is which pane is attached and streaming. A
                // frame that collapsed them could not show focus at all, and a
                // keypress would act somewhere the user cannot see.
                let here = if cursor == Some(id.as_str()) {
                    FOCUS_CURSOR
                } else {
                    " "
                };
                let attached = if self.model.focused_id() == Some(id.as_str()) {
                    SELECTION_MARKER
                } else {
                    " "
                };
                let row_style = if cursor == Some(id.as_str()) {
                    // Inverse video: the focus survives a NO_COLOR terminal and
                    // a 16-color one, where a hue difference might not.
                    self.theme.text_style().add_modifier(Modifier::REVERSED)
                } else {
                    self.theme.text_style()
                };
                let bar = ram_bar(ram);
                let human = human_ram(ram);
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{here}{attached} {id:<ID_WIDTH$}{human:>6}"),
                        row_style,
                    ),
                    Span::styled(bar, Style::default().fg(self.theme.state_color(state))),
                ])));
                // **What it is asking (T-0061).** Indented under the pane, so the
                // operator reads "which agent, waiting for what" as one thing —
                // and on a remote pane this is the whole point: they cannot walk
                // over and look at the terminal.
                if let Some(asking) = pane.and_then(|p| p.asking.as_deref()) {
                    items.push(ListItem::new(Line::from(Span::styled(
                        format!("{ASK_INDENT}{}", truncate(asking, ask_width(area.width))),
                        self.theme.muted_style(),
                    ))));
                }
            }
        }
        let list = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(self.theme.border_style())
                // The session label *is* the title: "which machine, over what" is
                // the fact a sidebar of panes needs (T-0061), and it fits — a
                // prefix would push it past the block's width, where ratatui
                // clips silently and the operator learns nothing. Cut here, with a
                // mark, when a long machine name still does not fit.
                .title(truncate(
                    &self.session,
                    usize::from(area.width).saturating_sub(2),
                ))
                .title_style(self.theme.text_style()),
        );
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
                    let graph = sparkline(&pane.ram_history);
                    // The title carries the state's dot shape and its name, so
                    // the reading view says what it is without color too.
                    let marker = if self.model.focus_is_pane() {
                        format!("{FOCUS_CURSOR} ")
                    } else {
                        String::new()
                    };
                    let title = if self.scroll > 0 {
                        format!(
                            "{marker}{} {} [{}] ↑{} {}",
                            state_dot(pane.state),
                            pane.id,
                            pane.state,
                            self.scroll,
                            graph
                        )
                    } else {
                        format!(
                            "{marker}{} {} [{}] {}",
                            state_dot(pane.state),
                            pane.id,
                            pane.state,
                            graph
                        )
                    };
                    (title, visible)
                }
                None => ("(no pane)".to_string(), Vec::new()),
            },
            None => (
                "(no pane attached — Enter on a sidebar row)".to_string(),
                Vec::new(),
            ),
        };
        let text = lines
            .iter()
            .map(|l| Line::from(l.as_str()))
            .collect::<Vec<_>>();
        // **Focus, not selection (T-0076).** This border is `primary` (BRAND
        // §2: cyan is active states) exactly when the keyboard is in the pane
        // region; while the cursor is in the sidebar the border is the plain
        // hairline, so the two facts never look alike.
        let border = if self.model.focus_is_pane() {
            Style::default()
                .fg(self.theme.color("primary"))
                .add_modifier(Modifier::BOLD)
        } else {
            self.theme.border_style()
        };
        let paragraph = Paragraph::new(text).style(self.theme.text_style()).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border)
                .title(title)
                .title_style(self.theme.text_style()),
        );
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
            // Connection/daemon status *and* the key legend: the keys are the
            // part that must not be clipped away, so they come last and the
            // whole line is cut to the terminal (T-0076).
            let hints = key_hints(area.width);
            if self.status.is_empty() {
                hints.to_string()
            } else {
                format!("{} · {hints}", self.status)
            }
        };
        let text = truncate(&text, usize::from(area.width));
        let status = Paragraph::new(text).style(self.theme.text_style()).block(
            Block::default().style(Style::default().bg(self.theme.color("backgroundPanel"))),
        );
        frame.render_widget(status, bar);
    }

    /// Keyboard input. Returns false when the app should quit.
    pub fn on_key(&mut self, code: crossterm::event::KeyCode) -> bool {
        use crossterm::event::KeyCode as K;
        if self.picker.is_some() {
            match code {
                K::Esc | K::Char('q') => {
                    // Cancel restores the theme the picker opened with.
                    if let Some(picker) = self.picker.take() {
                        let _ = self.theme.select(&picker.original);
                    }
                }
                K::Char('j') | K::Down => {
                    if let Some(picker) = self.picker.as_mut() {
                        picker.next();
                    }
                }
                K::Char('k') | K::Up => {
                    if let Some(picker) = self.picker.as_mut() {
                        picker.prev();
                    }
                }
                K::Enter => self.apply_picked_theme(),
                _ => {}
            }
            return true;
        }
        if self.searching {
            match code {
                K::Esc => {
                    self.search.clear();
                    self.searching = false;
                }
                K::Enter => {
                    self.searching = false;
                    // The prompt is also the TUI's command line: `/theme`
                    // opens the picker, exactly like the docs promise.
                    if self.search.trim() == "theme" {
                        self.search.clear();
                        self.open_picker();
                    }
                }
                K::Backspace => {
                    self.search.pop();
                }
                K::Char(c) => self.search.push(c),
                _ => {}
            }
            return true;
        }
        if self.help {
            // The key list is modal like the picker: any dismissal key closes
            // it, and nothing behind it can be triggered by accident.
            match code {
                K::Esc | K::Char('q') | K::Char('?') => self.help = false,
                _ => {}
            }
            return true;
        }
        match code {
            K::Char('q') => false,
            K::Esc => {
                // Progressive dismissal, which is what the status line already
                // promises: an applied search is what Esc clears first, and
                // only an empty screen means "quit". A hint that lies about
                // what a key does is worse than no hint (T-0076).
                if self.search.is_empty() {
                    false
                } else {
                    self.search.clear();
                    true
                }
            }
            K::Char('?') => {
                self.help = true;
                true
            }
            K::Char('t') => {
                self.open_picker();
                true
            }
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
                self.move_cursor(1);
                true
            }
            K::Char('k') | K::Up => {
                self.move_cursor(-1);
                true
            }
            K::Enter => {
                match self.view {
                    // In the wall, Enter opens the highlighted tile full-height:
                    // the wall is the overview and the reading view is one key
                    // away, which is what the tile was for.
                    ViewMode::Wall => {
                        if self.model.focused_id().is_some() {
                            self.view = ViewMode::Focus;
                            self.model.focus_pane_view();
                        }
                    }
                    ViewMode::Focus => {
                        if let Focus::Sidebar(i) = self.model.focus() {
                            let ids: Vec<String> =
                                self.model.panes().iter().map(|p| p.id.clone()).collect();
                            if let Some(id) = ids.get(i) {
                                self.model.focus_pane(id);
                                self.scroll = 0;
                            }
                        }
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
                self.move_cursor(1);
                true
            }
            _ => true,
        }
    }

    /// Move the keyboard cursor: through the sidebar in the focus view, through
    /// the wall's tiles in the wall view (where the highlighted tile is also
    /// the attached one, so the cursor is never invisible).
    fn move_cursor(&mut self, delta: i32) {
        if self.view == ViewMode::Wall {
            let ids: Vec<String> = self.model.panes().iter().map(|p| p.id.clone()).collect();
            if ids.is_empty() {
                return;
            }
            let next = match self
                .model
                .focused_id()
                .and_then(|id| ids.iter().position(|candidate| candidate == id))
            {
                Some(i) if delta >= 0 => (i + 1) % ids.len(),
                Some(i) => (i + ids.len() - 1) % ids.len(),
                None if delta >= 0 => 0,
                None => ids.len() - 1,
            };
            let id = ids[next].clone();
            self.model.focus_pane(&id);
            self.scroll = 0;
            return;
        }
        if delta >= 0 {
            self.model.focus_next();
        } else {
            self.model.focus_prev();
        }
        self.scroll = 0;
    }

    /// Open the theme picker over every theme the catalog found.
    pub fn open_picker(&mut self) {
        let names = self.theme.names();
        let current = self.theme.theme().name().to_string();
        self.picker = Some(Picker::new(names, &current));
    }

    /// Apply the highlighted theme. A broken user theme is reported in the
    /// picker and leaves the current theme untouched.
    pub fn apply_picked_theme(&mut self) {
        let Some(picker) = self.picker.as_mut() else {
            return;
        };
        let Some(name) = picker.selected().map(str::to_string) else {
            return;
        };
        match self.theme.select(&name) {
            Ok(()) => {
                self.picker = None;
                self.status = format!("theme: {name}");
            }
            Err(e) => picker.error = Some(e.to_string()),
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

/// How much room a question line has inside the sidebar: the block's borders and
/// the four-column indent, off the sidebar's own width. A question that cannot fit
/// is cut with an ellipsis rather than wrapped — a wrapped question would push
/// every pane below it off the screen, and the full text is in the pane view.
fn ask_width(sidebar_width: u16) -> usize {
    usize::from(sidebar_width).saturating_sub(2 + ASK_INDENT.len() + 1)
}

/// Cut a line to `width` columns, marking that it was cut.
fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= width {
        return text.to_string();
    }
    let mut out: String = chars[..width.saturating_sub(1)].iter().collect();
    out.push('…');
    out
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

/// RAM sparkline from peak history (T-0040): eight block chars, oldest left.
///
/// Empty history renders nothing rather than a flat line — a flat line would
/// claim "steady" where the truth is "unknown". The peak labels the worst
/// moment; the line shows the shape. When enforcement is active the budget line
/// is the caller's job (T-0041 renders over the same series); this draws data.
fn sparkline(history: &[u64]) -> String {
    if history.is_empty() {
        return String::new();
    }
    const CELLS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    // Eight cells across the recent tail: the series is per-minute over the
    // last hour, and eight cells keep the title readable.
    let tail: Vec<u64> = {
        let mut tail = history.to_vec();
        if tail.len() > 8 {
            tail = tail[tail.len() - 8..].to_vec();
        }
        tail
    };
    let peak = tail.iter().copied().max().unwrap_or(0).max(1);
    let mut out = String::from("▏");
    for value in &tail {
        let level = (value * 7 / peak).min(7) as usize;
        out.push(CELLS[level]);
    }
    out.push('▏');
    out.push_str(&format!(" peak {}", human_ram(peak)));
    out
}
