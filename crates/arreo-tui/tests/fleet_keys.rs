//! T-0074: the fleet keys, as values.
//!
//! Every key below is asserted against the `Action` it queues and the frame it
//! draws — no daemon, no relay, no socket. That is the point of the UI's design:
//! a key that acts on the fleet pushes an action onto a queue, so what a
//! keypress *means* is testable here, and the main loop's side of the contract
//! (running the verb, printing the CLI's line) is the slice's job.

use arreo_core::identity::authority::VerbDenial;
use arreo_core::identity::role::{self, Verb};
use arreo_core::identity::{DeviceId, DeviceKey, Role};
use arreo_core::theme::{Depth, Variant};
use arreo_tui::fleet::{Code, Grant, Machine, Outcome};
use arreo_tui::model::PaneView;
use arreo_tui::theme::ThemeState;
use arreo_tui::ui::{Action, App, PromptKind, ViewMode};
use crossterm::event::KeyCode;
use ratatui::backend::TestBackend;
use ratatui::buffer::{Buffer, Cell};
use ratatui::layout::Position;
use ratatui::Terminal;

fn views() -> Vec<PaneView> {
    vec![PaneView {
        id: "alpha".into(),
        state: "working",
        ram_kb: 1024,
        lines: vec!["alpha output".into()],
        ram_history: Vec::new(),
        asking: None,
    }]
}

/// An owner-facing app with one pane attached: the shape every keys test starts
/// from.
fn owner_app() -> App {
    let mut app = App::new();
    app.model.set_panes(views());
    app.model.focus_pane("alpha");
    app.theme = ThemeState::with_depth(Depth::Truecolor, Variant::Dark);
    app
}

fn draw(app: &mut App, rows: u16, cols: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).expect("test terminal");
    terminal.draw(|frame| app.render(frame)).expect("draw");
    terminal.backend().buffer().clone()
}

fn screen(buffer: &Buffer) -> String {
    (0..buffer.area.height)
        .map(|row| {
            (0..buffer.area.width)
                .map(|x| {
                    buffer
                        .cell(Position::new(x, row))
                        .map(Cell::symbol)
                        .unwrap_or(" ")
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.on_key(KeyCode::Char(c));
    }
}

#[test]
fn s_prompts_for_a_spawn_in_the_clis_own_argument_shape() {
    let mut app = owner_app();
    app.on_key(KeyCode::Char('s'));
    let prompt = app.prompt.as_ref().expect("a prompt is open");
    assert_eq!(prompt.kind, PromptKind::Spawn);
    type_text(&mut app, "web /bin/sh -c sleep-forever");
    app.on_key(KeyCode::Enter);
    assert!(app.prompt.is_none(), "the prompt closed on Enter");
    assert_eq!(
        app.take_action(),
        Some(Action::Spawn {
            id: "web".into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep-forever".into()],
        })
    );
}

#[test]
fn a_spawn_line_without_a_program_queues_nothing() {
    let mut app = owner_app();
    app.on_key(KeyCode::Char('s'));
    type_text(&mut app, "web");
    app.on_key(KeyCode::Enter);
    assert_eq!(app.take_action(), None, "half a spawn is not a spawn");
    assert!(
        app.status.contains("usage is <id> <program>"),
        "{}",
        app.status
    );
}

#[test]
fn a_spawn_line_accepts_a_quoted_c_body_like_the_cli_does() {
    let mut app = owner_app();
    app.on_key(KeyCode::Char('s'));
    type_text(&mut app, "web /bin/sh -c 'echo hi; sleep 60'");
    app.on_key(KeyCode::Enter);
    assert_eq!(
        app.take_action(),
        Some(Action::Spawn {
            id: "web".into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "echo hi; sleep 60".into()],
        })
    );
    // An unclosed quote is an error, not a truncated spawn.
    let mut app = owner_app();
    app.on_key(KeyCode::Char('s'));
    type_text(&mut app, "web /bin/sh -c 'echo hi");
    app.on_key(KeyCode::Enter);
    assert_eq!(app.take_action(), None, "an unclosed quote spawns nothing");
    assert!(
        app.status.contains("usage is <id> <program>"),
        "{}",
        app.status
    );
}

#[test]
fn i_prompts_for_text_and_targets_the_attached_pane() {
    let mut app = owner_app();
    app.on_key(KeyCode::Char('i'));
    assert_eq!(
        app.prompt.as_ref().map(|p| p.kind.clone()),
        Some(PromptKind::Send { id: "alpha".into() })
    );
    type_text(&mut app, "echo hi");
    app.on_key(KeyCode::Enter);
    assert_eq!(
        app.take_action(),
        Some(Action::Send {
            id: "alpha".into(),
            text: "echo hi".into(),
        })
    );
}

#[test]
fn i_without_an_attached_pane_says_so_instead_of_queueing() {
    let mut app = App::new();
    app.on_key(KeyCode::Char('i'));
    assert!(app.prompt.is_none());
    assert_eq!(app.take_action(), None);
    assert!(app.status.contains("no pane attached"), "{}", app.status);
}

#[test]
fn x_confirms_and_the_confirmation_names_the_pane() {
    let mut app = owner_app();
    app.on_key(KeyCode::Char('x'));
    let confirm = app.confirm.as_ref().expect("a confirmation is open");
    assert!(
        confirm.title.contains("alpha") && confirm.lines.iter().any(|l| l.contains("alpha")),
        "the confirmation names the pane it will kill: {confirm:?}"
    );
    assert_eq!(confirm.yes, Action::Kill { id: "alpha".into() });
    // Nothing happened yet.
    assert_eq!(app.take_action(), None);
    app.on_key(KeyCode::Char('y'));
    assert!(app.confirm.is_none());
    assert_eq!(app.take_action(), Some(Action::Kill { id: "alpha".into() }));
}

#[test]
fn esc_cancels_a_destructive_confirmation_without_queueing_it() {
    let mut app = owner_app();
    app.on_key(KeyCode::Char('x'));
    app.on_key(KeyCode::Esc);
    assert!(app.confirm.is_none(), "Esc closed the confirmation");
    assert_eq!(app.take_action(), None, "a cancelled kill kills nothing");
}

#[test]
fn m_opens_the_machines_panel_and_asks_for_the_directory() {
    let mut app = owner_app();
    app.on_key(KeyCode::Char('m'));
    let panel = app.machines.as_ref().expect("the panel is open");
    assert!(panel.busy, "the panel says it is reading");
    assert_eq!(app.take_action(), Some(Action::MachinesList));

    // The answer fills the panel with what the relay said.
    let outcome = Outcome {
        code: Code::Ok,
        message: "2 machine(s) from the relay".into(),
        machines: vec![
            machine("workbox", "online", 2, false),
            machine("the-pi", "stale", 4000, false),
        ],
        grants: Vec::new(),
    };
    app.apply_fleet(&Action::MachinesList, outcome);
    let panel = app.machines.as_ref().expect("still open");
    assert!(!panel.busy);
    assert_eq!(panel.rows.len(), 2);
    assert_eq!(panel.rows[0].name, "workbox");
    assert_eq!(
        app.result.as_deref(),
        Some("2 machine(s) from the relay"),
        "the outcome's line is held until the next keypress"
    );

    let frame = screen(&draw(&mut app, 30, 120));
    assert!(frame.contains("workbox"), "{frame}");
    assert!(frame.contains("online"), "{frame}");
    assert!(frame.contains("name-reclaimable"), "{frame}");
}

#[test]
fn r_in_the_machines_panel_prompts_for_the_new_name() {
    let mut app = owner_app();
    app.on_key(KeyCode::Char('m'));
    let _ = app.take_action();
    app.apply_fleet(
        &Action::MachinesList,
        Outcome {
            code: Code::Ok,
            message: "1 machine(s) from the relay".into(),
            machines: vec![machine("workbox", "online", 2, false)],
            grants: Vec::new(),
        },
    );
    app.on_key(KeyCode::Char('r'));
    assert_eq!(
        app.prompt.as_ref().map(|p| p.kind.clone()),
        Some(PromptKind::Rename {
            from: "workbox".into()
        })
    );
    type_text(&mut app, "workbox-2");
    app.on_key(KeyCode::Enter);
    assert_eq!(
        app.take_action(),
        Some(Action::MachinesRename {
            from: "workbox".into(),
            to: "workbox-2".into(),
        })
    );
}

#[test]
fn removing_an_online_machine_needs_the_explicit_force_key() {
    let mut app = owner_app();
    app.on_key(KeyCode::Char('m'));
    let _ = app.take_action();
    app.apply_fleet(
        &Action::MachinesList,
        Outcome {
            code: Code::Ok,
            message: "1 machine(s) from the relay".into(),
            machines: vec![machine("workbox", "online", 2, false)],
            grants: Vec::new(),
        },
    );
    app.on_key(KeyCode::Char('x'));
    let confirm = app.confirm.as_ref().expect("a confirmation is open");
    let (_, force_label, force_action) = confirm.force.as_ref().expect("online offers force");
    assert!(force_label.contains("anyway"), "{force_label}");
    assert_eq!(
        *force_action,
        Action::MachinesRemove {
            name: "workbox".into(),
            force: true,
        }
    );
    // The plain yes is the plain remove: the relay refuses it, and the TUI shows
    // the CLI's sentence — which is what makes the safety rule and the refusal
    // parity the same fact.
    app.on_key(KeyCode::Char('y'));
    assert_eq!(
        app.take_action(),
        Some(Action::MachinesRemove {
            name: "workbox".into(),
            force: false,
        })
    );
}

#[test]
fn an_offline_machine_needs_no_force_key() {
    let mut app = owner_app();
    app.on_key(KeyCode::Char('m'));
    let _ = app.take_action();
    app.apply_fleet(
        &Action::MachinesList,
        Outcome {
            code: Code::Ok,
            message: "1 machine(s) from the relay".into(),
            machines: vec![machine("old-box", "offline", 90_000, true)],
            grants: Vec::new(),
        },
    );
    app.on_key(KeyCode::Char('x'));
    let confirm = app.confirm.as_ref().expect("a confirmation is open");
    assert!(confirm.force.is_none(), "offline needs no second word");
    app.on_key(KeyCode::Char('y'));
    assert_eq!(
        app.take_action(),
        Some(Action::MachinesRemove {
            name: "old-box".into(),
            force: false,
        })
    );
}

#[test]
fn a_write_that_succeeded_refreshes_the_listing_it_changed() {
    let mut app = owner_app();
    app.on_key(KeyCode::Char('m'));
    let _ = app.take_action();
    app.apply_fleet(
        &Action::MachinesList,
        Outcome {
            code: Code::Ok,
            message: "1 machine(s) from the relay".into(),
            machines: vec![machine("workbox", "online", 2, false)],
            grants: Vec::new(),
        },
    );
    app.on_key(KeyCode::Char('r'));
    type_text(&mut app, "renamed");
    app.on_key(KeyCode::Enter);
    assert_eq!(
        app.take_action(),
        Some(Action::MachinesRename {
            from: "workbox".into(),
            to: "renamed".into(),
        })
    );
    app.apply_fleet(
        &Action::MachinesRename {
            from: "workbox".into(),
            to: "renamed".into(),
        },
        Outcome {
            code: Code::Ok,
            message: "renamed workbox → renamed".into(),
            machines: Vec::new(),
            grants: Vec::new(),
        },
    );
    assert_eq!(
        app.take_action(),
        Some(Action::MachinesList),
        "a successful write asks the relay what it now holds"
    );
}

#[test]
fn g_opens_the_trust_panel_and_the_grant_flow_asks_device_then_role() {
    let mut app = owner_app();
    app.on_key(KeyCode::Char('g'));
    assert!(app.grants.as_ref().is_some_and(|p| p.busy));
    assert_eq!(app.take_action(), Some(Action::TrustList));

    let key = DeviceKey::generate().expect("entropy");
    let id = DeviceId::from_key(&key.public());
    app.apply_fleet(
        &Action::TrustList,
        Outcome {
            code: Code::Ok,
            message: "1 grant(s) on workbox".into(),
            machines: Vec::new(),
            grants: vec![Grant {
                device: id.display_id(),
                role: Role::Viewer,
                live: true,
            }],
        },
    );
    // `a` prefills the highlighted device: the common case is changing the role
    // of one this machine already knows.
    app.on_key(KeyCode::Char('a'));
    let prompt = app.prompt.as_ref().expect("device prompt");
    assert_eq!(prompt.kind, PromptKind::GrantDevice);
    assert_eq!(prompt.text, id.display_id());
    app.on_key(KeyCode::Enter);
    assert_eq!(
        app.prompt.as_ref().map(|p| p.kind.clone()),
        Some(PromptKind::GrantRole {
            device: id.display_id()
        })
    );
    // Empty role = viewer, the CLI's least-privilege default.
    app.on_key(KeyCode::Enter);
    assert_eq!(
        app.take_action(),
        Some(Action::TrustPreview {
            device: id.display_id(),
            role: String::new(),
        })
    );
}

#[test]
fn the_grant_confirmation_shows_the_fingerprint_and_queues_the_grant() {
    let mut app = owner_app();
    app.show_grant_confirm(arreo_tui::fleet::GrantPreview {
        device: DeviceId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").expect("id"),
        role: Role::Viewer,
        line: "grant dev_aaaa… (no grant here yet) viewer access to this machine (workbox)?"
            .to_string(),
        machine_name: "workbox".to_string(),
    });
    let confirm = app.confirm.as_ref().expect("confirmation");
    assert!(confirm.lines[0].contains("no grant here yet"));
    assert_eq!(
        confirm.yes,
        Action::TrustGrant {
            device: "dev_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            role: "viewer".into(),
        }
    );
    // Cancelling says what the CLI says when a human does not confirm.
    app.on_key(KeyCode::Esc);
    assert_eq!(app.status, "machines trust: not confirmed; nothing changed");
    assert_eq!(app.take_action(), None);
}

#[test]
fn x_in_the_trust_panel_confirms_the_revoke_by_fingerprint() {
    let mut app = owner_app();
    app.on_key(KeyCode::Char('g'));
    let _ = app.take_action();
    app.apply_fleet(
        &Action::TrustList,
        Outcome {
            code: Code::Ok,
            message: "1 grant(s) on workbox".into(),
            machines: Vec::new(),
            grants: vec![Grant {
                device: "dev_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
                role: Role::Viewer,
                live: true,
            }],
        },
    );
    app.on_key(KeyCode::Char('x'));
    let confirm = app.confirm.as_ref().expect("confirmation");
    assert!(
        confirm.lines[0].contains("dev_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "{:?}",
        confirm.lines
    );
    app.on_key(KeyCode::Char('y'));
    assert_eq!(
        app.take_action(),
        Some(Action::TrustRevoke {
            device: "dev_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        })
    );
}

#[test]
fn a_viewer_sees_the_daemons_own_denial_before_any_keypress() {
    let key = DeviceKey::generate().expect("entropy");
    let id = DeviceId::from_key(&key.public());
    let mut app = owner_app();
    app.role = Role::Viewer;
    app.device = Some(id.clone());

    // The reason *is* the daemon's refusal sentence: the same `role::check`,
    // wrapped in the same `VerbDenial::Role` the session gate builds.
    let expected = VerbDenial::Role {
        device: id.clone(),
        role: Role::Viewer,
        source: role::check(Role::Viewer, Verb::Send).expect_err("a viewer may not send"),
    }
    .to_string();
    assert_eq!(app.control_denial(Verb::Send), Some(expected.clone()));
    assert!(expected.contains("viewer may not Send"), "{expected}");

    // The key does not act, and the reason is on screen rather than in a failure
    // after the keypress.
    assert!(app.on_key(KeyCode::Char('s')), "the TUI stays up");
    assert_eq!(app.take_action(), None, "a disabled key queues nothing");
    assert!(app.prompt.is_none(), "a disabled key opens no prompt");
    assert_eq!(app.status, app.control_denial(Verb::Spawn).unwrap());

    // ...and it renders: the status line says the session is read-only, and the
    // key list marks the control bindings with the reason.
    let frame = screen(&draw(&mut app, 30, 120));
    assert!(frame.contains("viewer — read-only"), "{frame}");
    app.on_key(KeyCode::Char('?'));
    let help = screen(&draw(&mut app, 40, 120));
    assert!(
        help.contains("viewer may not Spawn (needs Control)"),
        "{help}"
    );
    assert!(
        help.contains("viewer may not Send (needs Control)"),
        "{help}"
    );
    assert!(
        help.contains("viewer may not Kill (needs Control)"),
        "{help}"
    );
    // A viewer may still read, and the keys that only read are not marked.
    assert!(help.contains("move the cursor"), "{help}");
}

#[test]
fn an_owner_is_never_disabled() {
    let mut app = owner_app();
    for verb in [Verb::Spawn, Verb::Send, Verb::Kill] {
        assert_eq!(app.control_denial(verb), None);
    }
    app.on_key(KeyCode::Char('s'));
    assert!(app.prompt.is_some(), "the owner's spawn prompt opens");
}

#[test]
fn every_documented_fleet_key_has_a_handler() {
    // The key list is the promise (T-0074's criterion 4): every key it names has
    // to do something, and this is the test that keeps the list honest.
    let mut app = owner_app();
    app.on_key(KeyCode::Char('s'));
    assert!(app.prompt.is_some(), "s spawns");
    app.on_key(KeyCode::Esc);

    let mut app = owner_app();
    app.on_key(KeyCode::Char('i'));
    assert!(app.prompt.is_some(), "i sends");
    app.on_key(KeyCode::Esc);

    let mut app = owner_app();
    app.on_key(KeyCode::Char('x'));
    assert!(app.confirm.is_some(), "x kills, after a confirmation");
    app.on_key(KeyCode::Esc);

    let mut app = owner_app();
    app.on_key(KeyCode::Char('m'));
    assert!(app.machines.is_some(), "m opens the machines panel");
    assert_eq!(app.take_action(), Some(Action::MachinesList));
    app.on_key(KeyCode::Esc);
    assert!(app.machines.is_none(), "Esc closes it");

    let mut app = owner_app();
    app.on_key(KeyCode::Char('g'));
    assert!(app.grants.is_some(), "g opens the trust panel");
    assert_eq!(app.take_action(), Some(Action::TrustList));

    // And the keys the list already promised still do what they say.
    let mut app = owner_app();
    app.on_key(KeyCode::Char('w'));
    assert_eq!(app.view, ViewMode::Wall);
    app.on_key(KeyCode::Char('t'));
    assert!(app.picker.is_some());
    app.on_key(KeyCode::Esc);
    app.on_key(KeyCode::Char('?'));
    assert!(app.help);
}

fn machine(name: &str, presence: &str, age_secs: i64, reclaimable: bool) -> Machine {
    Machine {
        name: name.to_string(),
        presence: arreo_core::mesh::Presence::parse(presence).expect("presence"),
        age_secs,
        reachable: true,
        name_conflict: false,
        tombstone_until_ms: reclaimable.then_some(0),
    }
}
