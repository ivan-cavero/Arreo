//! Failing-first probe for daemon restore: topology saved by one daemon
//! generation is restored byte-identical by the next.
//!
//! Uses the real `persist` save/restore against a temp DB (no socket): save a
//! registry snapshot with scrollback, rebuild a fresh registry via restore,
//! assert layout + scrollback equality. T-0072 adds the harness half: a record
//! that names a harness session is restored **on the resume argv** its adapter
//! strategy builds, and one whose strategy cannot produce a resume (no id, no
//! harness) falls back to exactly today's respawn+history.

use arreo_core::pty::Pane;
use arreo_core::state::AdapterRegistry;
use arreo_server::persist::{restore, snapshot, SnapshotPane};
use std::sync::Arc;
use std::time::Duration;

fn registry() -> &'static AdapterRegistry {
    AdapterRegistry::builtin()
}

#[test]
fn save_restore_round_trip_over_real_panes() {
    // Two live panes with committed output.
    let a = Arc::new(
        Pane::spawn(
            "/bin/sh",
            &["-c", "echo alpha-one && echo alpha-two && sleep 30"],
            80,
            24,
        )
        .unwrap(),
    );
    let b = Arc::new(Pane::spawn("/bin/sh", &["-c", "echo beta-one && sleep 30"], 80, 24).unwrap());
    std::thread::sleep(Duration::from_millis(800));
    let panes = vec![
        SnapshotPane {
            id: "alpha".to_string(),
            pane: Arc::clone(&a),
            harness: None,
            session_id: None,
        },
        SnapshotPane {
            id: "beta".to_string(),
            pane: Arc::clone(&b),
            harness: None,
            session_id: None,
        },
    ];

    let dir = std::env::temp_dir().join(format!("arreo-persist-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("sessions.db");

    let saved = snapshot(&panes, &db).expect("snapshot");
    assert_eq!(saved, 2);

    // Fresh generation: restore respawns + replays scrollback.
    let restored = restore(&db, registry()).expect("restore");
    assert_eq!(restored.len(), 2);
    let alpha = restored
        .iter()
        .find(|p| p.id == "alpha")
        .expect("alpha restored");
    // Byte-level scrollback equality (modulo PTY \r\n normalization: drain
    // returns decoded lines; both sides go through drain, so equal).
    let before: Vec<String> = a.drain();
    std::thread::sleep(Duration::from_millis(500));
    let after: Vec<String> = alpha.pane.drain();
    for line in &before {
        assert!(
            after.contains(line),
            "scrollback line {line:?} restored (got {after:?})"
        );
    }
    assert!(
        before.iter().any(|l| l.contains("alpha-one")),
        "content real: {before:?}"
    );

    // Cleanup: kill everything we spawned.
    for restored in restored {
        let _ = restored.pane.kill_shared();
    }
    let _ = a.kill_shared();
    let _ = b.kill_shared();
}

#[test]
fn restore_never_executes_scrollback() {
    // The T-0018 hazard test: scrollback containing a destructive command
    // must come back as TEXT, never run. Seed a pane whose drain holds the
    // trigger line, snapshot, restore, then assert no side effect exists
    // AND the line reads back verbatim.
    let trigger = "touch /tmp/arreo-restore-PWNED-MARKER";
    let source = Arc::new(
        Pane::spawn(
            "/bin/sh",
            &["-c", &format!("echo '{trigger}' && echo done && sleep 30")],
            80,
            24,
        )
        .unwrap(),
    );
    std::thread::sleep(Duration::from_millis(800));
    assert!(
        source
            .drain()
            .iter()
            .any(|l| l.contains("TRIGGER") || l.contains("touch")),
        "seed real"
    );

    let dir = std::env::temp_dir().join(format!("arreo-persist-haz-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("sessions.db");
    // Craft the record directly: the trigger line as pure scrollback text.
    {
        let store = arreo_core::store::SessionStore::open(&db).unwrap();
        store
            .save_topology(&[arreo_core::store::StoredPane {
                id: "haz".to_string(),
                program: "/bin/sh".to_string(),
                args: vec![],
                cols: 80,
                rows: 24,
                scrollback: vec![trigger.to_string(), "done".to_string()],
                harness: None,
                session_id: None,
            }])
            .unwrap();
    }
    let _ = std::fs::remove_file("/tmp/arreo-restore-PWNED-MARKER");
    let restored = restore(&db, registry()).expect("restore");
    std::thread::sleep(Duration::from_millis(1000));
    assert!(
        !std::path::Path::new("/tmp/arreo-restore-PWNED-MARKER").exists(),
        "restored scrollback was EXECUTED — code injection hole"
    );
    let haz = restored
        .iter()
        .find(|p| p.id == "haz")
        .expect("haz restored");
    assert!(
        haz.pane
            .drain()
            .iter()
            .any(|l| l.contains("touch /tmp/arreo-restore-PWNED-MARKER")),
        "line reads back as text"
    );
    for restored in restored {
        let _ = restored.pane.kill_shared();
    }
    let _ = source.kill_shared();
}

#[test]
fn a_pinned_record_is_resumed_on_its_resume_argv() {
    // T-0072's restore half, exercised on a real child: a record naming a
    // harness session must come back **on the resume argv** the adapter's
    // strategy builds, not merely on the recorded args.
    //
    // Adapter selection is by program basename, so a stub named `pi` in a temp
    // dir resolves the real pi adapter (the same data a live pi pane would
    // resolve), and that stub records the argv it was started with. What the
    // file then proves is exactly the criterion: the daemon constructed
    // `--session-id <the recorded id>` and passed it as argv — and, on the
    // *restore* path, did not stack a second copy of the flag on the args the
    // spawn had already written down.
    let dir = std::env::temp_dir().join(format!("arreo-persist-pin-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("sessions.db");
    let argv_log = dir.join("argv.txt");
    let stub = dir.join("pi");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nsleep 30\n",
            argv_log.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&stub).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&stub, perms).unwrap();
    }
    let session = "01a099cd-2d35-74c4-90c6-d0e3b10011b5";
    {
        let store = arreo_core::store::SessionStore::open(&db).unwrap();
        store
            .save_topology(&[arreo_core::store::StoredPane {
                id: "pinned".to_string(),
                program: stub.display().to_string(),
                // What a spawn writes: the base command plus the pin argv.
                args: vec![
                    "--mode".to_string(),
                    "json".to_string(),
                    "--session-id".to_string(),
                    session.to_string(),
                ],
                cols: 80,
                rows: 24,
                scrollback: vec!["pinned-history".to_string()],
                harness: Some("pi".to_string()),
                session_id: Some(session.to_string()),
            }])
            .unwrap();
    }
    let restored = restore(&db, registry()).expect("restore");
    assert_eq!(restored.len(), 1);
    let pinned = &restored[0];
    assert_eq!(pinned.harness.as_deref(), Some("pi"));
    assert_eq!(
        pinned.session_id.as_deref(),
        Some(session),
        "the resumed pane keeps the session the record named"
    );
    std::thread::sleep(Duration::from_millis(600));
    let argv = std::fs::read_to_string(&argv_log).expect("the resumed child ran");
    assert_eq!(
        argv.lines().collect::<Vec<_>>(),
        vec!["--mode", "json", "--session-id", session],
        "the resume argv is constructed, passed once, and carries the id"
    );
    assert!(
        pinned
            .pane
            .drain()
            .iter()
            .any(|l| l.contains("pinned-history")),
        "history is replayed alongside the resume"
    );
    for restored in restored {
        let _ = restored.pane.kill_shared();
    }
}

#[test]
fn unknown_harness_records_take_the_plain_path() {
    // The fallback criterion: a record with no harness (every pre-v8 row, and
    // every pane the universal adapter owns) must restore exactly as it did
    // before T-0072 — program + recorded args, no invented resume argv.
    let dir = std::env::temp_dir().join(format!("arreo-persist-bare-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("sessions.db");
    {
        let store = arreo_core::store::SessionStore::open(&db).unwrap();
        store
            .save_topology(&[arreo_core::store::StoredPane {
                id: "bare".to_string(),
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "echo bare-pane && sleep 30".to_string()],
                cols: 80,
                rows: 24,
                scrollback: vec!["bare-history".to_string()],
                harness: None,
                session_id: None,
            }])
            .unwrap();
    }
    let restored = restore(&db, registry()).expect("restore");
    assert_eq!(restored.len(), 1);
    let bare = &restored[0];
    assert_eq!(bare.id, "bare");
    assert_eq!(bare.harness, None);
    assert_eq!(bare.session_id, None);
    assert_eq!(
        bare.pane.spawn_spec().args,
        vec!["-c", "echo bare-pane && sleep 30"],
        "no resume argv is invented for a harness-less record"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        bare.pane.drain().iter().any(|l| l.contains("bare-pane")),
        "the plain respawn ran the recorded command"
    );
    assert!(
        bare.pane.drain().iter().any(|l| l.contains("bare-history")),
        "history is replayed as text"
    );
    for restored in restored {
        let _ = restored.pane.kill_shared();
    }
}

/// Finding A (review): a record that *names* a harness but cannot produce a
/// resume argv — the registry no longer knows the harness, or a `pin` record
/// whose id did not survive — must be loud, or the restored pane's freshness
/// is indistinguishable from a resumed session. The two shapes are stated by
/// the pure helper the restore uses, so the notice can be asserted without
/// capturing the daemon's stderr.
#[test]
fn the_no_resume_reason_names_why_a_harness_record_went_plain() {
    let reason = |harness: Option<&str>, session: Option<&str>| {
        arreo_server::persist::no_resume_reason(harness, session)
    };
    // A record that never claimed a harness stays silent: nothing was lost.
    assert_eq!(reason(None, None), None);
    assert_eq!(reason(None, Some("01a099cd-2d35-74c4-90c6-d0e3b10011b5")), None);
    // An unknown harness with an id: the registry has no strategy for it.
    let unknown = reason(Some("ghost"), Some("01a099cd-2d35-74c4-90c6-d0e3b10011b5"))
        .expect("unknown harness is loud");
    assert!(unknown.contains("no resume possible"), "{unknown}");
    assert!(unknown.contains("no resume strategy"), "{unknown}");
    assert!(!unknown.contains("01a099cd"), "{unknown}");
    assert!(
        !unknown.contains("--session-id"),
        "the notice never echoes argv: {unknown}"
    );
    // A harness with no id: the pin has nothing to resume with.
    let no_id = reason(Some("pi"), None).expect("pin without an id is loud");
    assert!(no_id.contains("no session id"), "{no_id}");
}

#[test]
fn a_resume_the_harness_refuses_falls_back_loudly_without_blocking_the_rest() {
    // T-0072's loud-fallback half, exercised end to end on a real child: a
    // record that names a harness session, run through an adapter whose resume
    // argv makes the child die non-zero (the shape of `opencode --session
    // <unknown>`), must come back on the plain respawn — and the panes after
    // it in the same store must still restore (one bad record never blocks the
    // rest, the T-0018 contract).
    //
    // The pi adapter is the one that pins, and its argv is what a real pi
    // accepts, so this test drives the fallback with a *program* that resolves
    // the opencode adapter (so the strategy is `continue`) while the recorded
    // program refuses the flag: the daemon does not know the harness will
    // refuse, which is exactly the case the grace exists for.
    let dir = std::env::temp_dir().join(format!("arreo-persist-refuse-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("sessions.db");
    let refusing = dir.join("opencode"); // basename resolves the opencode adapter
    std::fs::write(
        &refusing,
        "#!/bin/sh\n\
         for a in \"$@\"; do\n\
         \x20 case \"$a\" in --continue|--session|--fork) exit 3;; esac\n\
         done\n\
         echo plain-path-marker\n\
         sleep 30\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&refusing).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&refusing, perms).unwrap();
    }
    {
        let store = arreo_core::store::SessionStore::open(&db).unwrap();
        store
            .save_topology(&[
                arreo_core::store::StoredPane {
                    id: "refuser".to_string(),
                    program: refusing.display().to_string(),
                    args: vec![],
                    cols: 80,
                    rows: 24,
                    scrollback: vec!["refuser-history".to_string()],
                    harness: Some("opencode".to_string()),
                    session_id: Some("ses_deadbeef".to_string()),
                },
                arreo_core::store::StoredPane {
                    id: "after".to_string(),
                    program: "/bin/sh".to_string(),
                    args: vec![
                        "-c".to_string(),
                        "echo after-marker && sleep 30".to_string(),
                    ],
                    cols: 80,
                    rows: 24,
                    scrollback: vec!["after-history".to_string()],
                    harness: None,
                    session_id: None,
                },
            ])
            .unwrap();
    }
    let restored = restore(&db, registry()).expect("restore");
    assert_eq!(
        restored.len(),
        2,
        "the record after the refusing one still restored"
    );
    let refuser = restored
        .iter()
        .find(|p| p.id == "refuser")
        .expect("refuser restored (on the plain path)");
    assert_eq!(
        refuser.session_id, None,
        "a refused resume records no session — the fallback is not a claim"
    );
    std::thread::sleep(Duration::from_millis(600));
    let lines = refuser.pane.drain();
    assert!(
        lines.iter().any(|l| l.contains("plain-path-marker")),
        "the fallback respawned the recorded command for real: {lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("refuser-history")),
        "history is replayed even on the fallback path: {lines:?}"
    );
    let after = restored
        .iter()
        .find(|p| p.id == "after")
        .expect("the next record restored");
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        after
            .pane
            .drain()
            .iter()
            .any(|l| l.contains("after-marker")),
        "the record after the refusal came back live"
    );
    for restored in restored {
        let _ = restored.pane.kill_shared();
    }
    let _ = std::fs::remove_file(&refusing);
}
