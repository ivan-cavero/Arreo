//! T-0076: what a frame does for the person reading it.
//!
//! Four of the acceptance criteria are about the rendered frame rather than
//! about logic — state never carried by color alone, focus distinct from
//! selection, stable layout and small-terminal degradation, and "no per-tick
//! full repaints". They are asserted here, against a real ratatui buffer: a
//! screenshot is evidence, not a gate, so every claim below is a `assert_*`.
//!
//! The palette's *truth* (BRAND §2 token for token) and the WCAG arithmetic
//! live in `arreo_core::theme::brand`, where the document and the engine are
//! both in reach.

use arreo_core::theme::{Depth, Variant};
use arreo_tui::model::PaneView;
use arreo_tui::settings::{self, Settings};
use arreo_tui::theme::ThemeState;
use arreo_tui::ui::{
    key_hints, sidebar_extent, App, ViewMode, PANE_MIN, SIDEBAR_DEFAULT, SIDEBAR_FLOOR,
    SIDEBAR_MAX, SIDEBAR_MIN,
};
use crossterm::event::KeyCode;
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::{Position, Size};
use ratatui::style::{Color as UiColor, Modifier};
use ratatui::Terminal;

/// The six states the engine can name.
const STATES: &[&str] = &["question", "blocked", "working", "done", "idle", "unknown"];

fn view(id: &str, state: &'static str) -> PaneView {
    PaneView {
        id: id.to_string(),
        state,
        ram_kb: 3 * 1024,
        lines: vec![format!("{id} output")],
        ram_history: Vec::new(),
        asking: None,
    }
}

fn app_at(depth: Depth) -> App {
    let mut app = App::new();
    app.theme = ThemeState::with_depth(depth, Variant::Dark);
    app.status = "3 panes".to_string();
    app
}

fn draw(app: &mut App, rows: u16, cols: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).expect("test terminal");
    terminal.draw(|frame| app.render(frame)).expect("draw");
    terminal.backend().buffer().clone()
}

/// One row of the frame as text (wide glyphs read as themselves).
fn row_text(buffer: &Buffer, row: u16) -> String {
    (0..buffer.area.width)
        .map(|x| {
            buffer
                .cell(Position::new(x, row))
                .map(Cell::symbol)
                .unwrap_or(" ")
        })
        .collect()
}

fn screen(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|row| row_text(buffer, row))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One row, restricted to a column range (the sidebar and the pane region
/// repeat the same words, so a lookup that ignored the region could pass on a
/// frame with no sidebar at all).
fn row_text_in(buffer: &Buffer, row: u16, cols: std::ops::Range<u16>) -> String {
    (cols.start..cols.end)
        .map(|x| {
            buffer
                .cell(Position::new(x, row))
                .map(Cell::symbol)
                .unwrap_or(" ")
        })
        .collect()
}

/// Where a needle sits, as (row, column in characters), searching `cols` only.
fn find_in(buffer: &Buffer, needle: &str, cols: std::ops::Range<u16>) -> Option<(u16, u16)> {
    for row in 0..buffer.area.height {
        let text = row_text_in(buffer, row, cols.clone());
        if let Some(byte) = text.find(needle) {
            return Some((row, text[..byte].chars().count() as u16 + cols.start));
        }
    }
    None
}

/// The sidebar's own columns (its left border through its right border).
fn sidebar_cols(app: &App, buffer: &Buffer) -> std::ops::Range<u16> {
    0..sidebar_extent(buffer.area.width, app.sidebar_width)
}

/// Where a needle sits in the *sidebar*, as (row, column).
fn find_sidebar(app: &App, buffer: &Buffer, needle: &str) -> Option<(u16, u16)> {
    find_in(buffer, needle, sidebar_cols(app, buffer))
}

fn cell_at(buffer: &Buffer, row: u16, col: u16) -> &Cell {
    buffer
        .cell(Position::new(col, row))
        .expect("inside the frame")
}

/// Cells the frame asked the terminal to blink.
fn blinking_cells(buffer: &Buffer) -> usize {
    buffer
        .content()
        .iter()
        .filter(|cell| cell.modifier.contains(Modifier::SLOW_BLINK))
        .count()
}

// ---------------------------------------------------------------------------
// Criterion 2: state is never color-only
// ---------------------------------------------------------------------------

#[test]
fn every_state_has_its_own_dot_shape_and_its_own_word() {
    let mut shapes = std::collections::BTreeMap::new();
    for state in STATES {
        let mut app = app_at(Depth::Truecolor);
        app.model.set_panes(vec![view("pane-a", state)]);
        let buffer = draw(&mut app, 12, 60);
        let (row, col) = find_sidebar(&app, &buffer, state)
            .unwrap_or_else(|| panic!("no {state} row in the sidebar"));
        // The word is the label, it is the state's own name, and it starts at
        // the same column whatever the state is.
        assert_eq!(col, 5, "{state} label column");
        assert_eq!(
            row_text_in(&buffer, row, col..col + state.len() as u16),
            *state
        );
        // The glyph two columns before it is the state's dot: a shape, not a
        // hue, so the row is readable with no color at all.
        let dot = cell_at(&buffer, row, col - 2).symbol().to_string();
        assert!(
            !dot.trim().is_empty() && !dot.chars().all(|c| c.is_ascii_alphanumeric()),
            "{state} has no dot shape before its label: {dot:?}"
        );
        shapes.insert(state, dot);
    }
    let distinct: std::collections::BTreeSet<&String> = shapes.values().collect();
    assert_eq!(
        distinct.len(),
        STATES.len(),
        "two states share a dot shape: {shapes:?}"
    );
}

#[test]
fn the_state_label_falls_back_to_the_neutral_when_its_hue_cannot_carry_text() {
    // `idle` is BRAND §2's muted gray: 3.5:1 on the dark page, i.e. below AA
    // for text. The dot keeps the brand hue; the *word* steps to the neutral
    // that can carry it. Every other state keeps its own hue.
    let mut app = app_at(Depth::Truecolor);
    app.model
        .set_panes(vec![view("pane-a", "idle"), view("pane-b", "question")]);
    let buffer = draw(&mut app, 12, 60);
    let (idle_row, _) = find_sidebar(&app, &buffer, "idle").expect("idle row");
    let (question_row, _) = find_sidebar(&app, &buffer, "question").expect("question row");
    // Column 5 is where the label starts (border, two-space lead, dot, space).
    let idle_label = cell_at(&buffer, idle_row, 5);
    let question_label = cell_at(&buffer, question_row, 5);
    assert_eq!(
        idle_label.fg,
        app.theme.color("textMuted"),
        "the idle label must use the neutral"
    );
    assert_eq!(
        question_label.fg,
        app.theme.color("question"),
        "question can carry its own hue as text"
    );
    // ...and the idle dot still carries the state hue.
    assert_eq!(cell_at(&buffer, idle_row, 3).fg, app.theme.color("idle"));
}

#[test]
fn a_no_color_frame_is_fully_readable_and_carries_no_color_at_all() {
    let mut app = app_at(Depth::NoColor);
    app.model.set_panes(vec![
        view("alpha", "working"),
        view("beta", "question"),
        view("gamma", "done"),
    ]);
    app.model.focus_pane("alpha");
    app.model.focus_next(); // keyboard cursor back into the sidebar
    app.model.focus_next();
    let buffer = draw(&mut app, 14, 100);
    let text = screen(&buffer);

    for needle in [
        "alpha", "beta", "gamma", // every pane is named
        "working", "question", "done", // every state is spelled out
        "◉", "●", "✓", // ...and has its shape
        "▸", // the attached pane is marked by a glyph
        "▶", // the keyboard cursor is too
        "q quit", "? keys", // the key hints survive
    ] {
        assert!(text.contains(needle), "NO_COLOR frame lacks {needle:?}");
    }
    assert_eq!(
        app.theme.depth(),
        Depth::NoColor,
        "the test must run at NO_COLOR depth"
    );
    let colored = buffer
        .content()
        .iter()
        .filter(|cell| cell.fg != UiColor::Reset || cell.bg != UiColor::Reset)
        .count();
    assert_eq!(colored, 0, "{colored} cells still carry a color");
}

// ---------------------------------------------------------------------------
// Criterion 4: focus distinct from selection, stable layout, degradation
// ---------------------------------------------------------------------------

#[test]
fn focus_and_selection_are_two_visible_facts() {
    let mut app = app_at(Depth::Truecolor);
    app.model
        .set_panes(vec![view("alpha", "working"), view("beta", "working")]);
    app.model.focus_pane("alpha"); // attached = selection
    app.model.focus_next(); // keyboard cursor into the sidebar (alpha)
    app.model.focus_next(); // ...onto beta
    assert_eq!(app.model.cursor_id(), Some("beta"));
    assert_eq!(app.model.focused_id(), Some("alpha"));

    let buffer = draw(&mut app, 12, 70);
    let (beta_row, beta_col) = find_sidebar(&app, &buffer, "beta").expect("beta row");
    let (alpha_row, _) = find_sidebar(&app, &buffer, "alpha").expect("alpha row");
    // The pane id starts at column 4: border, cursor, marker, space.
    assert_eq!(beta_col, 4);
    // The cursor row: a glyph *and* inverse video, so it is visible at every
    // depth — including NO_COLOR, where a hue difference would not be.
    assert_eq!(
        cell_at(&buffer, beta_row, 1).symbol(),
        "▶",
        "the cursor row is not marked"
    );
    assert!(
        cell_at(&buffer, beta_row, beta_col)
            .modifier
            .contains(Modifier::REVERSED),
        "the cursor row is not highlighted"
    );
    // The attached pane: a different glyph in a different column, no inverse
    // video, so a frame showing only one of the two facts could not be
    // mistaken for the other.
    assert_eq!(cell_at(&buffer, alpha_row, 1).symbol(), " ");
    assert_eq!(cell_at(&buffer, alpha_row, 2).symbol(), "▸");
    assert!(!cell_at(&buffer, alpha_row, beta_col)
        .modifier
        .contains(Modifier::REVERSED));

    // Keyboard into the pane region: the cursor leaves the sidebar, the
    // selection does not move, and the *pane border* becomes the focus cue.
    app.model.focus_pane_view();
    let buffer = draw(&mut app, 12, 70);
    assert!(
        find_sidebar(&app, &buffer, "▶").is_none(),
        "the sidebar still claims the cursor"
    );
    let (alpha_row, _) = find_sidebar(&app, &buffer, "alpha").expect("alpha row");
    assert_eq!(cell_at(&buffer, alpha_row, 2).symbol(), "▸");
    let sidebar = sidebar_extent(70, app.sidebar_width);
    let border = cell_at(&buffer, 1, sidebar);
    assert_eq!(
        border.fg,
        app.theme.color("primary"),
        "the focused pane region is not marked"
    );
}

#[test]
fn the_row_grid_is_a_function_of_the_panes_not_of_their_states() {
    // Every state, one pane: same row, same columns, same number of rows. A
    // state change that re-flowed the sidebar (a longer label, a bar that
    // moved) would fail here.
    let mut expected: Option<(u16, u16, u16, usize)> = None;
    for state in STATES {
        let mut app = app_at(Depth::Truecolor);
        app.model.set_panes(vec![view("pane-a", state)]);
        let buffer = draw(&mut app, 12, 60);
        let (row, id_col) = find_sidebar(&app, &buffer, "pane-a").expect("pane row");
        let bar_col = row_text(&buffer, row).find('█').expect("ram bar") as u16;
        let used_rows = (0..buffer.area.height)
            .filter(|r| !row_text(&buffer, *r).trim().is_empty())
            .count();
        let got = (row, id_col, bar_col, used_rows);
        match expected {
            None => expected = Some(got),
            Some(want) => assert_eq!(got, want, "{state} moved the layout"),
        }
    }
}

#[test]
fn a_question_line_appears_under_its_own_pane_and_shifts_nothing_sideways() {
    // Same group structure in both frames, so the only difference is the
    // question line itself: both panes are `question`, and alpha starts asking.
    let mut quiet = app_at(Depth::Truecolor);
    quiet
        .model
        .set_panes(vec![view("alpha", "question"), view("beta", "question")]);
    let before = draw(&mut quiet, 12, 70);
    let (beta_row, beta_col) = find_sidebar(&quiet, &before, "beta").expect("beta row");

    let mut asking = app_at(Depth::Truecolor);
    let mut alpha = view("alpha", "question");
    alpha.asking = Some("May I run this?".to_string());
    asking
        .model
        .set_panes(vec![alpha, view("beta", "question")]);
    let after = draw(&mut asking, 12, 70);
    let (asking_row, _) = find_sidebar(&asking, &after, "alpha").expect("alpha row");
    let (asked_row, asked_col) =
        find_sidebar(&asking, &after, "May I run this?").expect("question line");
    let (beta_after, beta_col_after) = find_sidebar(&asking, &after, "beta").expect("beta row");

    assert_eq!(
        asked_row,
        asking_row + 1,
        "the question sits under its pane"
    );
    assert_eq!(
        asked_col, beta_col_after,
        "the question line is indented to the pane id column, not past it"
    );
    assert_eq!(beta_col_after, beta_col, "columns do not move");
    assert_eq!(
        beta_after,
        beta_row + 1,
        "the question costs exactly one row, below its pane"
    );
}

#[test]
fn a_narrow_terminal_clamps_the_sidebar_instead_of_glitching() {
    // The rule, as a table: the sidebar yields first, then holds a floor, and
    // never takes a column the pane region needs.
    assert_eq!(sidebar_extent(120, SIDEBAR_DEFAULT), SIDEBAR_DEFAULT);
    assert_eq!(sidebar_extent(80, SIDEBAR_DEFAULT), SIDEBAR_DEFAULT);
    // Enough room for both: the sidebar is what the user asked for.
    assert_eq!(sidebar_extent(52, SIDEBAR_DEFAULT), SIDEBAR_DEFAULT);
    // Not enough: the sidebar yields, one column at a time, so the pane region
    // keeps PANE_MIN.
    assert_eq!(sidebar_extent(51, SIDEBAR_DEFAULT), 27);
    assert_eq!(sidebar_extent(42, SIDEBAR_DEFAULT), SIDEBAR_MIN);
    assert_eq!(sidebar_extent(41, SIDEBAR_DEFAULT), 17);
    // Past that the floor holds and the pane gives way, rather than the
    // sidebar vanishing or the split going negative.
    assert_eq!(sidebar_extent(36, SIDEBAR_DEFAULT), SIDEBAR_FLOOR);
    assert_eq!(sidebar_extent(20, SIDEBAR_MAX), SIDEBAR_FLOOR);
    for width in 1..=200u16 {
        let sidebar = sidebar_extent(width, SIDEBAR_MAX);
        assert!(sidebar < width.max(1), "{width} -> {sidebar}");
        assert!(sidebar <= SIDEBAR_MAX);
        // The floor holds wherever the terminal can hold it; below that the
        // sidebar is squeezed to what is left rather than overflowing.
        let floor = SIDEBAR_FLOOR.min(width.saturating_sub(1));
        assert!(sidebar >= floor, "{width} -> {sidebar}");
        // The whole point of the clamp: where the terminal can pay for both,
        // the pane region never drops below its minimum.
        if width >= SIDEBAR_FLOOR + PANE_MIN {
            assert!(
                width - sidebar >= PANE_MIN,
                "{width} -> sidebar {sidebar} starves the pane region"
            );
        }
    }

    // 80x24 is the documented minimum and must be usable: the sidebar lists
    // panes and the pane region keeps the room to read one.
    let mut app = app_at(Depth::Truecolor);
    app.model
        .set_panes(vec![view("alpha", "working"), view("beta", "question")]);
    app.model.focus_pane("alpha");
    let buffer = draw(&mut app, 24, 80);
    let text = screen(&buffer);
    assert!(text.contains("alpha") && text.contains("beta"));
    assert!(text.contains("q quit"), "the key hints must survive 80x24");
    assert!(
        !text.contains('…'),
        "the status line must not need clipping"
    );
    assert!(
        80 - sidebar_extent(80, app.sidebar_width) >= 40,
        "the pane region is not usable at 80x24"
    );
    assert!(buffer.area.width == 80 && buffer.area.height == 24);

    // And below it: clamped, not glitched. Nothing panics, no row overflows,
    // the split lands on the clamped column, and the pane region still exists.
    for (rows, cols) in [(12u16, 40u16), (8, 24), (6, 16)] {
        let buffer = draw(&mut app, rows, cols);
        let sidebar = sidebar_extent(cols, app.sidebar_width);
        assert!(cols - sidebar >= 1, "{cols}x{rows} lost the pane region");
        assert_eq!(
            cell_at(&buffer, 0, sidebar - 1).symbol(),
            "┐",
            "{cols}x{rows}: the split is not at the clamped column"
        );
        assert!(
            (0..rows).all(|row| row_text(&buffer, row).chars().count() == cols as usize),
            "{cols}x{rows} has a row that is not the frame's width"
        );
    }
}

#[test]
fn the_key_hints_fit_the_terminal_they_are_shown_on() {
    for cols in [40u16, 64, 80, 96, 120, 200] {
        let hints = key_hints(cols);
        assert!(
            hints.chars().count() <= cols as usize,
            "{cols} columns cannot hold {hints:?}"
        );
        assert!(hints.contains('q'), "{cols}: {hints:?} lost quit");
        if cols >= 64 {
            assert!(hints.contains("? keys"), "{cols}: {hints:?} lost help");
        }
    }
    // A short status message plus the legend still fits the common widths.
    let mut app = app_at(Depth::Truecolor);
    app.model.set_panes(vec![view("alpha", "working")]);
    for cols in [80u16, 120] {
        let buffer = draw(&mut app, 24, cols);
        let status = row_text(&buffer, 23);
        assert!(status.contains("q quit"), "{cols}: {status:?}");
        assert!(
            !status.contains('…'),
            "{cols} clipped the legend: {status:?}"
        );
    }
}

#[test]
fn the_key_list_is_on_screen_and_dismissable() {
    let mut app = app_at(Depth::Truecolor);
    app.model.set_panes(vec![view("alpha", "working")]);
    assert!(!app.help);
    app.on_key(KeyCode::Char('?'));
    assert!(app.help, "? must open the key list");
    let buffer = draw(&mut app, 22, 100);
    let text = screen(&buffer);
    for needle in [
        "move the cursor",
        "attach",
        "wall ↔ focus",
        "theme picker",
        "search",
        "sidebar narrower",
        "scroll",
        "quit",
    ] {
        assert!(text.contains(needle), "the key list omits {needle:?}");
    }
    // Modal: a quit key closes the list rather than the app.
    assert!(app.on_key(KeyCode::Esc));
    assert!(!app.help);
    assert!(app.on_key(KeyCode::Char('?')), "still alive");
    assert!(
        app.on_key(KeyCode::Char('q')),
        "q closes the list, not the app"
    );
    assert!(!app.help);
}

#[test]
fn the_wall_cursor_is_visible_and_moves_with_the_keyboard() {
    let mut app = app_at(Depth::Truecolor);
    app.model.set_panes(vec![
        view("alpha", "working"),
        view("beta", "working"),
        view("gamma", "done"),
    ]);
    app.view = ViewMode::Wall;
    app.model.focus_pane("alpha");
    let first = draw(&mut app, 24, 100);
    let text = screen(&first);
    assert!(text.contains("alpha [working] ▶"), "no cursor in the wall");
    assert_eq!(
        text.matches('▶').count(),
        1,
        "exactly one tile carries the cursor"
    );
    app.on_key(KeyCode::Char('j'));
    assert_eq!(app.model.focused_id(), Some("beta"));
    let second = draw(&mut app, 24, 100);
    assert!(screen(&second).contains("beta [working] ▶"));
    // Enter opens the highlighted tile full-height.
    app.on_key(KeyCode::Enter);
    assert_eq!(app.view, ViewMode::Focus);
    assert_eq!(app.model.focused_id(), Some("beta"));
}

#[test]
fn motion_is_off_when_the_switch_says_so() {
    let mut app = app_at(Depth::Truecolor);
    app.model.set_panes(vec![view("beta", "question")]);
    // Default: the question group pulses (the terminal's own slow blink).
    let pulsing = draw(&mut app, 10, 60);
    assert!(
        blinking_cells(&pulsing) > 0,
        "the question cue must pulse by default"
    );

    // `[tui] reduce_motion = true`: still.
    app.settings = Settings {
        reduce_motion: true,
    };
    let still = draw(&mut app, 10, 60);
    assert_eq!(blinking_cells(&still), 0, "reduce_motion must still it");
    // ...and the state is still on screen: the dot and the word are the cue.
    let text = screen(&still);
    assert!(text.contains("◉") && text.contains("question"));

    // NO_COLOR implies still, without anyone writing a config.
    let (no_color, problem) = settings::resolve(None, Depth::NoColor);
    assert_eq!(problem, None);
    app.settings = no_color;
    let buffer = draw(&mut app, 10, 60);
    assert_eq!(
        blinking_cells(&buffer),
        0,
        "NO_COLOR implies no motion at all"
    );
}

// ---------------------------------------------------------------------------
// Criterion 3: no per-tick full repaints
// ---------------------------------------------------------------------------

/// A backend that counts what ratatui actually asked to be painted, i.e. the
/// cells of the diff — not the size of the screen. This is how "no per-tick
/// full repaints" becomes an assertion instead of an opinion.
struct CountingBackend {
    inner: TestBackend,
    cells: usize,
    clears: usize,
}

impl CountingBackend {
    fn new(width: u16, height: u16) -> Self {
        Self {
            inner: TestBackend::new(width, height),
            cells: 0,
            clears: 0,
        }
    }
}

impl Backend for CountingBackend {
    /// `TestBackend` cannot fail, so neither can this one: the count is the
    /// only thing this wrapper adds.
    type Error = std::convert::Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        let cells: Vec<(u16, u16, &Cell)> = content.collect();
        self.cells += cells.len();
        self.inner.draw(cells.into_iter())
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.clears += 1;
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.clears += 1;
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

#[test]
fn a_steady_frame_repaints_nothing_and_never_clears_the_screen() {
    let (rows, cols) = (24u16, 100u16);
    let mut app = app_at(Depth::Truecolor);
    app.model.set_panes(vec![
        view("alpha", "working"),
        view("beta", "question"),
        view("gamma", "done"),
    ]);
    app.model.focus_pane("alpha");
    let mut terminal = Terminal::new(CountingBackend::new(cols, rows)).expect("test terminal");

    terminal
        .draw(|frame| app.render(frame))
        .expect("first frame");
    let first = terminal.backend().cells;
    assert!(
        first > 0 && first <= usize::from(rows) * usize::from(cols),
        "the first frame paints the screen: {first} cells"
    );

    // Five ticks with nothing changed: not one cell, and no clear-screen.
    for tick in 0..5 {
        terminal.backend_mut().cells = 0;
        terminal
            .draw(|frame| app.render(frame))
            .expect("idle frame");
        assert_eq!(
            terminal.backend().cells,
            0,
            "tick {tick} repainted cells while idle"
        );
    }
    assert_eq!(
        terminal.backend().clears,
        0,
        "the app must never clear the screen"
    );

    // A state change repaints the rows it changed — a bounded handful, not the
    // screen: this is the assertion that a full repaint cannot sneak back in.
    terminal.backend_mut().cells = 0;
    app.model.set_panes(vec![
        view("alpha", "done"),
        view("beta", "question"),
        view("gamma", "done"),
    ]);
    terminal
        .draw(|frame| app.render(frame))
        .expect("changed frame");
    let changed = terminal.backend().cells;
    let screen_cells = usize::from(rows) * usize::from(cols);
    assert!(changed > 0, "a state change must repaint something");
    assert!(
        changed < screen_cells / 4,
        "a state change repainted {changed}/{screen_cells} cells"
    );

    // And the same again: back to zero. The diff is the mechanism, and it is
    // what keeps the daemon's 1 Hz poll from costing a screen per second.
    terminal.backend_mut().cells = 0;
    terminal
        .draw(|frame| app.render(frame))
        .expect("settled frame");
    assert_eq!(terminal.backend().cells, 0);
    assert_eq!(terminal.backend().clears, 0);
}
