//! The diff view (T-0092): the attached pane's worktree, read like a review.
//!
//! One sentence: this module turns `arreo_core::diff` into what a frame paints —
//! the resolution of "which worktree is this pane's", the rows, and the cursor
//! that walks them — with no socket and no terminal in it, so the whole surface
//! is testable without a daemon.
//!
//! ## The same answer as `arreo diff`
//!
//! The repository is this process's own directory (the CLI's `--repo`, which the
//! TUI has no flag for: the operator said which repository by starting the TUI
//! in it), and the worktree root is `[worktree] root` from the config file, else
//! the state directory's default. The order — repository, root, pane path, diff
//! — is the CLI's order, so the two verbs refuse for the same reason first. Every
//! refusal is carried in the refusing tool's own `Display`, and the file header,
//! the 6+6 gutter and git's no-newline marker are the same strings the CLI
//! prints: two surfaces that describe one worktree must not disagree about it.
//! The TUI adds the color the terminal asked for and nothing else (and never the
//! other way around — the CLI is colorless by construction, `NO_COLOR` and all).
//!
//! ## Read off the event loop
//!
//! [`read`] runs `git` (the worktree's `diff`, its `ls-files`, and one process
//! per untracked file) and is called from a blocking task in the main loop. The
//! UI thread only ever receives a finished [`State`].
//!
//! ## Not a remote view
//!
//! A pane on another machine is a worktree on another machine. That is answered
//! from the fact alone — this machine's disk is never consulted for a repository
//! that was never going to be there — and naming the machine is the whole answer.

use arreo_core::diff::{self, Change, Diff, FileDiff, Hunk};
use arreo_core::relay::config::WorktreeSettings;
use arreo_core::worktree;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use std::path::{Path, PathBuf};

use crate::theme::ThemeState;

/// The line-number gutter: `arreo diff`'s two 6-column numbers and the space
/// after them. Everything a row puts beside them — the `+`/`-`/space prefix, a
/// hunk's `@@`, git's no-newline marker — starts in this column, in both
/// consumers.
pub const GUTTER: usize = 14;

/// One read of a pane's worktree, as the caller knows it before any I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The pane whose worktree is wanted.
    pub pane: String,
    /// The directory the repository is looked for in: this process's own.
    pub dir: PathBuf,
    /// The config file this run reads (`--config`, else `$ARREO_CONFIG`), for
    /// `[worktree] root`. Resolved by the caller through `settings::config_path`,
    /// so this module has no second opinion about which file that is.
    pub config: Option<PathBuf>,
    /// The machine the panes are on, when it is not this one. `Some` means the
    /// worktree is over there and there is nothing here to read.
    pub remote: Option<String>,
}

/// What one read found, one variant per thing an operator can be looking at.
///
/// The distinctions are the point: a pane with no worktree is not a clean pane,
/// a refused read is neither, and "no changes" is only ever said about a
/// checkout that exists and was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// No pane is attached, so there is no worktree to name.
    NoPane,
    /// A read is in flight.
    Reading,
    /// The pane is on another machine.
    Remote { machine: String },
    /// The pane has no worktree where panes' worktrees live.
    NoWorktree { root: String, path: String },
    /// The worktree is there and nothing changed in it.
    Clean { path: String },
    /// Git (or the config) refused, in its own words.
    Refused { message: String },
    /// The changes. Boxed: a `Body` is every row of a diff, and a `State`
    /// travels through a channel.
    Ready(Box<Body>),
}

impl State {
    /// The worktree this state is about, when it is about one.
    #[must_use]
    fn path(&self) -> Option<&str> {
        match self {
            Self::Ready(body) => Some(&body.path),
            Self::Clean { path } => Some(path),
            _ => None,
        }
    }
}

/// A read diff, laid out as rows once — not once per frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Body {
    /// The worktree this came from.
    pub path: String,
    /// `Diff::summary()`, verbatim: the sentence the CLI prints too, hidden
    /// untracked files and all.
    pub summary: String,
    /// The row each file's header sits at, in file order.
    pub files: Vec<usize>,
    /// Every row, in order.
    rows: Vec<Row>,
    /// The longest row's text, for the sideways scroll's limit.
    cols: usize,
}

impl Body {
    /// Turn a diff into the rows a frame paints: a header per file, an `@@` per
    /// hunk, a row per line, and git's markers where the parser put them.
    fn new(path: &Path, diff: Diff) -> Self {
        let mut rows: Vec<Row> = Vec::new();
        let mut files: Vec<usize> = Vec::new();
        for file in &diff.files {
            files.push(rows.len());
            rows.push(Row::file(file));
            for hunk in &file.hunks {
                rows.push(Row::hunk(hunk));
                for line in &hunk.lines {
                    rows.push(Row::text_line(line));
                    if line.no_newline {
                        rows.push(Row::note());
                    }
                }
            }
        }
        let cols = rows
            .iter()
            .map(|row| row.text.chars().count())
            .max()
            .unwrap_or(0);
        Self {
            path: path.display().to_string(),
            summary: diff.summary(),
            files,
            rows,
            cols,
        }
    }

    /// One frame's rows: the document from `vscroll`, as many as the area has,
    /// with the gutter fixed and the text panned by `hscroll`.
    fn lines<'a>(
        &'a self,
        theme: &ThemeState,
        width: u16,
        height: u16,
        vscroll: usize,
        hscroll: usize,
    ) -> Vec<Line<'a>> {
        let width = usize::from(width);
        self.rows
            .iter()
            .skip(vscroll)
            .take(usize::from(height))
            .map(|row| row.paint(theme, width, hscroll))
            .collect()
    }
}

/// One row of the document: the gutter that stays put, and the text beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    /// The line numbers, in `arreo diff`'s 6+6 columns. Empty for a file's
    /// header, which starts at the left edge in both consumers.
    gutter: String,
    /// The line as the CLI prints it: the `+`/`-`/space prefix, then the text.
    text: String,
    ink: Ink,
}

/// What a row *is*, which decides its colors and whether it pans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ink {
    /// A file's header line.
    File,
    /// A hunk's `@@` line.
    Hunk,
    Added,
    Removed,
    Context,
    /// git's no-newline marker, which belongs to the line above it.
    Note,
}

impl Row {
    /// A file's header, character for character what `arreo diff` prints —
    /// including the binary sentence, which is a fact about the review (git
    /// could not show this file as text) rather than about the file's contents.
    fn file(file: &FileDiff) -> Self {
        let mut text = format!("{}  ", file.change.word());
        match &file.change {
            Change::Renamed { from, to, .. } | Change::Copied { from, to, .. } => {
                text.push_str(&format!("{from} -> {to}"));
            }
            Change::ModeChanged { old, new } => {
                text.push_str(&format!("{}  {old} -> {new}", file.path()));
            }
            _ => text.push_str(file.path()),
        }
        text.push_str(&format!("  +{} -{}", file.added(), file.removed()));
        if file.binary {
            text.push_str("  (binary: git could not show this file as text)");
        }
        Self {
            gutter: String::new(),
            text,
            ink: Ink::File,
        }
    }

    /// A hunk's `@@` line, in the column the diff's own text starts at: the
    /// marker and the code it introduces read as one block.
    fn hunk(hunk: &Hunk) -> Self {
        Self {
            gutter: " ".repeat(GUTTER),
            text: hunk.header(),
            ink: Ink::Hunk,
        }
    }

    /// One line of a hunk: the numbers, then the prefix, then the text.
    fn text_line(line: &diff::Line) -> Self {
        let prefix = match line.kind {
            diff::Kind::Added => '+',
            diff::Kind::Removed => '-',
            diff::Kind::Context => ' ',
        };
        let old = line.old_line.map(|n| n.to_string()).unwrap_or_default();
        let new = line.new_line.map(|n| n.to_string()).unwrap_or_default();
        // The parser keeps a CR from a CRLF file on purpose — it is the file's
        // content. A raw CR in a cell is a cursor movement inside a frame that
        // was already placed, so it would overwrite the row it was written on.
        // Shown instead of dropped: `␍` is that CR, and the operator can see the
        // line ends in one.
        let text = match line.text.strip_suffix('\r') {
            Some(rest) => format!("{prefix}{rest}\u{240d}"),
            None => format!("{prefix}{}", line.text),
        };
        Self {
            gutter: format!("{old:>6} {new:>6} "),
            text,
            ink: match line.kind {
                diff::Kind::Added => Ink::Added,
                diff::Kind::Removed => Ink::Removed,
                diff::Kind::Context => Ink::Context,
            },
        }
    }

    /// git's no-newline marker, in the column the diff's own prefix occupies:
    /// the marker annotates the line above it, so it lines up with it.
    fn note() -> Self {
        Self {
            gutter: " ".repeat(GUTTER),
            text: "\\ No newline at end of file".to_string(),
            ink: Ink::Note,
        }
    }

    /// Whether the text pans sideways. A diff line does — it can be longer than
    /// the terminal, and reading it is the whole point. A header, a hunk marker
    /// and the no-newline note do not: they are anchors, and panning them away
    /// would take the only thing on screen that says which file this is.
    fn pans(&self) -> bool {
        matches!(self.ink, Ink::Added | Ink::Removed | Ink::Context)
    }

    /// The row's two styles: the gutter's (which is where the added/removed
    /// number backgrounds belong) and the text's.
    fn styles(&self, theme: &ThemeState) -> (Style, Style) {
        let context_bg = theme.color("diffContextBg");
        match self.ink {
            // The header is the anchor a reader navigates by, so it carries the
            // theme's active color rather than a diff token: it is not a line of
            // the diff, it is a name for the block below it.
            Ink::File => (theme.primary_style(), theme.primary_style()),
            Ink::Hunk => (
                theme.muted_style(),
                Style::default().fg(theme.color("diffHunkHeader")),
            ),
            Ink::Added => (
                Style::default()
                    .fg(theme.color("diffLineNumber"))
                    .bg(theme.color("diffAddedLineNumberBg")),
                Style::default()
                    .fg(theme.color("diffAdded"))
                    .bg(theme.color("diffAddedBg")),
            ),
            Ink::Removed => (
                Style::default()
                    .fg(theme.color("diffLineNumber"))
                    .bg(theme.color("diffRemovedLineNumberBg")),
                Style::default()
                    .fg(theme.color("diffRemoved"))
                    .bg(theme.color("diffRemovedBg")),
            ),
            Ink::Context => (
                Style::default()
                    .fg(theme.color("diffLineNumber"))
                    .bg(context_bg),
                Style::default()
                    .fg(theme.color("diffContext"))
                    .bg(context_bg),
            ),
            Ink::Note => (
                theme.muted_style(),
                Style::default().fg(theme.color("diffContext")),
            ),
        }
    }

    /// One painted row: the gutter (fixed), the text (panned, when it pans) and
    /// — for the rows that carry a background — the padding that takes that
    /// background to the right edge, so an added block reads as a block from
    /// across the room rather than as a colored sentence.
    fn paint<'a>(&'a self, theme: &ThemeState, width: usize, hscroll: usize) -> Line<'a> {
        let (gutter_style, text_style) = self.styles(theme);
        let mut spans: Vec<Span<'a>> = Vec::with_capacity(3);
        let mut used = 0;
        if !self.gutter.is_empty() {
            used += self.gutter.chars().count();
            spans.push(Span::styled(self.gutter.as_str(), gutter_style));
        }
        if self.pans() && hscroll > 0 {
            // Owned, and only for the rows the operator actually panned: every
            // other row borrows its text straight out of the document.
            let rest = self.text.chars().skip(hscroll).collect::<String>();
            used += rest.chars().count();
            spans.push(Span::styled(rest, text_style));
        } else {
            used += self.text.chars().count();
            spans.push(Span::styled(self.text.as_str(), text_style));
        }
        // Counted in chars, not cells: a line with wide glyphs pads a little
        // short, which ends a background early rather than lying about the text.
        if self.pans() && used < width {
            spans.push(Span::styled(" ".repeat(width - used), text_style));
        }
        Line::from(spans)
    }
}

/// The diff view's own state: which pane it is about, what the last read
/// answered, and where in the document the operator is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffView {
    /// The attached pane this view is about. `None` before any read, and while
    /// nothing is attached.
    pane: Option<String>,
    state: State,
    /// Index into `Body::files` of the file at the top of the viewport.
    file: usize,
    vscroll: usize,
    hscroll: usize,
}

impl Default for DiffView {
    /// A view that has never been opened has no pane attached, which is what
    /// its body says.
    fn default() -> Self {
        Self {
            pane: None,
            state: State::NoPane,
            file: 0,
            vscroll: 0,
            hscroll: 0,
        }
    }
}

impl DiffView {
    /// Nothing is attached: nothing to ask for, and the view says so.
    pub fn no_pane(&mut self) {
        self.pane = None;
        self.reset(State::NoPane);
    }

    /// A read has been asked for: the view is about `pane` until told otherwise.
    pub fn reading(&mut self, pane: &str) {
        self.pane = Some(pane.to_string());
        self.reset(State::Reading);
    }

    /// Take a read's answer for `pane`. An answer for a pane the operator has
    /// moved on from is dropped: showing one pane's changes under another pane's
    /// name is the one thing this view must never do.
    pub fn apply(&mut self, pane: &str, state: State) {
        if self.pane.as_deref() != Some(pane) {
            return;
        }
        self.reset(state);
    }

    fn reset(&mut self, state: State) {
        self.state = state;
        self.file = 0;
        self.vscroll = 0;
        self.hscroll = 0;
    }

    /// What the last read answered. The human-facing presentation of this is
    /// [`Self::title`], [`Self::status`] and [`Self::lines`]; this is the same
    /// fact for a caller that has to *decide* on it (the tests, and the
    /// acceptance runner that drives the binary).
    #[must_use]
    pub fn state(&self) -> &State {
        &self.state
    }

    /// Which file of the document the top of the viewport is in (0-based): the
    /// half of `file N/M` that is not in the summary.
    #[must_use]
    pub fn file(&self) -> usize {
        self.file
    }

    /// What the title says: the pane, and which of its worktrees. The path is
    /// the fact no row of the diff carries, and what tells two panes apart.
    #[must_use]
    pub fn title(&self) -> String {
        match (self.pane.as_deref(), self.state.path()) {
            (Some(pane), Some(path)) => format!("diff {pane} · {path}"),
            (Some(pane), None) => format!("diff {pane}"),
            (None, _) => "diff".to_string(),
        }
    }

    /// The status line's own part of this view: what changed, and where in the
    /// reading the operator is. `None` when there is no document — the body
    /// carries the sentence then, and the status line is the daemon's.
    #[must_use]
    pub fn status(&self) -> Option<String> {
        let State::Ready(body) = &self.state else {
            return None;
        };
        Some(format!(
            "{} · file {}/{}",
            body.summary,
            self.file + 1,
            body.files.len()
        ))
    }

    /// One frame's rows: the document, or the one sentence that says why there
    /// is none.
    #[must_use]
    pub fn lines<'a>(&'a self, theme: &ThemeState, width: u16, height: u16) -> Vec<Line<'a>> {
        let pane = self.pane.as_deref().unwrap_or_default();
        match &self.state {
            State::Ready(body) => body.lines(theme, width, height, self.vscroll, self.hscroll),
            State::NoPane => message(
                "(no pane attached — Enter on a sidebar row)",
                theme.muted_style(),
                width,
            ),
            State::Reading => message(
                &format!("reading the diff of {pane}…"),
                theme.muted_style(),
                width,
            ),
            State::Remote { machine } => message(
                &format!(
                    "pane {pane:?} is on {machine}: its worktree is on that machine, not this one"
                ),
                theme.text_style(),
                width,
            ),
            // The pane's own line, in the CLI's words: "no changes" here would
            // report a pane that was never spawned as a reviewed one.
            State::NoWorktree { root, path } => message(
                &format!(
                    "pane {pane:?} has no worktree under {root} (nothing registered at {path})"
                ),
                theme.text_style(),
                width,
            ),
            State::Clean { path } => {
                message(&format!("no changes in {path}"), theme.text_style(), width)
            }
            State::Refused { message: why } => {
                message(why, Style::default().fg(theme.color("error")), width)
            }
        }
    }

    /// Rows the document has (0 when there is no document).
    fn rows(&self) -> usize {
        match &self.state {
            State::Ready(body) => body.rows.len(),
            _ => 0,
        }
    }

    /// Where the viewport may start at most: the last full screenful, so the end
    /// of a diff is reachable without scrolling past it into blank space.
    fn limit(&self, visible: u16) -> usize {
        self.rows().saturating_sub(usize::from(visible))
    }

    /// Scroll by `delta` rows. Positive is forward, like every pager.
    pub fn scroll(&mut self, delta: i32, visible: u16) {
        self.vscroll = step(self.vscroll, delta).min(self.limit(visible));
        self.sync_file();
    }

    pub fn home(&mut self) {
        self.vscroll = 0;
        self.file = 0;
    }

    pub fn end(&mut self, visible: u16) {
        self.vscroll = self.limit(visible);
        self.sync_file();
    }

    /// The next file's header at the top of the viewport — walking files is
    /// what "which file am I reading" needs when a diff is longer than the
    /// screen.
    pub fn next_file(&mut self, visible: u16) {
        self.goto(self.file + 1, visible);
    }

    pub fn prev_file(&mut self, visible: u16) {
        self.goto(self.file.saturating_sub(1), visible);
    }

    fn goto(&mut self, index: usize, visible: u16) {
        let Some(start) = self.file_start(index) else {
            return;
        };
        self.vscroll = start.min(self.limit(visible));
        self.sync_file();
    }

    /// The row a file's header sits at, for an index clamped into range.
    fn file_start(&self, index: usize) -> Option<usize> {
        let State::Ready(body) = &self.state else {
            return None;
        };
        body.files
            .get(index.min(body.files.len().checked_sub(1)?))
            .copied()
    }

    /// Which file the top of the viewport is in: the last one whose header is at
    /// or above it. Scrolling by lines moves through files, and `file N/M` has
    /// to move with it or it is a number about nothing.
    fn sync_file(&mut self) {
        let State::Ready(body) = &self.state else {
            return;
        };
        let containing = body.files.partition_point(|start| *start <= self.vscroll);
        self.file = containing.saturating_sub(1);
    }

    /// Sideways scroll, in columns: the offset a long line is read at. The limit
    /// leaves the longest row's end at the right edge, so `l` cannot pan into
    /// emptiness.
    pub fn pan(&mut self, delta: i32, visible: u16) {
        let State::Ready(body) = &self.state else {
            return;
        };
        let limit = body.cols.saturating_sub(usize::from(visible));
        self.hscroll = step(self.hscroll, delta).min(limit);
    }
}

/// `value` moved by `delta` rows/columns, saturating at both ends.
fn step(value: usize, delta: i32) -> usize {
    match delta >= 0 {
        true => value.saturating_add(delta.unsigned_abs() as usize),
        false => value.saturating_sub(delta.unsigned_abs() as usize),
    }
}

/// A one- or few-sentence body, wrapped to the area: a refusal names paths and
/// can be several lines of its own (a TOML error, git's advice), so each of its
/// lines is wrapped in turn — a newline smuggled inside one row would be a
/// zero-width glyph that jams the words around it together.
fn message<'a>(text: &str, style: Style, width: u16) -> Vec<Line<'a>> {
    text.lines()
        .flat_map(|line| crate::ui::wrap(line, usize::from(width)))
        .map(|line| Line::from(Span::styled(line, style)))
        .collect()
}

/// The worktree root: `[worktree] root` from the config file this run reads,
/// else the state directory's default.
///
/// A named file that cannot be read is refused rather than defaulted, in the
/// CLI's own sentence: the operator named the file, and looking under the wrong
/// root is a worse answer than saying the configuration is broken.
fn worktree_settings(config: Option<&Path>) -> Result<WorktreeSettings, String> {
    let Some(path) = config else {
        return Ok(WorktreeSettings::default());
    };
    WorktreeSettings::load(path).map_err(|e| {
        format!(
            "the configuration at {} cannot be read: {e}",
            path.display()
        )
    })
}

/// Read the pane's worktree, in the CLI's order: the repository can refuse
/// first (a TUI started outside a repository has no worktrees to list), then the
/// config, then the pane's path, then the diff itself.
#[must_use]
pub fn read(request: &Request) -> State {
    // A pane on another machine, before anything else: the repository, the
    // worktree and the diff are all over there, and looking for them here would
    // find *this* machine's worktree for a pane id that means nothing on it.
    if let Some(machine) = &request.remote {
        return State::Remote {
            machine: machine.clone(),
        };
    }
    // The configuration first, because it names the **repository** as well as
    // the root: the daemon resolves a pane's worktree against `[worktree] repo`,
    // so a view that looked for it in whatever directory the TUI happened to be
    // started in would report "no worktree" about a pane that has one — a wrong
    // answer that reads like a fact about the pane.
    let settings = match worktree_settings(request.config.as_deref()) {
        Ok(settings) => settings,
        Err(message) => return State::Refused { message },
    };
    let chosen = settings
        .repo
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| request.dir.clone());
    let repo = match worktree::repo_root(&chosen) {
        Ok(repo) => repo,
        Err(e) => {
            return State::Refused {
                message: e.to_string(),
            }
        }
    };
    let root = settings
        .root
        .map(PathBuf::from)
        .unwrap_or_else(worktree::default_root);
    let path = worktree::path_for(&root, &request.pane);
    let entry = match worktree::find(&repo, &path) {
        Ok(entry) => entry,
        Err(e) => {
            return State::Refused {
                message: e.to_string(),
            }
        }
    };
    let Some(entry) = entry else {
        return State::NoWorktree {
            root: root.display().to_string(),
            path: path.display().to_string(),
        };
    };
    match diff::worktree_diff(&entry.path) {
        Ok(changed) if changed.is_empty() => State::Clean {
            path: entry.path.display().to_string(),
        },
        Ok(changed) => State::Ready(Box::new(Body::new(&entry.path, changed))),
        Err(e) => State::Refused {
            message: e.to_string(),
        },
    }
}
