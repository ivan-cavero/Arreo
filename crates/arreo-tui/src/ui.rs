//! TUI rendering + event loop (T-0015): sidebar + focused pane, mouse-first.
//!
//! Layout: left sidebar (state groups with dots + labels + RAM bars) | right
//! region. The right region is either the focused pane (scrollback text) or the
//! pane wall (every pane tiled, focused one highlighted).
//! Keys: j/k or arrows (move), Enter (attach/focus), / (search), q (quit),
//! w (wall ↔ focus), t (theme picker), ? (key list), [ / ] (sidebar
//! narrower/wider), Tab (cycle panes), PgUp/PgDn/Home/End (scrollback).
//! **The fleet keys (T-0074)**: s (spawn), i (send), x (kill), m (machines),
//! g (trust grants) — every one of them a keyboard equivalent of what the
//! mouse or the CLI could do, and every one documented in [`KEY_LIST`], the
//! `--help` text and `docs/tour.md`.
//! The `/theme` command in the search prompt opens the same picker.
//! Mouse: click a sidebar row or a wall tile to focus it, drag the sidebar
//! border to resize the split.
//! Rendering is delta-driven: only changed lines re-render (the model's
//! dirty cache); steady state redraws chrome only.
//!
//! **The UI never touches a socket (T-0074).** A key that acts on the fleet
//! pushes an [`Action`] onto a queue; the main loop drains it, runs the verb and
//! pushes the answer back as a status line and, for a listing, rows for a panel.
//! That is what keeps every key unit-testable without a daemon — and what makes
//! "the UI renders a viewer's actions disabled" a property of a pure value
//! rather than of a round trip that has already failed.
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

use crate::fleet::{Grant, Machine, Outcome};
use crate::model::{Focus, Model};
use crate::settings::Settings;
use crate::theme::ThemeState;
use arreo_core::identity::authority::VerbDenial;
use arreo_core::identity::role::{self, Verb};
use arreo_core::identity::{DeviceId, Role};
use arreo_core::mesh::Presence;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;
use std::collections::VecDeque;

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

/// The key list (`?`), as (keys, what they do, the control verb the key needs —
/// `None` for a key a viewer may use). One place: the on-screen list, the
/// status line's legend and the handler are the same bindings the handler
/// implements, and a binding added without a line here is a binding nobody can
/// find. The third field is what makes a viewer's disabled actions *render*
/// disabled (T-0074): the reason is the daemon's own denial, built from the
/// same `role::check` the daemon's gate calls.
const KEY_LIST: &[(&str, &str, Option<Verb>)] = &[
    ("j/k ↑/↓", "move the cursor", None),
    ("Enter", "attach the pane (or open the wall tile)", None),
    ("Tab", "next pane", None),
    ("w", "wall ↔ focus", None),
    ("t", "theme picker", None),
    ("/", "search the transcript", None),
    ("[ ]", "sidebar narrower / wider", None),
    ("PgUp/PgDn", "scroll the transcript", None),
    ("Home/End", "oldest line / live output", None),
    (
        "s",
        "spawn an agent: <id> <program> [args…]",
        Some(Verb::Spawn),
    ),
    ("i", "send text to the attached pane", Some(Verb::Send)),
    ("x", "kill the attached pane (confirm)", Some(Verb::Kill)),
    ("m", "machines: list · a add · r rename · x remove", None),
    (
        "g",
        "trust: this machine's grants · a grant · x revoke",
        None,
    ),
    ("?", "this list", None),
    ("q / Esc", "quit", None),
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
/// with the width instead of being truncated (T-0076). The fleet keys (T-0074)
/// join the ladder at the widths that can hold them: `s/i/x` for the agents and
/// `m`/`g` for the servers, in that order of usefulness.
#[must_use]
pub fn key_hints(columns: u16) -> &'static str {
    if columns >= 124 {
        "j/k move · Enter attach · s spawn · i send · x kill · m machines · g trust · w wall · t theme · / search · ? keys · q quit"
    } else if columns >= 108 {
        "j/k move · Enter attach · s/i/x agents · m machines · g trust · w wall · t theme · / find · ? keys · q quit"
    } else if columns >= 92 {
        "j/k · Enter · s/i/x agents · m machines · g trust · w · t theme · / find · ? keys · q quit"
    } else if columns >= 54 {
        "j/k · Enter · s/i/x · m · g · w · t · ? keys · q quit"
    } else {
        "? keys · q quit"
    }
}

/// One thing the UI asked for that cannot happen on the key path (T-0074):
/// a verb to the daemon, or a fleet verb.
///
/// The main loop drains these ([`App::take_action`]) and puts the answer back
/// as a status line — and, for a listing, as rows for a panel. The UI never
/// touches a socket, so every key below is testable without a daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `Spawn` with the CLI's argument shape: id, program, args.
    Spawn {
        id: String,
        program: String,
        args: Vec<String>,
    },
    Kill {
        id: String,
    },
    Send {
        id: String,
        text: String,
    },
    MachinesList,
    MachinesRename {
        from: String,
        to: String,
    },
    MachinesRemove {
        name: String,
        force: bool,
    },
    MachinesAdd {
        code: String,
        uri: String,
    },
    TrustList,
    /// Check a grant the human asked for, and show the confirmation it needs.
    TrustPreview {
        device: String,
        role: String,
    },
    /// Everything the grant needs was checked; the human has now confirmed.
    TrustGrant {
        device: String,
        role: String,
    },
    TrustRevoke {
        device: String,
    },
}

/// A one-line prompt (T-0074). What Enter does with the text is decided by
/// [`PromptKind`]; nothing is performed until the prompt is accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub title: String,
    pub kind: PromptKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    /// `<id> <program> [args…]` — exactly the CLI's `arreo spawn` shape, so the
    /// two surfaces ask for the same thing in the same order.
    Spawn,
    /// Text to send to the (attached) pane.
    Send {
        id: String,
    },
    /// The new name for the machine the panel has selected.
    Rename {
        from: String,
    },
    /// The pairing code, then (second prompt) the invite URI (T-0058).
    AddCode,
    AddUri {
        code: String,
    },
    /// The device fingerprint to grant.
    GrantDevice,
    /// The role to grant it, once the device is known.
    GrantRole {
        device: String,
    },
    /// The device fingerprint to revoke.
    RevokeDevice,
}

/// A destructive action's confirmation (T-0074): it names what it will act on,
/// and nothing happens until the human says yes.
///
/// `force` is the second, explicit word T-0057 requires for a machine that is
/// answering right now — the key the refusal itself asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub title: String,
    pub lines: Vec<String>,
    pub yes: Action,
    pub force: Option<(char, String, Action)>,
    /// What the CLI says when the human does not confirm.
    pub cancel: Option<String>,
}

/// The machines overlay (`m`): the account's directory (T-0074).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachinesPanel {
    pub rows: Vec<Machine>,
    pub index: usize,
    /// The last verb's line — a success or the CLI's refusal.
    pub message: String,
    /// True while a verb is in flight.
    pub busy: bool,
}

/// The trust overlay (`g`): this machine's grants (T-0074).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GrantsPanel {
    pub rows: Vec<Grant>,
    pub index: usize,
    pub message: String,
    pub busy: bool,
}

impl Prompt {
    #[must_use]
    pub fn new(title: String, kind: PromptKind) -> Self {
        Self {
            title,
            kind,
            text: String::new(),
        }
    }

    #[must_use]
    pub fn with_text(title: String, kind: PromptKind, text: String) -> Self {
        Self { title, kind, text }
    }
}

impl MachinesPanel {
    fn selected(&self) -> Option<&Machine> {
        self.rows.get(self.index)
    }

    fn next(&mut self) {
        if !self.rows.is_empty() {
            self.index = (self.index + 1) % self.rows.len();
        }
    }

    fn prev(&mut self) {
        if !self.rows.is_empty() {
            self.index = (self.index + self.rows.len() - 1) % self.rows.len();
        }
    }
}

impl GrantsPanel {
    fn selected(&self) -> Option<String> {
        self.rows.get(self.index).map(|row| row.device.clone())
    }

    fn next(&mut self) {
        if !self.rows.is_empty() {
            self.index = (self.index + 1) % self.rows.len();
        }
    }

    fn prev(&mut self) {
        if !self.rows.is_empty() {
            self.index = (self.index + self.rows.len() - 1) % self.rows.len();
        }
    }
}

/// The confirmation for removing a machine (T-0057): a name is tombstoned for
/// 30 days, and a machine that is answering right now needs the explicit
/// `--force` the CLI's own refusal asks for.
fn remove_confirm(row: &Machine) -> Confirm {
    let online = row.presence == Presence::Online;
    let mut lines = vec![format!(
        "tombstone {:?}'s name for 30 days? no other key can take it until then.",
        row.name
    )];
    if online {
        lines.push(format!(
            "{} is online right now — the relay refuses a plain remove.",
            row.name
        ));
    }
    Confirm {
        title: format!("remove machine {}", row.name),
        lines,
        yes: Action::MachinesRemove {
            name: row.name.clone(),
            force: false,
        },
        force: online.then(|| {
            (
                'f',
                "tombstone it anyway".to_string(),
                Action::MachinesRemove {
                    name: row.name.clone(),
                    force: true,
                },
            )
        }),
        cancel: None,
    }
}

/// The confirmation for cutting a device's grant (T-0059). The fingerprint is
/// the whole point: revoking the wrong device is not a mistake an operator
/// should be able to make quietly.
fn revoke_confirm(device: &str) -> Confirm {
    Confirm {
        title: "revoke trust".to_string(),
        lines: vec![
            format!("cut {device}'s grant on this machine?"),
            "it keeps whatever access it has elsewhere.".to_string(),
        ],
        yes: Action::TrustRevoke {
            device: device.to_string(),
        },
        force: None,
        cancel: Some("devices revoke: not confirmed; nothing changed".to_string()),
    }
}

/// Split a spawn line the way a shell would hand argv to the program: the pane
/// id, the program, then everything else as args.
fn parse_spawn(text: &str) -> Option<(String, String, Vec<String>)> {
    let mut fields = shell_words(text)?.into_iter();
    let id = fields.next()?;
    let program = fields.next()?;
    Some((id, program, fields.collect()))
}

/// Split a line into argv fields the way the shell would: whitespace separates
/// fields; single and double quotes group a field (double quotes keep `$` and
/// allow backslash escapes); a backslash escapes the next character; an
/// unclosed quote is an error.
///
/// This is the *smallest* quoting a spawn prompt needs — a `-c` body with
/// spaces, semicolons and quotes is the shape any real agent takes, and without
/// it a TUI spawn could not express what `arreo spawn /bin/sh -c "…"` can.
fn shell_words(text: &str) -> Option<Vec<String>> {
    let mut fields: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut chars = text.chars().peekable();
    loop {
        // Skip the gap between fields.
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            if !field.is_empty() {
                fields.push(field);
            }
            return Some(fields);
        }
        field.clear();
        loop {
            match chars.peek() {
                None => {
                    if !field.is_empty() {
                        fields.push(field);
                    }
                    return Some(fields);
                }
                Some(c) if c.is_whitespace() => {
                    fields.push(field.clone());
                    break;
                }
                Some('\'') => {
                    chars.next();
                    loop {
                        match chars.next() {
                            Some('\'') => break,
                            Some(c) => field.push(c),
                            None => return None,
                        }
                    }
                }
                Some('"') => {
                    chars.next();
                    loop {
                        match chars.next() {
                            Some('"') => break,
                            Some('\\') => field.push(chars.next()?),
                            Some(c) => field.push(c),
                            None => return None,
                        }
                    }
                }
                Some('\\') => {
                    chars.next();
                    field.push(chars.next()?);
                }
                Some(c) => {
                    field.push(*c);
                    chars.next();
                }
            }
        }
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
    /// A one-shot result the operator must actually see (T-0074): the CLI's
    /// sentence for a spawned/sent/killed/fleet verb. The 1 Hz pane poll would
    /// overwrite `status` before the frame that carries it was ever drawn, so
    /// results live here and hold the line until the next keypress.
    pub result: Option<String>,
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
    /// This TUI's role (T-0074): what the daemon on the other end will allow.
    /// `owner` on the local socket (same-machine and trusted, so the daemon
    /// applies no gate), the certificate's role on a remote target — the same
    /// value the daemon's own gate reads.
    pub role: Role,
    /// This device's id, when there is one to name in a denial (a remote
    /// target; the local socket has no device).
    pub device: Option<DeviceId>,
    /// The open prompt, if any (T-0074). Modal: it consumes input until
    /// accepted or cancelled.
    pub prompt: Option<Prompt>,
    /// The open confirmation, if any (T-0074). Modal, and destructive actions
    /// are only ever performed through one.
    pub confirm: Option<Confirm>,
    /// The machines panel, if it is open (`m`).
    pub machines: Option<MachinesPanel>,
    /// The grants panel, if it is open (`g`).
    pub grants: Option<GrantsPanel>,
    /// Verbs the UI has queued for the main loop (T-0074).
    actions: VecDeque<Action>,
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
            result: None,
            view: ViewMode::Focus,
            help: false,
            sidebar_width: SIDEBAR_DEFAULT,
            dragging: false,
            screen_rows: 30,
            screen_cols: 120,
            scroll: 0,
            picker: None,
            role: Role::Owner,
            device: None,
            prompt: None,
            confirm: None,
            machines: None,
            grants: None,
            actions: VecDeque::new(),
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
        if let Some(panel) = self.machines.clone() {
            self.render_machines(frame, area, &panel);
        }
        if let Some(panel) = self.grants.clone() {
            self.render_grants(frame, area, &panel);
        }
        if let Some(prompt) = self.prompt.clone() {
            self.render_prompt(frame, area, &prompt);
        }
        if let Some(confirm) = self.confirm.clone() {
            self.render_confirm(frame, area, &confirm);
        }
        if self.help {
            self.render_help(frame, area);
        }
        self.render_status(frame, area);
    }

    /// The key list (`?`): every binding, on screen, so the TUI is complete
    /// without the docs (T-0076). Rendered over a cleared box so the frame
    /// behind it cannot be mistaken for the list.
    ///
    /// A control key a viewer may not use is **rendered disabled with the
    /// reason** (T-0074) — the daemon's own denial sentence, built from the
    /// same `role::check` its gate calls — instead of failing after the
    /// keypress.
    fn render_help(&self, frame: &mut Frame, area: Rect) {
        let width = 96.min(area.width.saturating_sub(4));
        // The gutter (a key) and the two border columns take their share, so a
        // wrapped line lands exactly where `Paragraph` would put it.
        let gutter = 12usize;
        let inner = usize::from(width).saturating_sub(2 + gutter);
        let mut lines: Vec<Line> = Vec::new();
        if let Some(reason) = self.control_denial(Verb::Send) {
            for chunk in wrap(&format!(" read-only: {reason}"), inner + gutter) {
                lines.push(Line::from(Span::styled(
                    chunk,
                    Style::default().fg(self.theme.color("error")),
                )));
            }
        }
        for (keys, what, verb) in KEY_LIST {
            let denial = verb.and_then(|verb| self.denial_reason(verb));
            let (keys_style, what_style) = match &denial {
                Some(_) => (self.theme.muted_style(), self.theme.muted_style()),
                None => (self.theme.primary_style(), self.theme.text_style()),
            };
            let what = match denial {
                Some(reason) => format!("{what} — {reason}"),
                None => (*what).to_string(),
            };
            let chunks = wrap(&what, inner);
            for (i, chunk) in chunks.iter().enumerate() {
                let gutter_text = if i == 0 {
                    format!(" {keys:<11}")
                } else {
                    " ".repeat(gutter)
                };
                lines.push(Line::from(vec![
                    Span::styled(gutter_text, keys_style),
                    Span::styled(chunk.clone(), what_style),
                ]));
            }
        }
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

    /// An overlay box centred on `area`, sized to `lines` (T-0074's panels and
    /// dialogs all use it, so they cannot disagree about where they sit).
    fn overlay(area: Rect, width: u16, lines: usize) -> Rect {
        let width = width.min(area.width.saturating_sub(2)).max(8);
        let height = (lines as u16 + 2).min(area.height.saturating_sub(2)).max(3);
        Rect {
            x: area.x + (area.width.saturating_sub(width)) / 2,
            y: area.y + (area.height.saturating_sub(height)) / 2,
            width,
            height,
        }
    }

    /// A titled box: cleared, bordered in the active color, with the body
    /// **wrapped** rather than clipped.
    ///
    /// Wrapping is not decoration here (T-0074): the boxes carry sentences —
    /// the daemon's denial for a disabled key, the CLI's refusal for a machine
    /// that is online — and a reason the operator cannot read is a reason that
    /// was not rendered at all.
    fn overlay_box(
        &self,
        frame: &mut Frame,
        area: Rect,
        width: u16,
        title: String,
        body: Vec<(String, Style)>,
        footer: Option<String>,
    ) {
        let inner = usize::from(width).saturating_sub(2);
        let mut lines: Vec<Line<'static>> = Vec::new();
        for (text, style) in body {
            for chunk in wrap(&text, inner) {
                lines.push(Line::from(Span::styled(chunk, style)));
            }
        }
        // The footer rides in the block's bottom title, which is a row of its
        // own inside the border.
        let rows = lines.len() + usize::from(footer.is_some());
        let overlay = Self::overlay(area, width, rows);
        frame.render_widget(ratatui::widgets::Clear, overlay);
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(self.theme.color("borderActive")))
            .title(title)
            .title_style(self.theme.text_style())
            .style(Style::default().bg(self.theme.color("backgroundPanel")));
        if let Some(footer) = footer {
            block = block.title_bottom(Line::from(Span::styled(
                format!(" {footer}"),
                self.theme.muted_style(),
            )));
        }
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .style(self.theme.text_style()),
            overlay,
        );
    }

    /// The machines overlay (`m`): the directory, T-0074's servers surface.
    fn render_machines(&self, frame: &mut Frame, area: Rect, panel: &MachinesPanel) {
        let now = arreo_core::identity::authority::now_ms();
        let mut body: Vec<(String, Style)> = Vec::new();
        // The last verb's line first, where a refusal is the thing to read.
        let message = match panel.busy {
            true => "working…".to_string(),
            false => panel.message.clone(),
        };
        if !message.is_empty() {
            body.push((message, self.theme.muted_style()));
        }
        if panel.rows.is_empty() {
            body.push((
                match panel.busy {
                    true => "reading the directory…".to_string(),
                    false => "(no machines)".to_string(),
                },
                self.theme.muted_style(),
            ));
        }
        for (index, row) in panel.rows.iter().enumerate() {
            let cursor = if index == panel.index { "▶" } else { " " };
            let flags = row.flags(now);
            let suffix = match flags.is_empty() {
                true => String::new(),
                false => format!("  {}", flags.join(",")),
            };
            let style = if index == panel.index {
                self.theme.text_style().add_modifier(Modifier::REVERSED)
            } else {
                self.theme.text_style()
            };
            body.push((
                format!(
                    "{cursor} {:<20} {:<8} {:>5}{suffix}",
                    row.name,
                    row.presence.as_str(),
                    crate::fleet::human_age(row.age_secs)
                ),
                style,
            ));
        }
        self.overlay_box(
            frame,
            area,
            74,
            format!("machines · {}", self.session),
            body,
            Some("a add · r rename · x remove · g trust · Esc close".to_string()),
        );
    }

    /// The trust overlay (`g`): this machine's grants, T-0074's trust surface.
    fn render_grants(&self, frame: &mut Frame, area: Rect, panel: &GrantsPanel) {
        let mut body: Vec<(String, Style)> = Vec::new();
        let message = match panel.busy {
            true => "working…".to_string(),
            false => panel.message.clone(),
        };
        if !message.is_empty() {
            body.push((message, self.theme.muted_style()));
        }
        if panel.rows.is_empty() {
            body.push((
                match panel.busy {
                    true => "reading this machine's grants…".to_string(),
                    false => "(no grant on this machine)".to_string(),
                },
                self.theme.muted_style(),
            ));
        }
        for (index, row) in panel.rows.iter().enumerate() {
            let cursor = if index == panel.index { "▶" } else { " " };
            let style = if index == panel.index {
                self.theme.text_style().add_modifier(Modifier::REVERSED)
            } else {
                self.theme.text_style()
            };
            body.push((
                format!(
                    "{cursor} {:<38} {:<9} {}",
                    row.device,
                    row.role.operator_term(),
                    if row.live { "live" } else { "revoked" }
                ),
                style,
            ));
        }
        self.overlay_box(
            frame,
            area,
            72,
            "trust · this machine".to_string(),
            body,
            Some("a grant · x revoke · Esc close".to_string()),
        );
    }

    /// A one-line prompt (`s`, `i`, and the machines/trust flows).
    fn render_prompt(&self, frame: &mut Frame, area: Rect, prompt: &Prompt) {
        let text = format!("{}▏", prompt.text);
        self.overlay_box(
            frame,
            area,
            74,
            prompt.title.clone(),
            vec![(text, self.theme.text_style())],
            Some("Enter apply · Esc cancel".to_string()),
        );
    }

    /// A destructive action's confirmation (T-0074). The lines are the CLI's
    /// own sentences where the CLI has them, wrapped because a fingerprint that
    /// gets clipped is a confirmation nobody actually confirmed.
    fn render_confirm(&self, frame: &mut Frame, area: Rect, confirm: &Confirm) {
        let mut body: Vec<(String, Style)> = confirm
            .lines
            .iter()
            .map(|text| (text.clone(), self.theme.text_style()))
            .collect();
        body.push((
            "type 'yes' to confirm, or Esc".to_string(),
            self.theme.muted_style(),
        ));
        let keys = match &confirm.force {
            Some((key, what, _)) => format!("y confirm · {key} {what} · Esc cancel"),
            None => "y confirm · Esc cancel".to_string(),
        };
        self.overlay_box(frame, area, 74, confirm.title.clone(), body, Some(keys));
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
            //
            // A viewer sees the role first (T-0074): it is the fact that decides
            // which of the keys below do anything. Then a result beats the
            // heartbeat: the operator is still reading the sentence that
            // spawned/killed sent back, and the pane count can wait (a result
            // clobbered before its frame was drawn was never a result at all).
            let status = match (
                self.role,
                self.result.as_deref().or(Some(self.status.as_str())),
            ) {
                (Role::Viewer, Some(text)) => {
                    let text = text.trim();
                    if text.is_empty() {
                        "viewer — read-only".to_string()
                    } else {
                        format!("viewer — read-only · {text}")
                    }
                }
                (_, Some(text)) => text.to_string(),
                (_, None) => String::new(),
            };
            let hints = key_hints(area.width);
            if status.is_empty() {
                hints.to_string()
            } else {
                format!("{status} · {hints}")
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
        // The fleet modals (T-0074), most specific first: a prompt or a
        // confirmation owns the keyboard while it is up, so a keypress behind it
        // can never trigger the action the human is still deciding about.
        if self.prompt.is_some() {
            self.on_key_prompt(code);
            return true;
        }
        if self.confirm.is_some() {
            self.on_key_confirm(code);
            return true;
        }
        if self.machines.is_some() {
            self.on_key_machines(code);
            return true;
        }
        if self.grants.is_some() {
            self.on_key_grants(code);
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
            K::Char('s') => {
                self.open_spawn();
                true
            }
            K::Char('i') => {
                self.open_send();
                true
            }
            K::Char('x') => {
                self.open_kill();
                true
            }
            K::Char('m') => {
                self.open_machines();
                true
            }
            K::Char('g') => {
                self.open_grants();
                true
            }
            K::Tab => {
                self.move_cursor(1);
                true
            }
            _ => true,
        }
    }

    /// The refusal the daemon would send this device for a control verb, built
    /// from the *same* core types its gate uses (T-0074): `role::check` over
    /// this TUI's role, wrapped in `VerbDenial::Role` exactly as
    /// `SessionAuth::check` wraps it. `None` means the verb is allowed — and on
    /// the local socket it always is, because the daemon applies no gate there.
    ///
    /// This is what makes a viewer's disabled actions a *rendered* fact: the
    /// reason is available before the keypress, so the UI shows it instead of
    /// letting the round trip fail.
    #[must_use]
    pub fn control_denial(&self, verb: Verb) -> Option<String> {
        let source = role::check(self.role, verb).err()?;
        Some(match &self.device {
            Some(device) => VerbDenial::Role {
                device: device.clone(),
                role: self.role,
                source,
            }
            .to_string(),
            // No device to name (the local socket): the role's own sentence is
            // the honest one.
            None => source.to_string(),
        })
    }

    /// The reason *without* the device attribution, for a row where the device
    /// is not the point ("viewer may not Spawn (needs Control)") — the key list
    /// marks the disabled keys with this so the reason fits beside the binding.
    #[must_use]
    fn denial_reason(&self, verb: Verb) -> Option<String> {
        role::check(self.role, verb).err().map(|e| e.to_string())
    }

    /// Queue a verb for the main loop, draining it with [`Self::take_action`].
    fn queue(&mut self, action: Action) {
        self.actions.push_back(action);
    }

    /// The next verb the UI wants run, if any. The main loop drains this after
    /// every key event and puts the answer back with [`Self::apply_fleet`].
    pub fn take_action(&mut self) -> Option<Action> {
        self.actions.pop_front()
    }

    /// Apply one fleet verb's answer (T-0074): the CLI's line becomes the status
    /// (so a refusal is the CLI's own sentence), the rows fill the panel the
    /// action came from, and a write that succeeded refreshes the listing it
    /// changed.
    pub fn apply_fleet(&mut self, action: &Action, outcome: Outcome) {
        // The result is the CLI's sentence, held until the next keypress so the
        // 1 Hz pane poll cannot clobber it before the frame is drawn.
        self.result = Some(outcome.message.clone());
        match action {
            Action::MachinesList => {
                let panel = self.machines.get_or_insert_with(MachinesPanel::default);
                panel.busy = false;
                panel.message = outcome.message.clone();
                panel.rows = outcome.machines;
                panel.index = panel.index.min(panel.rows.len().saturating_sub(1));
            }
            Action::MachinesRename { .. }
            | Action::MachinesRemove { .. }
            | Action::MachinesAdd { .. } => {
                if let Some(panel) = self.machines.as_mut() {
                    panel.busy = false;
                    panel.message = outcome.message.clone();
                }
                if outcome.is_ok() {
                    // The directory changed: ask again rather than patch the
                    // rows locally, so what is on screen is what the relay says.
                    self.machines
                        .get_or_insert_with(MachinesPanel::default)
                        .busy = true;
                    self.queue(Action::MachinesList);
                }
            }
            Action::TrustList => {
                let panel = self.grants.get_or_insert_with(GrantsPanel::default);
                panel.busy = false;
                panel.message = outcome.message.clone();
                panel.rows = outcome.grants;
                panel.index = panel.index.min(panel.rows.len().saturating_sub(1));
            }
            Action::TrustGrant { .. } | Action::TrustRevoke { .. } => {
                if let Some(panel) = self.grants.as_mut() {
                    panel.busy = false;
                    panel.message = outcome.message.clone();
                }
                if outcome.is_ok() {
                    self.grants.get_or_insert_with(GrantsPanel::default).busy = true;
                    self.queue(Action::TrustList);
                }
            }
            // A preview that failed its checks: the status line already carries
            // the CLI's sentence. A preview that passed never reaches here — it
            // becomes a confirmation instead.
            Action::Spawn { .. }
            | Action::Kill { .. }
            | Action::Send { .. }
            | Action::TrustPreview { .. } => {}
        }
    }

    /// `s`: spawn an agent, or say why this device may not.
    fn open_spawn(&mut self) {
        if let Some(reason) = self.control_denial(Verb::Spawn) {
            self.status = reason;
            return;
        }
        self.prompt = Some(Prompt::new(
            "spawn · <id> <program> [args…]".to_string(),
            PromptKind::Spawn,
        ));
    }

    /// `i`: send text to the attached pane, or say why this device may not.
    fn open_send(&mut self) {
        if let Some(reason) = self.control_denial(Verb::Send) {
            self.status = reason;
            return;
        }
        let Some(id) = self.model.focused_id().map(str::to_string) else {
            self.status = "send: no pane attached (Enter on a sidebar row first)".to_string();
            return;
        };
        self.prompt = Some(Prompt::new(
            format!("send to {id}"),
            PromptKind::Send { id },
        ));
    }

    /// `x`: kill the attached pane — behind a confirmation that names it.
    fn open_kill(&mut self) {
        if let Some(reason) = self.control_denial(Verb::Kill) {
            self.status = reason;
            return;
        }
        let Some(id) = self.model.focused_id().map(str::to_string) else {
            self.status = "kill: no pane attached (Enter on a sidebar row first)".to_string();
            return;
        };
        self.confirm = Some(Confirm {
            title: format!("kill pane {id}"),
            lines: vec![format!("kill pane {id:?}? its process is terminated.")],
            yes: Action::Kill { id },
            force: None,
            cancel: None,
        });
    }

    /// `m`: the account's machines (T-0074's servers surface).
    fn open_machines(&mut self) {
        self.machines = Some(MachinesPanel {
            busy: true,
            ..Default::default()
        });
        self.queue(Action::MachinesList);
    }

    /// `g`: this machine's trust grants.
    fn open_grants(&mut self) {
        self.grants = Some(GrantsPanel {
            busy: true,
            ..Default::default()
        });
        self.queue(Action::TrustList);
    }

    /// Keys inside the machines panel (T-0074). Modal, like the picker: `q` and
    /// `Esc` close it, and the wrapping prompt/confirm stack above it.
    fn on_key_machines(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode as K;
        match code {
            K::Char('j') | K::Down => {
                if let Some(panel) = self.machines.as_mut() {
                    panel.next();
                }
                return;
            }
            K::Char('k') | K::Up => {
                if let Some(panel) = self.machines.as_mut() {
                    panel.prev();
                }
                return;
            }
            _ => {}
        }
        let selected = self
            .machines
            .as_ref()
            .and_then(|panel| panel.selected().cloned());
        match code {
            K::Esc | K::Char('q') | K::Char('m') => self.machines = None,
            K::Char('a') => {
                self.prompt = Some(Prompt::new(
                    "machines add · pairing code".to_string(),
                    PromptKind::AddCode,
                ));
            }
            K::Char('r') => {
                if let Some(row) = selected {
                    self.prompt = Some(Prompt::new(
                        format!("rename {} to", row.name),
                        PromptKind::Rename { from: row.name },
                    ));
                }
            }
            K::Char('x') => {
                if let Some(row) = selected {
                    self.confirm = Some(remove_confirm(&row));
                }
            }
            K::Char('g') => {
                self.machines = None;
                self.open_grants();
            }
            _ => {}
        }
    }

    /// Keys inside the trust panel.
    fn on_key_grants(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode as K;
        match code {
            K::Char('j') | K::Down => {
                if let Some(panel) = self.grants.as_mut() {
                    panel.next();
                }
                return;
            }
            K::Char('k') | K::Up => {
                if let Some(panel) = self.grants.as_mut() {
                    panel.prev();
                }
                return;
            }
            _ => {}
        }
        let selected = self.grants.as_ref().and_then(|panel| panel.selected());
        match code {
            K::Esc | K::Char('q') | K::Char('g') => self.grants = None,
            K::Char('a') => {
                // Prefilled with the highlighted device: the common case is
                // changing the role of one this machine already knows.
                self.prompt = Some(Prompt::with_text(
                    "grant trust · device id".to_string(),
                    PromptKind::GrantDevice,
                    selected.unwrap_or_default(),
                ));
            }
            K::Char('x') => {
                if let Some(device) = selected {
                    self.confirm = Some(revoke_confirm(&device));
                }
            }
            _ => {}
        }
    }

    /// Keys while a one-line prompt is up: text, Enter to accept, Esc to cancel.
    fn on_key_prompt(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode as K;
        match code {
            K::Esc => {
                self.prompt = None;
                self.status = "cancelled".to_string();
            }
            K::Enter => self.accept_prompt(),
            K::Backspace => {
                if let Some(prompt) = self.prompt.as_mut() {
                    prompt.text.pop();
                }
            }
            K::Char(c) => {
                if let Some(prompt) = self.prompt.as_mut() {
                    prompt.text.push(c);
                }
            }
            _ => {}
        }
    }

    /// Keys while a confirmation is up: an explicit yes (or the force key), or
    /// nothing happens.
    fn on_key_confirm(&mut self, code: crossterm::event::KeyCode) {
        use crossterm::event::KeyCode as K;
        let Some(confirm) = self.confirm.as_ref() else {
            return;
        };
        match code {
            K::Char('y') | K::Enter => {
                let yes = confirm.yes.clone();
                self.confirm = None;
                self.queue(yes);
            }
            K::Char('f') => {
                if let Some((_, _, action)) = &confirm.force {
                    let action = action.clone();
                    self.confirm = None;
                    self.queue(action);
                }
            }
            K::Esc | K::Char('n') | K::Char('q') => {
                // The CLI's own line for a human who did not confirm, so a
                // cancelled grant reads the same as one declined at the CLI.
                let cancel = confirm.cancel.clone();
                self.confirm = None;
                if let Some(line) = cancel {
                    self.status = line;
                }
            }
            _ => {}
        }
    }

    /// Accept the open prompt: the text becomes the next step of the flow — a
    /// verb, or the following prompt.
    fn accept_prompt(&mut self) {
        let Some(prompt) = self.prompt.take() else {
            return;
        };
        let text = prompt.text.trim().to_string();
        match prompt.kind {
            PromptKind::Spawn => match parse_spawn(&text) {
                Some((id, program, args)) => self.queue(Action::Spawn { id, program, args }),
                None => self.status = "spawn: usage is <id> <program> [args…]".to_string(),
            },
            PromptKind::Send { id } => {
                if text.is_empty() {
                    self.status = format!("send: nothing to send to {id}");
                } else {
                    self.queue(Action::Send { id, text });
                }
            }
            PromptKind::Rename { from } => {
                if text.is_empty() {
                    self.status = "machines rename: the new name is empty".to_string();
                } else {
                    self.queue(Action::MachinesRename { from, to: text });
                }
            }
            PromptKind::AddCode => {
                if text.is_empty() {
                    self.status = "machines add: needs the pairing code".to_string();
                } else {
                    self.prompt = Some(Prompt::new(
                        format!("machines add · invite for {text}"),
                        PromptKind::AddUri { code: text },
                    ));
                }
            }
            PromptKind::AddUri { code } => {
                if text.is_empty() {
                    self.status = "machines add: needs --uri (the invite)".to_string();
                } else {
                    self.queue(Action::MachinesAdd { code, uri: text });
                }
            }
            PromptKind::GrantDevice => {
                if text.is_empty() {
                    self.status = "machines trust: no device id given".to_string();
                } else {
                    self.prompt = Some(Prompt::new(
                        format!("role for {text} (viewer|operator, empty = viewer)"),
                        PromptKind::GrantRole { device: text },
                    ));
                }
            }
            PromptKind::GrantRole { device } => {
                // The checks (pinned, this machine, the ledger) run before the
                // confirmation, exactly as the CLI orders them.
                self.queue(Action::TrustPreview { device, role: text });
            }
            PromptKind::RevokeDevice => {
                if text.is_empty() {
                    self.status = "devices revoke: no device id given".to_string();
                } else {
                    self.confirm = Some(revoke_confirm(&text));
                }
            }
        }
    }

    /// Show the confirmation a grant that cleared every check must pass
    /// (T-0074's fingerprint confirmation). The line is the one the CLI prints
    /// before its own `confirm()`.
    pub fn show_grant_confirm(&mut self, preview: crate::fleet::GrantPreview) {
        self.confirm = Some(Confirm {
            title: "grant trust".to_string(),
            lines: vec![preview.line.clone()],
            yes: Action::TrustGrant {
                device: preview.device.display_id(),
                role: preview.role.operator_term().to_string(),
            },
            force: None,
            cancel: Some("machines trust: not confirmed; nothing changed".to_string()),
        });
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
        // The rendered row index, not the model index: pty row 0 is the block's
        // title/border, and a pane that is asking renders an extra indented line
        // under it (T-0061), so the Nth pane is not the Nth row — a click that
        // does not know that lands on the pane below the one the user pointed at.
        let row = row.saturating_sub(1);
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
                let asks = self
                    .model
                    .panes()
                    .iter()
                    .find(|p| p.id == id)
                    .is_some_and(|p| p.asking.as_deref().is_some_and(|a| !a.is_empty()));
                if asks {
                    current_row += 1;
                }
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

/// Greedy word wrap of one logical line at `width` columns.
///
/// Kept deliberately simple (spaces are breaks, glyphs are cells — the rest of
/// the renderer's width arithmetic does the same), because it only has to agree
/// with its own callers: the box's line count *is* the wrapped count, so there
/// is no second wrap implementation to drift from. Wide glyphs count as one
/// cell, which under-counts, so a box may be one column wider than planned —
/// a clipped-cell corner, not a wrong layout.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut len = 0usize;
    for word in text.split(' ') {
        let word_len = word.chars().count();
        let added = if current.is_empty() {
            word_len
        } else {
            1 + word_len
        };
        if len + added > width && !current.is_empty() {
            out.push(std::mem::take(&mut current));
            len = 0;
        }
        if !current.is_empty() {
            current.push(' ');
            len += 1;
        }
        current.push_str(word);
        len += word_len;
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
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
