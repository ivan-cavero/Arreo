//! T-0018 failing-first probes: session store, audit redaction, migrations.
//!
//! Written before `src/store.rs` exists — MUST fail to compile until it lands.
//!
//! Gated on the `sqlite` feature (T-0010 lite pass): the store is the one
//! C-backed module, so `cargo check --no-default-features --all-targets`
//! — the foreign-target portability gate — must not try to compile this file.
#![cfg(feature = "sqlite")]

use arreo_core::store::{AuditEvent, SessionStore, StoredPane};

fn panes(n: usize) -> Vec<StoredPane> {
    (0..n)
        .map(|i| StoredPane {
            id: format!("pane-{i}"),
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), format!("echo scroll-{i}")],
            cols: 80,
            rows: 24,
            scrollback: vec![format!("scroll-{i}-line-0"), format!("scroll-{i}-line-1")],
        })
        .collect()
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
        .audit(AuditEvent {
            ts_ms: 1_700_000_000_000,
            device: "phone".to_string(),
            agent: "pane-1".to_string(),
            prompt: "deploy with OPENAI_API_KEY=sk-abc123XYZ4567890abcdef now".to_string(),
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
            .audit(AuditEvent {
                ts_ms: 1_700_000_000_000 + i,
                device: "cli".to_string(),
                agent: "pane-0".to_string(),
                prompt: format!("command-{i}"),
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
        .audit_event(
            arreo_core::store::AuditKind::AuthReject,
            arreo_core::store::AuditEvent {
                ts_ms: 2000,
                device: "dev_new".into(),
                agent: String::new(),
                prompt: "no certificate".into(),
            },
        )
        .expect("reject row");
    let rows = store.audit_recent(10).expect("audit reads back");
    assert_eq!(rows[0].kind, arreo_core::store::AuditKind::AuthReject);
    assert_eq!(rows[1].kind, arreo_core::store::AuditKind::Prompt);
}
