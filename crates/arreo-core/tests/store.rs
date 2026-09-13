//! T-0018 failing-first probes: session store, audit redaction, migrations.
//!
//! Written before `src/store.rs` exists — MUST fail to compile until it lands.
//!
//! Gated on the `sqlite` feature (T-0010 lite pass): the store is the one
//! C-backed module, so `cargo check --no-default-features --all-targets`
//! — the foreign-target portability gate — must not try to compile this file.
#![cfg(feature = "sqlite")]

use arreo_core::store::{AuditEvent, AuditKind, SessionStore, StoredPane};

fn panes(n: usize) -> Vec<StoredPane> {
    (0..n)
        .map(|i| StoredPane {
            id: format!("pane-{i}"),
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), format!("echo scroll-{i}")],
            cols: 80,
            rows: 24,
            scrollback: vec![format!("scroll-{i}-line-0"), format!("scroll-{i}-line-1")],
            // Universal-adapter panes: no harness, no session (T-0072). The
            // harness columns get their own round-trip test below.
            harness: None,
            session_id: None,
        })
        .collect()
}

#[test]
fn harness_and_session_round_trip_and_a_v7_row_migrates_untouched() {
    // T-0072: `panes` gained `harness` + `session_id` in place. Two halves, in
    // one test because they are one contract: a new row round-trips its
    // harness session, and a row written *before* the columns existed (a v7
    // database) migrates forward with its program/args/scrollback intact and
    // both new columns NULL — the honest value for "nothing is known about a
    // session this version never recorded".
    let dir = std::env::temp_dir().join(format!("arreo-migv8-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("migv8.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta(key, value) VALUES ('schema_version', '7');
             CREATE TABLE panes(id TEXT PRIMARY KEY, program TEXT NOT NULL,
               args TEXT NOT NULL, cols INTEGER NOT NULL, rows INTEGER NOT NULL);
             CREATE TABLE scrollback(pane TEXT NOT NULL, line_no INTEGER NOT NULL,
               text TEXT NOT NULL, PRIMARY KEY (pane, line_no));
             INSERT INTO panes VALUES ('old', '/usr/bin/vim', '[\"notes.txt\"]', 100, 30);
             INSERT INTO scrollback VALUES ('old', 0, 'old-line-0');
             INSERT INTO scrollback VALUES ('old', 1, 'old-line-1');",
        )
        .unwrap();
    }
    let store = SessionStore::open(&path).expect("v7 opens and migrates");
    assert_eq!(
        store.schema_version().expect("version"),
        arreo_core::store::SCHEMA_VERSION
    );
    let rows = store.load_topology().expect("load");
    assert_eq!(rows.len(), 1, "the v7 row survived the migration");
    assert_eq!(rows[0].id, "old");
    assert_eq!(rows[0].program, "/usr/bin/vim");
    assert_eq!(rows[0].args, vec!["notes.txt".to_string()]);
    assert_eq!(rows[0].cols, 100);
    assert_eq!(rows[0].rows, 30);
    assert_eq!(rows[0].scrollback, vec!["old-line-0", "old-line-1"]);
    assert_eq!(rows[0].harness, None, "a pre-v8 row names no harness");
    assert_eq!(rows[0].session_id, None, "and no session either");

    // A row written by this version keeps its harness session.
    let mut fresh = panes(1);
    fresh[0].harness = Some("pi".to_string());
    fresh[0].session_id = Some("01a099cd-2d35-74c4-90c6-d0e3b10011b5".to_string());
    store.save_topology(&fresh).expect("save");
    let back = store.load_topology().expect("load");
    assert_eq!(back[0].harness.as_deref(), Some("pi"));
    assert_eq!(
        back[0].session_id.as_deref(),
        Some("01a099cd-2d35-74c4-90c6-d0e3b10011b5")
    );
}

#[test]
fn save_and_restore_round_trip() {
    let store = SessionStore::open_memory().expect("open");
    store.save_topology(&panes(10)).expect("save");
    let restored = store.load_topology().expect("load");
    assert_eq!(restored.len(), 10);
    assert_eq!(
        restored[3].scrollback,
        vec!["scroll-3-line-0", "scroll-3-line-1"]
    );
    assert_eq!(restored[3].program, "/bin/sh");
}

#[test]
fn audit_log_redacts_secrets_but_keeps_shape() {
    let store = SessionStore::open_memory().expect("open");
    store
        .record(&AuditEvent {
            device: "phone".to_string(),
            agent: "pane-1".to_string(),
            prompt: "deploy with OPENAI_API_KEY=sk-abc123XYZ4567890abcdef now".to_string(),
            ..AuditEvent::new(
                arreo_core::store::actions::PROMPT,
                AuditKind::Prompt,
                arreo_core::store::AuditOutcome::Ok,
                1_700_000_000_000,
            )
        })
        .expect("audit");
    let events = store.audit_recent(10).expect("recent");
    assert_eq!(events.len(), 1);
    assert!(
        !events[0].prompt.contains("sk-abc123"),
        "key material redacted: {}",
        events[0].prompt
    );
    assert!(
        events[0].prompt.contains("OPENAI_API_KEY"),
        "field name kept for debugging: {}",
        events[0].prompt
    );
    assert!(events[0].redacted, "redaction flagged");
}

#[test]
fn audit_log_is_append_only() {
    let store = SessionStore::open_memory().expect("open");
    for i in 0..3 {
        store
            .record(&AuditEvent {
                device: "cli".to_string(),
                agent: "pane-0".to_string(),
                prompt: format!("command-{i}"),
                ..AuditEvent::new(
                    arreo_core::store::actions::PROMPT,
                    AuditKind::Prompt,
                    arreo_core::store::AuditOutcome::Ok,
                    1_700_000_000_000 + i,
                )
            })
            .expect("audit");
    }
    // No update/delete API exists — compile-time append-only. Runtime check:
    // re-saving topology must not touch audit rows.
    store.save_topology(&panes(1)).expect("save");
    assert_eq!(store.audit_recent(10).expect("recent").len(), 3);
}

#[test]
fn v1_database_migrates_to_the_current_schema() {
    // A v1 DB has only meta+rollups (the T-0006 metrics schema). Opening it
    // with the current store must migrate forward — sessions/panes/audit, then
    // devices/audit.kind — and bump the version to SCHEMA_VERSION. Never wipe
    // user data (rollups survive).
    let dir = std::env::temp_dir().join(format!("arreo-mig-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mig.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta(key, value) VALUES ('schema_version', '1');
             CREATE TABLE rollups(pane TEXT NOT NULL, ts_ms INTEGER NOT NULL,
               rss_bytes INTEGER NOT NULL, cpu REAL NOT NULL, pids INTEGER NOT NULL,
               PRIMARY KEY (pane, ts_ms));
             INSERT INTO rollups VALUES ('p', 1, 2, 3.0, 1);",
        )
        .unwrap();
    }
    let store = SessionStore::open(&path).expect("open migrates");
    // The contract is "opening migrates to current", not "to a hardcoded N":
    // the assertion does not need editing on the next migration.
    assert_eq!(
        store.schema_version().expect("version"),
        arreo_core::store::SCHEMA_VERSION
    );
    // Rollup data survived the migration.
    let conn = rusqlite::Connection::open(&path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM rollups", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "user data survives migration");
    // New tables exist and work.
    store.save_topology(&panes(2)).expect("save post-migration");
    assert_eq!(store.load_topology().expect("load").len(), 2);
    // The v3 additions are present on a database that never had them.
    assert!(store.devices().expect("devices table exists").is_empty());
}

#[test]
fn v2_database_migrates_to_v3_keeping_its_audit_rows() {
    // T-0025 adds `devices` and `audit.kind` in place. A v2 database with real
    // audit rows must gain the column without losing or mangling those rows,
    // and its old rows are prompt events (what they were).
    let dir = std::env::temp_dir().join(format!("arreo-mig32-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("mig32.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
             INSERT INTO meta(key, value) VALUES ('schema_version', '2');
             CREATE TABLE panes(id TEXT PRIMARY KEY, program TEXT NOT NULL,
               args TEXT NOT NULL, cols INTEGER NOT NULL, rows INTEGER NOT NULL);
             CREATE TABLE scrollback(pane TEXT NOT NULL, line_no INTEGER NOT NULL,
               text TEXT NOT NULL, PRIMARY KEY (pane, line_no));
             CREATE TABLE audit(ts_ms INTEGER NOT NULL, device TEXT NOT NULL,
               agent TEXT NOT NULL, prompt TEXT NOT NULL, redacted INTEGER NOT NULL);
             INSERT INTO audit VALUES (1234, 'dev_old', 'agent-1', 'do the thing', 0);",
        )
        .unwrap();
    }
    let store = SessionStore::open(&path).expect("open migrates");
    let rows = store.audit_recent(10).expect("audit reads back");
    assert_eq!(rows.len(), 1, "the pre-existing audit row survived");
    assert_eq!(rows[0].prompt, "do the thing");
    assert_eq!(rows[0].device, "dev_old");
    assert_eq!(
        rows[0].kind,
        arreo_core::store::AuditKind::Prompt,
        "a row written before the column existed is a prompt event"
    );
    // And the new kinds write/read alongside it.
    store
        .record(&arreo_core::store::AuditEvent {
            device: "dev_new".into(),
            prompt: "no certificate".into(),
            ..arreo_core::store::AuditEvent::new(
                arreo_core::store::actions::AUTH_REJECT,
                arreo_core::store::AuditKind::AuthReject,
                arreo_core::store::AuditOutcome::Refused,
                2000,
            )
        })
        .expect("reject row");
    let rows = store.audit_recent(10).expect("audit reads back");
    assert_eq!(rows[0].kind, arreo_core::store::AuditKind::AuthReject);
    assert_eq!(rows[1].kind, arreo_core::store::AuditKind::Prompt);
}

#[test]
fn an_existing_loose_store_is_tightened_by_the_next_open() {
    // T-0078: an install that predates the policy must be fixed by the first
    // open of the fixed build, not only by fresh files. A store nobody chmods
    // carries the ambient umask's mode (0644 at the default) and its -wal/-shm
    // sidecars with it; the next open must re-apply owner-only to all three.
    use std::os::unix::fs::PermissionsExt;
    let dir = std::env::temp_dir().join(format!(
        "arreo-store-modes-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("loose.db");
    let wal = dir.join("loose.db-wal");
    let shm = dir.join("loose.db-shm");

    // One connection writes and then stays open, so the sidecars exist and
    // survive while the "next open" runs — the daemon's own shape, where a
    // background op can hold the store open while another opens it.
    {
        let held = SessionStore::open(&db).expect("open");
        held.save_topology(&panes(1)).expect("save");

        // Widen every file to the mode a pre-fix install would have.
        for file in [&db, &wal, &shm] {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).expect("loosen");
        }

        // The next open (an existing install's very next op) must re-apply the
        // policy to all three files.
        let _next = SessionStore::open(&db).expect("reopen");
        for (file, name) in [(&db, "store"), (&wal, "-wal"), (&shm, "-shm")] {
            let mode = std::fs::metadata(file).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "{name} must be owner-only (0600) after the next open, was {mode:o}"
            );
        }
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn session_ids_are_not_secret_scanned_away() {
    // T-0072's safety criterion, second half. Session ids look nothing like the
    // credential prefixes the redactor hunts (`sk-`, `AKIA`, `ghp_`, `xox`) —
    // pi's are v4 UUIDs, opencode's are `ses_<base62>` — and the redaction
    // scan must leave both alone. Pinned because "add the new prefix to
    // `TOKEN_PREFIXES`" is a one-line change that would silently replace a
    // session id with `[REDACTED:...]` in whatever row carried it, and because
    // the harness-side continuity checks read exactly these strings back.
    let store = SessionStore::open_memory().expect("open");
    let pi = "01a099cd-2d35-74c4-90c6-d0e3b10011b5";
    let opencode = "ses_f65f0f5d3ffeGhl8q7ftjyml5K";
    store
        .record(&AuditEvent {
            device: "cli".to_string(),
            agent: "pane-1".to_string(),
            prompt: format!("session {pi}"),
            detail: Some(format!("harness=opencode session={opencode}")),
            ..AuditEvent::new(
                arreo_core::store::actions::SESSION_CONNECT,
                AuditKind::Unknown,
                arreo_core::store::AuditOutcome::Ok,
                1_700_000_000_000,
            )
        })
        .expect("audit");
    let rows = store.audit_recent(1).expect("recent");
    assert_eq!(rows.len(), 1);
    assert!(
        rows[0].prompt.contains(pi),
        "pi's session id survived the secret scan: {}",
        rows[0].prompt
    );
    assert!(
        rows[0]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains(opencode)),
        "opencode's session id survived the secret scan: {:?}",
        rows[0].detail
    );
    assert!(!rows[0].redacted, "neither id is secret-shaped");
}
