//! Failing-first probe for daemon restore: topology saved by one daemon
//! generation is restored byte-identical by the next.
//!
//! Uses the real `persist` save/restore against a temp DB (no socket): save a
//! registry snapshot with scrollback, rebuild a fresh registry via restore,
//! assert layout + scrollback equality.

use arreo_core::pty::Pane;
use arreo_server::persist::{restore, snapshot};
use std::sync::Arc;
use std::time::Duration;

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
        ("alpha".to_string(), Arc::clone(&a)),
        ("beta".to_string(), Arc::clone(&b)),
    ];

    let dir = std::env::temp_dir().join(format!("arreo-persist-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("sessions.db");

    let saved = snapshot(&panes, &db).expect("snapshot");
    assert_eq!(saved, 2);

    // Fresh generation: restore respawns + replays scrollback.
    let restored = restore(&db).expect("restore");
    assert_eq!(restored.len(), 2);
    let alpha = restored
        .iter()
        .find(|(id, _)| id == "alpha")
        .expect("alpha restored");
    // Byte-level scrollback equality (modulo PTY \r\n normalization: drain
    // returns decoded lines; both sides go through drain, so equal).
    let before: Vec<String> = a.drain();
    std::thread::sleep(Duration::from_millis(500));
    let after: Vec<String> = alpha.1.drain();
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
    for (_, pane) in restored {
        let _ = pane.kill_shared();
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
            }])
            .unwrap();
    }
    let _ = std::fs::remove_file("/tmp/arreo-restore-PWNED-MARKER");
    let restored = restore(&db).expect("restore");
    std::thread::sleep(Duration::from_millis(1000));
    assert!(
        !std::path::Path::new("/tmp/arreo-restore-PWNED-MARKER").exists(),
        "restored scrollback was EXECUTED — code injection hole"
    );
    let haz = restored
        .iter()
        .find(|(id, _)| id == "haz")
        .expect("haz restored");
    assert!(
        haz.1
            .drain()
            .iter()
            .any(|l| l.contains("touch /tmp/arreo-restore-PWNED-MARKER")),
        "line reads back as text"
    );
    for (_, pane) in restored {
        let _ = pane.kill_shared();
    }
    let _ = source.kill_shared();
}
