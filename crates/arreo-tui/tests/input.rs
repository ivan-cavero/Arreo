//! Input mapping tests: the cursor position a user clicks must focus the pane
//! rendered on that row (0-based terminal coordinates), and the key map must
//! keep `q` = quit, `/` = search with Esc cancel.

use arreo_tui::model::PaneView;
use arreo_tui::ui::{wall_grid, App, ViewMode, SIDEBAR_DEFAULT, SIDEBAR_MAX, SIDEBAR_MIN};
use crossterm::event::KeyCode;

fn views() -> Vec<PaneView> {
    // Attention order sorts question first, so the sidebar renders
    // beta (question), then alpha, gamma (working).
    vec![
        PaneView {
            id: "alpha".into(),
            state: "working",
            ram_kb: 3500,
            lines: vec![],
        },
        PaneView {
            id: "beta".into(),
            state: "question",
            ram_kb: 3500,
            lines: vec!["Proceed? [y/n]".into()],
        },
        PaneView {
            id: "gamma".into(),
            state: "working",
            ram_kb: 3500,
            lines: vec!["GAMMA".into()],
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
        })
        .collect()
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
