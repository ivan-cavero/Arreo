//! T-0015 failing-first probes: model behavior without a terminal.
//!
//! The model (pane list + focus + scrollback + search) is pure logic —
//! tested here without rendering. Rendering is proven by the interactive
//! tmux evidence (criterion 4), not by pixel assertions.

use arreo_tui::model::{Focus, Model, PaneView};

fn panes() -> Vec<PaneView> {
    vec![
        PaneView {
            id: "a".to_string(),
            state: "working",
            ram_kb: 100,
            lines: vec!["hello".to_string()],
            ram_history: Vec::new(),
        },
        PaneView {
            id: "b".to_string(),
            state: "question",
            ram_kb: 200,
            lines: vec!["May I? [y/n]".to_string()],
            ram_history: Vec::new(),
        },
        PaneView {
            id: "c".to_string(),
            state: "idle",
            ram_kb: 50,
            lines: vec![],
            ram_history: Vec::new(),
        },
    ]
}

#[test]
fn sidebar_groups_by_state_with_question_first() {
    let mut model = Model::new();
    model.set_panes(panes());
    let groups = model.sidebar_groups();
    // Question group sorts before working before idle (attention order).
    let order: Vec<&str> = groups.iter().map(|(state, _)| *state).collect();
    assert_eq!(order, vec!["question", "working", "idle"]);
    assert_eq!(groups[0].1, vec!["b"]);
}

#[test]
fn focus_moves_with_keyboard_and_wraps() {
    let mut model = Model::new();
    model.set_panes(panes());
    assert_eq!(model.focus(), Focus::Sidebar(0));
    model.focus_next();
    assert_eq!(model.focus(), Focus::Sidebar(1));
    model.focus_next();
    model.focus_next();
    assert_eq!(model.focus(), Focus::Sidebar(0), "wraps around");
    model.focus_prev();
    assert_eq!(model.focus(), Focus::Sidebar(2), "wraps backwards");
}

#[test]
fn scrollback_search_finds_matches() {
    let mut model = Model::new();
    model.set_panes(panes());
    model.focus_pane("a");
    let hits = model.search("hello");
    assert_eq!(hits, vec![0]);
    assert!(model.search("nope").is_empty());
}

#[test]
fn delta_render_skips_unchanged_lines() {
    // The render cache: identical pane content twice → second frame reports
    // zero dirty lines (the no-full-repaints criterion, unit level).
    let mut model = Model::new();
    model.set_panes(panes());
    let first = model.render_dirty();
    assert!(first > 0, "first frame paints everything");
    let second = model.render_dirty();
    assert_eq!(second, 0, "steady state repaints nothing");
    // New output dirties exactly the changed pane.
    model.push_lines("a", &["world".to_string()]);
    assert_eq!(model.render_dirty(), 1);
}
