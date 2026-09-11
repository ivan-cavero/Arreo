//! T-0033 acceptance tests: the machine audit trail.
//!
//! Gated on the `sqlite` feature (T-0010 lite pass): the audit log lives in the
//! store, so `cargo check --no-default-features --all-targets` — the
//! foreign-target portability gate — must not try to compile this file.
//!
//! The audit log is the thing an operator reads *after* something happened, so
//! these tests are about what it can be asked and what it must never contain:
//! the actions and outcomes it records, the order it replays in, the redaction
//! it applies before anything reaches disk, and the export that must be a view
//! rather than a second source of truth.
#![cfg(feature = "sqlite")]

use arreo_core::store::{
    actions, audit_json, truncate_peer, AuditEvent, AuditKind, AuditOutcome, AuditQuery,
    ExportFormat, SessionStore,
};
use std::net::SocketAddr;

fn fresh() -> SessionStore {
    SessionStore::open_memory().expect("open")
}

/// Write one row with the given action/outcome and return nothing (the tests
/// read back through the public readers).
fn write(store: &SessionStore, action: &str, outcome: AuditOutcome, ts_ms: u64) {
    store
        .record(&AuditEvent {
            device: "dev_a".to_string(),
            agent: "pane-1".to_string(),
            prompt: format!("{action} happened"),
            ..AuditEvent::new(action, AuditKind::Unknown, outcome, ts_ms)
        })
        .expect("record");
}

/// Every action the machine writes round-trips with its outcome and is findable
/// by name — the property an operator's query depends on.
#[test]
fn actions_and_outcomes_round_trip_and_are_queryable() {
    let store = fresh();
    let cases = [
        (actions::SESSION_CONNECT, AuditOutcome::Ok),
        (actions::SESSION_DISCONNECT, AuditOutcome::Ok),
        (actions::ATTACH, AuditOutcome::Ok),
        (actions::SEND, AuditOutcome::Ok),
        (actions::SPAWN, AuditOutcome::Ok),
        (actions::SPLIT, AuditOutcome::Ok),
        (actions::DEVICE_REVOKE, AuditOutcome::Ok),
        (actions::AUTH_REJECT, AuditOutcome::Refused),
        (actions::PAIRING_FAILED, AuditOutcome::Refused),
        (actions::PRUNE, AuditOutcome::Expired),
    ];
    for (index, (action, outcome)) in cases.iter().enumerate() {
        write(&store, action, *outcome, 1_000 + index as u64);
    }

    let all = store.audit_query(&AuditQuery::all(100)).expect("query");
    assert_eq!(all.len(), cases.len(), "every row is readable");
    for (action, outcome) in cases {
        let rows = store.audit_by_action(action, 10).expect("by action");
        assert_eq!(rows.len(), 1, "{action} is findable by name");
        assert_eq!(rows[0].outcome, outcome, "{action} keeps its outcome");
        assert_eq!(rows[0].action, action);
    }

    // An outcome is what tells intent from result: a refused row must not read
    // as a completed one.
    let refused = store
        .audit_by_action(actions::AUTH_REJECT, 10)
        .expect("query");
    assert_eq!(refused[0].outcome.as_str(), "refused");
}

/// History replays in write order even if the clock steps backwards — a
/// suspended VM or an NTP correction must not reorder what happened.
#[test]
fn ordering_survives_a_backwards_clock() {
    let store = fresh();
    // Three rows: the third claims to be *older* than the first two.
    write(&store, actions::ATTACH, AuditOutcome::Ok, 5_000);
    write(&store, actions::SEND, AuditOutcome::Ok, 5_001);
    write(&store, actions::SPAWN, AuditOutcome::Ok, 1_000);

    let rows = store.audit_query(&AuditQuery::all(10)).expect("query");
    let actions_seen: Vec<&str> = rows.iter().map(|row| row.action.as_str()).collect();
    // Sorted by (ts_ms, rowid): the backwards row sorts first by its timestamp,
    // and rows sharing a timestamp keep their write order.
    assert_eq!(
        actions_seen,
        vec![actions::SPAWN, actions::ATTACH, actions::SEND],
        "ordering is (ts_ms, rowid), never arrival order"
    );

    // And two rows with the *same* timestamp keep the order they were written.
    let ties = fresh();
    write(&ties, actions::ATTACH, AuditOutcome::Ok, 7_000);
    write(&ties, actions::SEND, AuditOutcome::Ok, 7_000);
    let rows = ties.audit_query(&AuditQuery::all(10)).expect("query");
    assert_eq!(
        rows.iter()
            .map(|row| row.action.as_str())
            .collect::<Vec<_>>(),
        vec![actions::ATTACH, actions::SEND],
        "a tie is broken by rowid, which is monotonic"
    );
}

/// Redaction happens on the way *in*: a secret never reaches the file, and no
/// reader or export flag can un-redact what was never stored.
#[test]
fn secrets_are_redacted_before_they_reach_the_file() {
    let store = fresh();
    store
        .record(&AuditEvent {
            device: "dev_a".to_string(),
            agent: "pane-1".to_string(),
            prompt: "export OPENAI_API_KEY=sk-abc123XYZ4567890abcdef and run".to_string(),
            ..AuditEvent::new(actions::SEND, AuditKind::Prompt, AuditOutcome::Ok, 1_000)
        })
        .expect("record");

    let rows = store.audit_recent(10).expect("read");
    assert!(rows[0].redacted, "the row is flagged as redacted");
    assert!(
        !rows[0].prompt.contains("sk-abc123XYZ4567890abcdef"),
        "the secret must not be in the stored row: {}",
        rows[0].prompt
    );

    // The same holds through the export — there is no path that reads the
    // original, because the original was never written.
    let exported = store
        .audit_export(&AuditQuery::all(10), ExportFormat::Jsonl)
        .expect("export");
    assert!(
        !exported.contains("sk-abc123XYZ4567890abcdef"),
        "{exported}"
    );
}

/// A secret-shaped token is masked **wherever it sits in the line**, not only
/// when it begins a whitespace-delimited word.
///
/// The narrow rule let a live token reach disk: `GITHUB_TOKEN=ghp_...` is one
/// word that begins with `GITHUB_TOKEN=`, so the scan flagged the line while the
/// masker changed nothing, and the row was stored with the secret intact beside
/// `redacted = 1`. A flag that says "redacted" over unmasked bytes is the one
/// outcome this contract exists to prevent.
#[test]
fn a_token_is_masked_wherever_it_appears() {
    let store = fresh();
    let secrets = [
        // The shape that leaked: the prefix is not at the start of the word.
        "export GITHUB_TOKEN=ghp_AAAABBBBCCCCDDDDEEEEFFFF",
        // Quoted, so the run ends at the quote rather than the line.
        "curl -H 'Authorization: token ghp_AAAABBBBCCCCDDDDEEEEFFFF' https://x",
        // Two on one line: both go.
        "sk-abc123XYZ4567890 and AKIAIOSFODNN7EXAMPLE",
    ];
    for (index, prompt) in secrets.iter().enumerate() {
        store
            .record(&AuditEvent {
                device: "dev_a".to_string(),
                agent: "pane-1".to_string(),
                prompt: (*prompt).to_string(),
                ..AuditEvent::new(
                    actions::SEND,
                    AuditKind::Prompt,
                    AuditOutcome::Ok,
                    1_000 + index as u64,
                )
            })
            .expect("record");
    }
    let rows = store.audit_recent(10).expect("read");
    let all: String = rows.iter().map(|row| row.prompt.clone()).collect();
    for token in [
        "ghp_AAAABBBBCCCCDDDDEEEEFFFF",
        "sk-abc123XYZ4567890",
        "AKIAIOSFODNN7EXAMPLE",
    ] {
        assert!(!all.contains(token), "{token} must not be stored: {all}");
    }
    assert!(
        rows.iter().all(|row| row.redacted),
        "every row is flagged: {rows:?}"
    );
    // The field name survives, so a redacted row is still debuggable.
    assert!(all.contains("GITHUB_TOKEN="), "the key part is kept: {all}");

    // A prefix inside an ordinary word is *not* a token: `ask-me` contains
    // `sk-`, and masking it would have mangled a prompt nobody would call a
    // secret. The scan has to agree with the masker, or a flagged line goes to
    // disk unmasked.
    store
        .record(&AuditEvent {
            prompt: "ask-me about the sk-1 prefix".to_string(),
            ..AuditEvent::new(actions::SEND, AuditKind::Prompt, AuditOutcome::Ok, 2_000)
        })
        .expect("record");
    let plain = store.audit_recent(1).expect("read");
    assert_eq!(
        plain[0].prompt, "ask-me about the sk-1 prefix",
        "a short run after a prefix is not a token"
    );
    assert!(!plain[0].redacted, "and it is not flagged either");
}

/// The peer is truncated **at write**, so even a database dump is not a location
/// history — and the truncation survives the export.
#[test]
fn peer_addresses_are_truncated_at_write() {
    let store = fresh();
    let cases = [
        ("203.0.113.77:41000", "203.0.113.0/24"),
        ("[2001:db8:1234:5678::1]:41000", "2001:db8:1234::/48"),
    ];
    for (index, (raw, _)) in cases.iter().enumerate() {
        let addr: SocketAddr = raw.parse().expect("address");
        store
            .record(&AuditEvent {
                device: "dev_a".to_string(),
                peer: Some(truncate_peer(&addr)),
                ..AuditEvent::new(
                    actions::SESSION_CONNECT,
                    AuditKind::Unknown,
                    AuditOutcome::Ok,
                    1_000 + index as u64,
                )
            })
            .expect("record");
    }
    let rows = store.audit_query(&AuditQuery::all(10)).expect("query");
    let peers: Vec<String> = rows.iter().filter_map(|row| row.peer.clone()).collect();
    assert_eq!(peers.len(), 2);
    for (raw, expected) in cases {
        assert!(
            peers.contains(&expected.to_string()),
            "{expected} is what a {raw} peer is stored as: {peers:?}"
        );
    }
    for peer in &peers {
        assert!(!peer.contains(":41000"), "the port is not stored: {peer}");
    }

    // The export carries the truncated form, not the original.
    let exported = store
        .audit_export(&AuditQuery::all(10), ExportFormat::Json)
        .expect("export");
    assert!(exported.contains("203.0.113.0/24"), "{exported}");
    assert!(!exported.contains("203.0.113.77"), "{exported}");
}

/// The truncation rule itself, including the addresses a naive implementation
/// gets wrong.
#[test]
fn the_truncation_rule_is_the_stated_one() {
    let v4: SocketAddr = "10.20.30.40:1234".parse().expect("v4");
    assert_eq!(truncate_peer(&v4), "10.20.30.0/24");
    // Loopback and a /32-shaped address still truncate: the rule has no
    // exceptions, because an exception is a leak with a justification.
    let loopback: SocketAddr = "127.0.0.1:1".parse().expect("v4");
    assert_eq!(truncate_peer(&loopback), "127.0.0.0/24");
    let v6: SocketAddr = "[2001:db8:abcd:1234::9]:80".parse().expect("v6");
    assert_eq!(truncate_peer(&v6), "2001:db8:abcd::/48");
    let v6_short: SocketAddr = "[::1]:80".parse().expect("v6");
    assert_eq!(truncate_peer(&v6_short), "0:0:0::/48");
}

/// The export is a view: identical filters give identical bytes, and the two
/// formats describe the same rows.
#[test]
fn the_export_is_deterministic_and_filtered() {
    let store = fresh();
    for index in 0..5u64 {
        write(&store, actions::SEND, AuditOutcome::Ok, 1_000 + index);
    }
    write(&store, actions::ATTACH, AuditOutcome::Ok, 2_000);

    let query = AuditQuery {
        since_ms: Some(1_001),
        until_ms: Some(1_003),
        action: Some(actions::SEND.to_string()),
        limit: 100,
    };
    let first = store
        .audit_export(&query, ExportFormat::Jsonl)
        .expect("export");
    let second = store
        .audit_export(&query, ExportFormat::Jsonl)
        .expect("export");
    assert_eq!(first, second, "the same filters give the same bytes");
    assert_eq!(
        first.lines().count(),
        3,
        "the window is inclusive on both ends: {first}"
    );
    assert!(
        first.lines().all(|line| line.contains(actions::SEND)),
        "the action filter is applied: {first}"
    );

    // JSON describes the same rows as JSONL, in the same order.
    let as_json = store
        .audit_export(&query, ExportFormat::Json)
        .expect("export");
    let parsed: serde_json::Value = serde_json::from_str(&as_json).expect("valid json");
    let array = parsed.as_array().expect("an array");
    assert_eq!(array.len(), 3);
    let lines: Vec<serde_json::Value> = first
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid jsonl"))
        .collect();
    assert_eq!(lines, *array, "both formats describe the same rows");
    // And the shape is the documented one.
    assert_eq!(lines[0]["action"], serde_json::json!(actions::SEND));
    assert_eq!(lines[0]["outcome"], serde_json::json!("ok"));
    assert!(lines[0]["peer"].is_null(), "no peer on a local action");
}

/// A row read back through the readers and the same row through `audit_json`
/// agree: the JSON is a rendering of the row, not a second opinion.
#[test]
fn the_json_rendering_matches_the_row() {
    let store = fresh();
    store
        .record(&AuditEvent {
            device: "dev_a".to_string(),
            agent: "pane-7".to_string(),
            prompt: "hello".to_string(),
            peer: Some("10.0.0.0/24".to_string()),
            detail: Some("proto=1".to_string()),
            ..AuditEvent::new(
                actions::SESSION_CONNECT,
                AuditKind::Unknown,
                AuditOutcome::Ok,
                4_242,
            )
        })
        .expect("record");
    let row = store.audit_recent(1).expect("read").remove(0);
    let value = audit_json(&row);
    assert_eq!(value["ts_ms"], serde_json::json!(4_242));
    assert_eq!(value["action"], serde_json::json!(actions::SESSION_CONNECT));
    assert_eq!(value["device"], serde_json::json!("dev_a"));
    assert_eq!(value["agent"], serde_json::json!("pane-7"));
    assert_eq!(value["peer"], serde_json::json!("10.0.0.0/24"));
    assert_eq!(value["detail"], serde_json::json!("proto=1"));
    assert_eq!(value["outcome"], serde_json::json!("ok"));
}

/// Pruning is explicit, bounded, and recorded — and the record survives the
/// prune that wrote it.
#[test]
fn a_prune_removes_only_what_it_was_asked_to_and_records_itself() {
    let store = fresh();
    for index in 0..3u64 {
        write(&store, actions::SEND, AuditOutcome::Ok, 1_000 + index);
    }
    write(&store, actions::ATTACH, AuditOutcome::Ok, 9_000);

    let removed = store.audit_prune(5_000, 10_000).expect("prune");
    assert_eq!(removed, 3, "exactly the rows before the bound");

    let rows = store.audit_query(&AuditQuery::all(100)).expect("query");
    let actions_seen: Vec<&str> = rows.iter().map(|row| row.action.as_str()).collect();
    assert!(actions_seen.contains(&actions::ATTACH), "newer rows stay");
    assert!(
        actions_seen.contains(&actions::PRUNE),
        "the prune is recorded"
    );

    // The prune row is *not* itself prunable by the prune that wrote it: a log
    // whose own deletion is the one gap in it is not a log. (`u64::MAX` is the
    // natural "everything" bound, and it is exactly the value a naive `as i64`
    // turns into `-1` — which would match nothing and prune silently.)
    let again = store
        .audit_prune(u64::MAX, 11_000)
        .expect("prune everything");
    assert_eq!(again, 1, "only the surviving action row goes");
    let rows = store.audit_query(&AuditQuery::all(100)).expect("query");
    assert_eq!(
        rows.len(),
        2,
        "both prune rows remain: the first prune's record and the second's"
    );
    assert!(
        rows.iter().all(|row| row.action == actions::PRUNE),
        "nothing else survives an everything-prune: {rows:?}"
    );

    // The count is in the row, so a review can see how much went.
    assert!(
        rows.iter().any(|row| row.prompt.contains("removed 1 row")),
        "the second prune row carries its count: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row.prompt.contains("removed 3 row")),
        "and so does the first: {rows:?}"
    );
}

/// The size guard reports what the table holds, which is what the warning and
/// the operator's decision both rest on.
#[test]
fn the_size_report_counts_rows_and_bytes() {
    let store = fresh();
    let (rows, bytes) = store.audit_size().expect("size");
    assert_eq!(rows, 0);
    assert_eq!(bytes, 0, "an empty log is zero bytes, not a fixed overhead");

    write(&store, actions::SEND, AuditOutcome::Ok, 1_000);
    let (rows, bytes) = store.audit_size().expect("size");
    assert_eq!(rows, 1);
    assert!(bytes > 0, "a row has a size: {bytes}");

    // It grows monotonically with content, which is what a threshold needs.
    write(&store, actions::SEND, AuditOutcome::Ok, 1_001);
    let (rows_later, bytes_later) = store.audit_size().expect("size");
    assert_eq!(rows_later, 2);
    assert!(bytes_later > bytes);
}

/// The two conversions between Rust and SQLite are clamped, because the values
/// that mean "everything" are exactly the ones a naive cast turns negative.
#[test]
fn the_extremes_of_the_range_mean_what_they_say() {
    let store = fresh();
    for index in 0..3u64 {
        write(&store, actions::SEND, AuditOutcome::Ok, 1_000 + index);
    }
    // A query that asks for every row gets every row.
    let all = store
        .audit_query(&AuditQuery::all(usize::MAX))
        .expect("query");
    assert_eq!(all.len(), 3, "usize::MAX means no limit, not none");

    // And a prune bounded at the top of the range removes everything prunable.
    let removed = store.audit_prune(u64::MAX, 9_000).expect("prune");
    assert_eq!(removed, 3, "u64::MAX means 'before everything'");

    // A window from 0 to the top covers everything too.
    let window = store
        .audit_query(&AuditQuery {
            since_ms: Some(0),
            until_ms: Some(u64::MAX),
            action: None,
            limit: usize::MAX,
        })
        .expect("query");
    assert_eq!(window.len(), 1, "only the prune row is left");
}

/// An issued certificate is recorded as a success, not a refusal — the outcome
/// field is what tells an operator whether to go looking for a problem.
#[test]
fn issuing_and_rotating_are_recorded_as_successes() {
    use arreo_core::identity::authority::{DeviceAuthority, Layout};
    use arreo_core::identity::{DeviceKey, Role};

    let dir = std::env::temp_dir().join(format!("arreo-audit-outcome-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let layout = Layout {
        root_key: dir.join("identity").join("root.key"),
        cert_dir: dir.join("identity").join("devices"),
        store: dir.join("arreo.sock.db"),
    };
    let mut authority = DeviceAuthority::load(layout).expect("authority");
    let device = DeviceKey::generate().expect("entropy");
    let cert = authority
        .issue("phone", Role::Owner, &device.public())
        .expect("issue");

    let store = SessionStore::open(&dir.join("arreo.sock.db")).expect("store");
    let issued = store
        .audit_by_action(actions::DEVICE_ISSUE, 10)
        .expect("query");
    assert_eq!(issued.len(), 1);
    assert_eq!(
        issued[0].outcome,
        AuditOutcome::Ok,
        "issuing a certificate succeeded; a `refused` row would send an operator \
         looking for a failure that never happened"
    );

    // Revoking is a success too (the operator asked and it happened), while a
    // refusal is a refusal — the two must not be confused.
    authority
        .revoke(cert.device(), "local-cli", 5_000)
        .expect("revoke");
    let revoked = store
        .audit_by_action(actions::DEVICE_REVOKE, 10)
        .expect("query");
    assert_eq!(revoked[0].outcome, AuditOutcome::Ok);
    // `device` names the device the row is *about* on every action — the target
    // here, the same spelling device.issue used — so one query answers "what
    // happened to this device". Who did it is in `detail`.
    assert_eq!(
        revoked[0].device,
        cert.device().display_id(),
        "the revoke row's subject is the revoked device"
    );
    assert!(
        revoked[0]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("local-cli")),
        "the row names the actor: {:?}",
        revoked[0].detail
    );

    let stranger = DeviceKey::generate().expect("entropy");
    assert!(authority.authorize(&stranger.public()).is_err());
    let refused = store
        .audit_by_action(actions::AUTH_REJECT, 10)
        .expect("query");
    assert!(!refused.is_empty(), "the refusal is on record");
    assert_eq!(refused[0].outcome, AuditOutcome::Refused);

    let _ = std::fs::remove_dir_all(&dir);
}

/// The log is append-only through its API: no update, no delete-one, and the
/// only removal is the explicit, bounded, recorded prune.
#[test]
fn the_log_is_append_only_through_its_api() {
    let store = fresh();
    write(&store, actions::SEND, AuditOutcome::Ok, 1_000);
    write(&store, actions::SEND, AuditOutcome::Ok, 1_001);
    let before = store.audit_recent(10).expect("read").len();
    assert_eq!(before, 2);

    // Re-writing the "same" event appends rather than replacing: there is no
    // upsert on this table, which is what makes the history a history.
    write(&store, actions::SEND, AuditOutcome::Ok, 1_000);
    assert_eq!(
        store.audit_recent(10).expect("read").len(),
        3,
        "a repeated event is a new row, never a replacement"
    );
}
