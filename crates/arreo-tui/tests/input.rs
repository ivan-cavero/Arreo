//! Input mapping tests: the cursor position a user clicks must focus the pane
//! rendered on that row (0-based terminal coordinates), and the key map must
//! keep `q` = quit, `/` = search with Esc cancel.

use arreo_core::theme::{Depth, Variant};
use arreo_tui::model::PaneView;
use arreo_tui::theme::ThemeState;
use arreo_tui::ui::{wall_grid, App, ViewMode, SIDEBAR_DEFAULT, SIDEBAR_MAX, SIDEBAR_MIN};
use crossterm::event::KeyCode;
use ratatui::style::Color as UiColor;

fn areo_depth(truecolor: bool) -> Depth {
    if truecolor {
        Depth::Truecolor
    } else {
        Depth::Ansi256
    }
}

fn views() -> Vec<PaneView> {
    // Attention order sorts question first, so the sidebar renders
    // beta (question), then alpha, gamma (working).
    vec![
        PaneView {
            id: "alpha".into(),
            state: "working",
            ram_kb: 3500,
            lines: vec![],
            ram_history: Vec::new(),
            asking: None,
        },
        PaneView {
            id: "beta".into(),
            state: "question",
            ram_kb: 3500,
            lines: vec!["Proceed? [y/n]".into()],
            ram_history: Vec::new(),
            asking: None,
        },
        PaneView {
            id: "gamma".into(),
            state: "working",
            ram_kb: 3500,
            lines: vec!["GAMMA".into()],
            ram_history: Vec::new(),
            asking: None,
        },
    ]
}

#[test]
fn click_on_sidebar_row_focuses_that_pane() {
    let mut app = App::new();
    app.model.set_panes(views());
    // Rendered sidebar rows (0-based): 0 = "◉ question" header, 1 = beta,
    // 2 = "● working" header, 3 = alpha, 4 = gamma.
    app.on_click(6, 4);
    assert_eq!(app.model.focused_id(), Some("gamma"));
    app.on_click(6, 1);
    assert_eq!(app.model.focused_id(), Some("beta"));
    // Group headers and the pane column are not pane targets.
    app.on_click(6, 2);
    assert_eq!(app.model.focused_id(), Some("beta"));
    app.on_click(40, 4);
    assert_eq!(app.model.focused_id(), Some("beta"));
}

#[test]
fn keys_move_focus_and_quit() {
    let mut app = App::new();
    app.model.set_panes(views());
    // Sidebar order is attention order: beta (question), alpha, gamma.
    app.on_key(KeyCode::Char('j'));
    app.on_key(KeyCode::Enter);
    assert_eq!(app.model.focused_id(), Some("alpha"));
    // From an attached pane, movement restarts at the top of the list.
    app.on_key(KeyCode::Char('j'));
    app.on_key(KeyCode::Enter);
    assert_eq!(app.model.focused_id(), Some("beta"));
    assert!(!app.on_key(KeyCode::Char('q')), "q must quit");
}

#[test]
fn search_mode_consumes_keys_until_escaped() {
    let mut app = App::new();
    app.model.set_panes(views());
    app.on_key(KeyCode::Char('/'));
    assert!(app.searching);
    // A quit key typed into the prompt is text, not a command.
    assert!(app.on_key(KeyCode::Char('q')));
    assert_eq!(app.search, "q");
    app.on_key(KeyCode::Esc);
    assert!(!app.searching);
    assert!(app.search.is_empty(), "Esc clears the query");
    assert!(
        !app.on_key(KeyCode::Char('q')),
        "q quits once out of search"
    );
}

fn many_views(n: usize) -> Vec<PaneView> {
    (0..n)
        .map(|i| PaneView {
            id: format!("p{i}"),
            state: "working",
            ram_kb: 3500,
            lines: (0..40).map(|l| format!("line-{l}")).collect(),
            ram_history: Vec::new(),
            asking: None,
        })
        .collect()
}

#[test]
fn esc_clears_an_applied_search_before_it_means_quit() {
    // The status line promises "Esc clears" while a filter is applied, so Esc
    // must do that — quitting instead would make the hint a lie (T-0076).
    let mut app = App::new();
    app.model.set_panes(views());
    app.model.focus_pane("gamma");
    app.on_key(KeyCode::Char('/'));
    for c in "GAMMA".chars() {
        app.on_key(KeyCode::Char(c));
    }
    app.on_key(KeyCode::Enter);
    assert!(!app.searching && app.search == "GAMMA");
    assert!(app.on_key(KeyCode::Esc), "Esc clears, it does not quit");
    assert!(app.search.is_empty(), "the filter is gone");
    // Nothing left to clear: now Esc is the quit key.
    assert!(!app.on_key(KeyCode::Esc));
}

#[test]
fn the_help_list_is_modal() {
    let mut app = App::new();
    app.model.set_panes(views());
    app.on_key(KeyCode::Char('?'));
    assert!(app.help);
    // Every other binding is inert while the list is up.
    app.on_key(KeyCode::Char('w'));
    assert_eq!(app.view, ViewMode::Focus, "w must not fire behind the list");
    app.on_key(KeyCode::Char('t'));
    assert!(app.picker.is_none());
    app.on_key(KeyCode::Char('?'));
    assert!(!app.help, "? closes it again");
}

#[test]
fn wall_grid_tiles_panes_near_square() {
    assert_eq!(wall_grid(1), (1, 1));
    assert_eq!(wall_grid(2), (1, 2));
    assert_eq!(wall_grid(4), (2, 2));
    assert_eq!(wall_grid(5), (2, 3));
    assert_eq!(wall_grid(9), (3, 3));
    assert_eq!(wall_grid(10), (3, 4));
}

#[test]
fn wall_click_focuses_the_tile_under_the_cursor() {
    let mut app = App::new();
    app.model.set_panes(many_views(4));
    app.view = ViewMode::Wall;
    app.screen_rows = 30;
    app.screen_cols = 120;
    let body_rows = 29; // status line owns row 29
    let region_start = app.sidebar_width;
    let region_cols = app.screen_cols - region_start;
    // Tiles are 2x2: click the middle of each quadrant.
    let click = |app: &mut App, r: u16, c: u16| {
        let row = body_rows / 4 + r * (body_rows / 2);
        let col = region_start + region_cols / 4 + c * (region_cols / 2);
        app.on_click(col, row);
    };
    click(&mut app, 0, 0);
    assert_eq!(app.model.focused_id(), Some("p0"));
    click(&mut app, 0, 1);
    assert_eq!(app.model.focused_id(), Some("p1"));
    click(&mut app, 1, 0);
    assert_eq!(app.model.focused_id(), Some("p2"));
    click(&mut app, 1, 1);
    assert_eq!(app.model.focused_id(), Some("p3"));
}

#[test]
fn dragging_the_border_resizes_the_sidebar_within_bounds() {
    let mut app = App::new();
    app.model.set_panes(many_views(3));
    assert_eq!(app.sidebar_width, SIDEBAR_DEFAULT);
    // Grab the border, drag right, release.
    app.on_click(app.sidebar_width, 4);
    assert!(app.dragging);
    app.on_drag(40);
    assert_eq!(app.sidebar_width, 41);
    app.on_drag_end();
    assert!(!app.dragging);
    // A drag past the limits clamps instead of collapsing the layout — and
    // only a held button resizes at all.
    app.on_drag(5);
    assert_eq!(app.sidebar_width, 41, "an unheld drag must not resize");
    app.on_click(41, 4);
    app.on_drag(5);
    assert_eq!(app.sidebar_width, SIDEBAR_MIN);
    app.on_drag(500);
    assert_eq!(app.sidebar_width, SIDEBAR_MAX);
    // Keys are the keyboard equivalent.
    app.on_drag_end();
    app.on_drag(10);
    assert_eq!(app.sidebar_width, SIDEBAR_MAX);
    app.on_key(KeyCode::Char('['));
    assert_eq!(app.sidebar_width, SIDEBAR_MAX - 2);
    app.on_key(KeyCode::Char(']'));
    assert_eq!(app.sidebar_width, SIDEBAR_MAX);
}

#[test]
fn wall_toggle_and_scrollback() {
    let mut app = App::new();
    app.model.set_panes(many_views(2));
    app.model.focus_pane("p0");
    assert_eq!(app.view, ViewMode::Focus);
    app.on_key(KeyCode::Char('w'));
    assert_eq!(app.view, ViewMode::Wall);
    app.on_key(KeyCode::Char('w'));
    assert_eq!(app.view, ViewMode::Focus);
    // Wheel up scrolls back, is bounded by the buffer, and End returns live.
    app.on_scroll(10);
    assert_eq!(app.scroll, 10);
    app.on_scroll(1000);
    assert_eq!(app.scroll, 40);
    app.on_scroll(-5);
    assert_eq!(app.scroll, 35);
    app.on_key(KeyCode::End);
    assert_eq!(app.scroll, 0);
    // Moving focus resets the viewport to live output.
    app.on_scroll(20);
    app.on_key(KeyCode::Char('j'));
    assert_eq!(app.scroll, 0);
}

#[test]
fn picker_opens_navigates_applies_and_cancels() {
    let mut app = App::new();
    // Pin the depth: the test process may run under NO_COLOR, and this test
    // is about the picker, not about detection.
    app.theme = ThemeState::with_depth(Depth::Truecolor, Variant::Dark);
    app.model.set_panes(many_views(2));
    let names = app.theme.names();
    assert!(names.len() >= 5, "expected the built-ins, got {names:?}");
    let original = app.theme.theme().name().to_string();
    let original_primary = app.theme.color("primary");

    // `t` opens it; the cursor starts on the active theme.
    app.on_key(KeyCode::Char('t'));
    let picker = app.picker.as_ref().expect("picker open");
    assert_eq!(picker.selected(), Some(original.as_str()));

    // j/k move and wrap.
    app.on_key(KeyCode::Char('k'));
    assert_eq!(
        app.picker.as_ref().expect("open").selected(),
        Some(names.last().expect("non-empty").as_str())
    );
    app.on_key(KeyCode::Char('j'));
    assert_eq!(
        app.picker.as_ref().expect("open").selected(),
        Some(original.as_str())
    );

    // Enter applies the highlighted theme and closes the picker.
    app.on_key(KeyCode::Char('j'));
    let wanted = names.get(1).expect("at least two").clone();
    app.on_key(KeyCode::Enter);
    assert!(app.picker.is_none());
    assert_eq!(app.theme.theme().name(), wanted);
    assert_ne!(
        app.theme.color("primary"),
        original_primary,
        "palette changed"
    );

    // Esc cancels back to the theme the picker opened with.
    app.on_key(KeyCode::Char('t'));
    app.on_key(KeyCode::Char('j'));
    app.on_key(KeyCode::Esc);
    assert!(app.picker.is_none());
    assert_eq!(
        app.theme.theme().name(),
        wanted,
        "cancel restores the open theme"
    );

    // `/theme` is the documented command path to the same picker.
    app.on_key(KeyCode::Char('/'));
    for c in "theme".chars() {
        app.on_key(KeyCode::Char(c));
    }
    assert!(app.searching, "still typing in the prompt");
    app.on_key(KeyCode::Enter);
    assert!(app.picker.is_some(), "/theme must open the picker");
    app.on_key(KeyCode::Esc);
    assert!(app.picker.is_none());
}

#[test]
fn depth_reaches_every_widget_through_the_theme_state() {
    // The TUI never hardcodes a color: switching the depth re-quantizes what
    // the sidebar asks for.
    let truecolor = ThemeState::with_depth(areo_depth(true), Variant::Dark);
    let ansi = ThemeState::with_depth(areo_depth(false), Variant::Dark);
    assert!(matches!(truecolor.color("working"), UiColor::Rgb(_, _, _)));
    assert!(matches!(ansi.color("working"), UiColor::Indexed(_)));
}
