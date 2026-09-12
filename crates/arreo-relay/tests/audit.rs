//! T-0053 acceptance tests: the relay's own audit trail.
//!
//! Two levels, deliberately. The **store-level** tests cover the mechanical
//! properties (ordering, truncation, the export shape, pruning, the size guard)
//! where a real QUIC session would only add noise. The **exchange-level** tests
//! spawn the real relay binary and speak the real protocol, because the facts
//! that matter most — that a session is recorded, that a refusal names its
//! reason, that nothing the relay carried reached its database — are facts about
//! what the running process writes, and an in-process mock would assert nothing
//! about them.

use arreo_core::identity::{DeviceCert, DeviceKey, Role, RootKey, VerifyingKey};
use arreo_core::relay::{RelayClient, RELAY_VERSION};
use arreo_core::store::{AuditOutcome, AuditQuery, ExportFormat};
use arreo_relay::audit::{actions, relay_audit_json, RelayAuditEvent, StoredRelayAudit};
use arreo_relay::inbox::{Inbox, InboxLimits};
use arreo_relay::store::RelayStore;
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const ACCOUNT: &str = "acct-1";

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "arreo-relay-audit-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn all_rows(store: &RelayStore) -> Vec<StoredRelayAudit> {
    store
        .audit_query(&AuditQuery::all(i64::MAX as usize))
        .expect("query")
}

// ---- the schema -----------------------------------------------------------

/// The column set is a contract: adding one is a decision, not a detail. This is
/// the same shape `tests/directory.rs` uses for the machine table, applied to the
/// relay's trail — because "metadata only" is a property of the schema first and
/// of the writers second.
#[test]
fn the_trail_has_nowhere_to_put_content() {
    let store = RelayStore::open_memory().expect("store");
    let columns = store.columns("relay_audit").expect("columns");
    assert_eq!(
        columns,
        vec![
            "ts_ms",
            "action",
            "outcome",
            "device_id",
            "account_id",
            "peer",
            "proto_version",
            "detail",
        ],
        "the relay_audit columns are a contract: adding one is a decision, not a detail"
    );
    for column in &columns {
        for forbidden in [
            "prompt",
            "agent",
            "scrollback",
            "payload",
            "bytes",
            "body",
            "secret",
            "private",
            "token",
            "password",
            "key",
        ] {
            assert!(
                !column.contains(forbidden),
                "column {column:?} suggests the relay is storing {forbidden:?}"
            );
        }
    }
}

// ---- the exchange ---------------------------------------------------------

/// A running relay, its state directory, and everything it logged.
struct Relay {
    child: Child,
    addr: SocketAddr,
    state_dir: PathBuf,
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.state_dir);
    }
}

impl Relay {
    fn start(tag: &str) -> Self {
        let state_dir = scratch(tag);
        let mut child = Command::new(relay_binary())
            .args([
                "serve",
                "--listen",
                "127.0.0.1:0",
                "--state-dir",
                &state_dir.display().to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the relay starts");
        let stderr = child.stderr.take().expect("stderr is piped");
        let (ready_tx, ready_rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(addr) = line
                    .split("router on ")
                    .nth(1)
                    .and_then(|rest| rest.split_whitespace().next())
                    .and_then(|addr| addr.parse::<SocketAddr>().ok())
                {
                    let _ = ready_tx.send(addr);
                }
            }
        });
        let addr = ready_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the relay announces its address");
        Self {
            child,
            addr,
            state_dir,
        }
    }

    fn register_account(&self, account_id: &str, root: &VerifyingKey) {
        let output = Command::new(relay_binary())
            .args([
                "account",
                "add",
                "--state-dir",
                &self.state_dir.display().to_string(),
                "--account",
                account_id,
                "--root-key",
                &hex(root.to_bytes().as_slice()),
            ])
            .output()
            .expect("the account command runs");
        assert!(
            output.status.success(),
            "registering an account failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Read the trail back through the binary's own export verb — the path an
    /// operator uses, rather than a store handle the operator does not have.
    fn export(&self, extra: &[&str]) -> String {
        let state_dir = self.state_dir.display().to_string();
        let mut args = vec!["audit", "export", "--state-dir", state_dir.as_str()];
        args.extend_from_slice(extra);
        let output = Command::new(relay_binary())
            .args(&args)
            .output()
            .expect("the export command runs");
        assert!(
            output.status.success(),
            "export failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).expect("utf8")
    }
}

fn relay_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arreo-relay"))
}

fn device(root: &RootKey, name: &str, serial: u64) -> (DeviceKey, DeviceCert) {
    let key = DeviceKey::generate().expect("entropy");
    let cert = DeviceCert::issue(root, &key.public(), name, Role::Owner, 1_000, serial);
    (key, cert)
}

/// The keys of a serialised JSON object, in the order they appear in the line.
///
/// Scanned rather than parsed: parsing into a `serde_json::Value` sorts the keys,
/// which is exactly the property under test, so a parse would assert nothing. A
/// key is a quoted run followed by `:` — values are quoted too, but they are
/// followed by a comma or a brace.
fn object_keys(line: &str) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut keys = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '"' {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        let mut key = String::new();
        while j < chars.len() && chars[j] != '"' {
            key.push(chars[j]);
            j += 1;
        }
        if chars.get(j + 1) == Some(&':') {
            keys.push(key);
        }
        i = j + 1;
    }
    keys
}

/// A session that authenticates and then goes away, so the trail holds both ends
/// of it.
#[tokio::test]
async fn a_real_session_writes_its_connect_and_disconnect() {
    let relay = Relay::start("session");
    let root = RootKey::generate().expect("entropy");
    relay.register_account(ACCOUNT, &root.public());
    let (key, cert) = device(&root, "alice", 1);

    {
        let client = RelayClient::connect(relay.addr, ACCOUNT, &key, &cert)
            .await
            .expect("alice authenticates");
        assert_eq!(client.device_id(), cert.device());
        // Dropping the client closes the stream; the relay's read loop then ends
        // and writes the disconnect row.
    }
    // **The wait is long because the relay's notice is slow, and that is a fact
    // about the product rather than about this test.** A client that vanishes
    // without closing its QUIC connection is noticed when the connection idles
    // out — measured at ~15 s here — not when its socket goes away. So this waits
    // well past that instead of asserting a latency nobody has promised; the
    // disconnect row's *content* is what this test is about.
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    let text = loop {
        let text = relay.export(&[]);
        if text.contains(actions::SESSION_DISCONNECT) || std::time::Instant::now() > deadline {
            break text;
        }
        std::thread::sleep(Duration::from_millis(250));
    };

    assert!(
        text.contains(actions::SESSION_CONNECT),
        "a session that authenticated must be recorded:\n{text}"
    );
    assert!(
        text.contains(actions::SESSION_DISCONNECT),
        "and so must its end:\n{text}"
    );
    // The connect row carries the four facts the criterion names. The device is
    // the **canonical** id, not the `dev_`-prefixed display form: that is the
    // spelling every relay table keys on (`relay_device`, the router's route map),
    // so the trail joins to them without a conversion.
    assert!(
        text.contains(&format!("\"device\":\"{}\"", cert.device().as_str())),
        "the device, in the form the relay's own tables use:\n{text}"
    );
    assert!(text.contains(ACCOUNT), "the account:\n{text}");
    assert!(
        text.contains("\"proto_version\":1")
            || text.contains(&format!("\"proto_version\":{RELAY_VERSION}")),
        "the protocol version the peer spoke:\n{text}"
    );
    assert!(
        text.contains("\"outcome\":\"ok\""),
        "an admitted session is `ok`:\n{text}"
    );
    // And the peer, truncated at write — the relay is the box most likely to be
    // someone else's, so it never keeps a full address.
    assert!(
        text.contains("\"peer\":\"127.0.0.0/24\""),
        "the peer must be truncated to its /24:\n{text}"
    );
    assert!(
        !text.contains("127.0.0.1:"),
        "no full address may survive in the trail:\n{text}"
    );
}

#[tokio::test]
async fn an_unknown_account_is_refused_and_the_refusal_is_recorded() {
    let relay = Relay::start("unknown");
    let root = RootKey::generate().expect("entropy");
    relay.register_account(ACCOUNT, &root.public());
    let (key, cert) = device(&root, "alice", 1);

    // A different account id, with a perfectly good certificate for it: the
    // refusal is about the account not existing, not about the device.
    let refused = RelayClient::connect(relay.addr, "acct-nope", &key, &cert).await;
    assert!(
        refused.is_err(),
        "an unknown account must not get a session"
    );

    let text = relay.export(&[]);
    assert!(
        text.contains(actions::REFUSE),
        "the refusal must be recorded:\n{text}"
    );
    assert!(
        text.contains("\"outcome\":\"refused\""),
        "with the refused outcome:\n{text}"
    );
    assert!(
        text.contains("unknown account"),
        "and a reason an operator can act on:\n{text}"
    );
    assert!(
        text.contains("acct-nope"),
        "naming the account that was asked for:\n{text}"
    );
}

#[tokio::test]
async fn a_certificate_from_the_wrong_root_is_refused_and_named_as_such() {
    let relay = Relay::start("badcert");
    let root = RootKey::generate().expect("entropy");
    relay.register_account(ACCOUNT, &root.public());

    // A certificate issued by a root the relay does not know: the device proves
    // possession of its key perfectly well, and it is still refused.
    let other_root = RootKey::generate().expect("entropy");
    let (key, cert) = device(&other_root, "mallory", 1);
    let refused = RelayClient::connect(relay.addr, ACCOUNT, &key, &cert).await;
    assert!(refused.is_err(), "a foreign root must not get a session");

    let text = relay.export(&[]);
    assert!(text.contains(actions::REFUSE), "{text}");
    // The reason distinguishes the failure. "auth" alone would not: a bad
    // certificate and a bad proof of possession are the same word to a reader of
    // the status line, and they have different fixes.
    assert!(
        text.contains("certificate") || text.contains("cert"),
        "the reason must name the certificate, not just `auth`:\n{text}"
    );
}

/// The criterion's sharpest check: after a real exchange, nothing the relay
/// carried is anywhere in its database. The machine's log makes this check for
/// its own store; this is the relay's, over the file the relay actually writes.
#[tokio::test]
async fn the_trail_carries_no_marker_from_the_traffic() {
    let relay = Relay::start("marker");
    let root = RootKey::generate().expect("entropy");
    relay.register_account(ACCOUNT, &root.public());
    let (alice_key, alice_cert) = device(&root, "alice", 1);
    let (bob_key, bob_cert) = device(&root, "bob", 2);
    let bob_id = bob_cert.device().clone();

    let mut bob = RelayClient::connect(relay.addr, ACCOUNT, &bob_key, &bob_cert)
        .await
        .expect("bob authenticates");
    let mut alice = RelayClient::connect(relay.addr, ACCOUNT, &alice_key, &alice_cert)
        .await
        .expect("alice authenticates");

    let marker = "ARREO-AUDIT-MARKER-4b7e2d";
    let payload = format!("\x1b[32m$ \x1b[0m{marker}\n> waiting for input\n").into_bytes();
    let seq = alice.send(&bob_id, &payload).await.expect("alice sends");
    let received = bob.recv_envelope().await.expect("bob receives");
    assert_eq!(received.payload, payload, "the payload survives the relay");
    let _ = seq;

    // Give the relay a moment to write its own rows, so the scan is not passing
    // merely because nothing has been recorded yet.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if relay.export(&[]).contains(actions::SESSION_CONNECT) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // 1. The rows themselves.
    let text = relay.export(&[]);
    assert!(
        text.contains(actions::SESSION_CONNECT),
        "the exchange must have been recorded, or this test proves nothing:\n{text}"
    );
    assert!(
        !text.contains(marker),
        "the payload reached the trail:\n{text}"
    );

    // 2. And the database file, which is the thing that would leak in a backup.
    let db = relay.state_dir.join("relay.db");
    let mut scanned = 0usize;
    for path in std::fs::read_dir(&relay.state_dir)
        .expect("state dir")
        .flatten()
    {
        let path = path.path();
        if path.is_file() {
            let bytes = std::fs::read(&path).expect("read state file");
            assert!(
                !bytes
                    .windows(marker.len())
                    .any(|window| window == marker.as_bytes()),
                "the payload reached {} — the relay must not persist what it routes",
                path.display()
            );
            scanned += 1;
        }
    }
    assert!(scanned > 0, "the relay keeps state files to scan");
    assert!(db.exists(), "the relay keeps its database at relay.db");
}

// ---- the store-level properties -------------------------------------------

#[test]
fn a_peer_address_is_truncated_when_written_and_only_the_network_remains() {
    let store = RelayStore::open_memory().expect("store");
    // A private address, the shape a real deployment sees, plus a v6 one.
    store
        .record(
            &RelayAuditEvent::new(actions::SESSION_CONNECT, AuditOutcome::Ok)
                .device("dev_1")
                .peer("203.0.113.77:51234".parse().expect("addr")),
        )
        .expect("record");
    store
        .record(
            &RelayAuditEvent::new(actions::SESSION_CONNECT, AuditOutcome::Ok)
                .device("dev_2")
                .peer("[2001:db8:1234:5678::9]:443".parse().expect("addr")),
        )
        .expect("record");

    let rows = all_rows(&store);
    assert_eq!(rows[0].peer.as_deref(), Some("203.0.113.0/24"));
    assert_eq!(rows[1].peer.as_deref(), Some("2001:db8:1234::/48"));

    // And the truncation is what the export carries: there is no untruncated form
    // to recover, because the row never held one.
    let text = store
        .audit_export(&AuditQuery::all(100), ExportFormat::Jsonl)
        .expect("export");
    assert!(text.contains("203.0.113.0/24"), "{text}");
    assert!(!text.contains("203.0.113.77"), "{text}");
    assert!(!text.contains("51234"), "{text}");
}

#[test]
fn rows_written_in_the_same_millisecond_read_back_in_the_order_they_were_written() {
    // The clock is not a sequence number: a reconnect that displaces a session
    // writes two rows in one millisecond, and a reader must see them in the order
    // they happened. `rowid` is what breaks the tie — this is the property the
    // ordering rule exists for, so it is the property the test asserts.
    let dir = scratch("ordering");
    let path = dir.join("relay.db");
    let store = RelayStore::open(&path).expect("store");
    store
        .record(
            &RelayAuditEvent::new(actions::SESSION_DISCONNECT, AuditOutcome::Ok).device("first"),
        )
        .expect("record");
    store
        .record(&RelayAuditEvent::new(actions::SESSION_CONNECT, AuditOutcome::Ok).device("second"))
        .expect("record");

    // Pin both rows to one instant, which is what a fast reconnect produces. A
    // second connection to the same file is how the test reaches the rows the
    // store's own API only appends to.
    {
        let conn = rusqlite::Connection::open(&path).expect("second connection");
        let ts: i64 = conn
            .query_row("SELECT ts_ms FROM relay_audit LIMIT 1", [], |row| {
                row.get(0)
            })
            .expect("a row exists");
        conn.execute("UPDATE relay_audit SET ts_ms = ?1", [ts])
            .expect("pin");
    }

    let rows = all_rows(&store);
    let devices: Vec<&str> = rows
        .iter()
        .filter_map(|row| row.device_id.as_deref())
        .collect();
    assert_eq!(
        devices,
        vec!["first", "second"],
        "rows in one millisecond must read back in write order"
    );
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_export_is_the_machines_export_with_the_relay_s_own_fields() {
    let store = RelayStore::open_memory().expect("store");
    store
        .record(
            &RelayAuditEvent::new(actions::SESSION_CONNECT, AuditOutcome::Ok)
                .device("dev_1")
                .account(ACCOUNT)
                .peer("198.51.100.9:1".parse().expect("addr"))
                .proto_version(1),
        )
        .expect("record");

    // The relay's rows go through the *same* renderer as the machine's, so the
    // bytes agree on everything that is not a field name: the trailing newline,
    // the pretty-printing, and what an empty window looks like.
    let rows = all_rows(&store);
    let values: Vec<serde_json::Value> = rows.iter().map(relay_audit_json).collect();
    for format in [ExportFormat::Jsonl, ExportFormat::Json] {
        assert_eq!(
            store
                .audit_export(&AuditQuery::all(100), format)
                .expect("export"),
            arreo_core::store::render_export(&values, format).expect("shared renderer"),
            "the relay's export must be the shared renderer's output, byte for byte"
        );
    }

    // An empty window is part of the contract: a log pipeline must not see a line
    // that is not an object, and a JSON parser must get an array.
    let empty = store
        .audit_export(
            &AuditQuery {
                since_ms: Some(u64::MAX - 1),
                ..AuditQuery::all(100)
            },
            ExportFormat::Jsonl,
        )
        .expect("export");
    assert_eq!(empty, "", "JSONL of nothing is zero bytes");
    let empty_json = store
        .audit_export(
            &AuditQuery {
                since_ms: Some(u64::MAX - 1),
                ..AuditQuery::all(100)
            },
            ExportFormat::Json,
        )
        .expect("export");
    assert_eq!(empty_json, "[]\n", "JSON of nothing is an empty array");

    // Key order is alphabetical (serde_json's map), which is what makes two
    // exports of the same window byte-identical rather than merely equivalent.
    let text = store
        .audit_export(&AuditQuery::all(100), ExportFormat::Jsonl)
        .expect("export");
    let line = text.lines().next().expect("one row");
    let keys = object_keys(line);
    assert_eq!(
        keys,
        vec![
            "account",
            "action",
            "detail",
            "device",
            "outcome",
            "peer",
            "proto_version",
            "ts_ms"
        ],
        "the key order is part of the byte contract: {line}"
    );
}

#[test]
fn a_prune_records_itself_with_the_count_it_removed() {
    let store = RelayStore::open_memory().expect("store");
    for _ in 0..5 {
        store
            .record(&RelayAuditEvent::new(actions::SESSION_CONNECT, AuditOutcome::Ok).device("d"))
            .expect("record");
    }
    let removed = store.audit_prune(u64::MAX).expect("prune");
    assert_eq!(removed, 5, "everything was before the cutoff");

    // The prune is the only thing left, and it says what it did. A gap in a log
    // with no explanation is worse than the log being long.
    let rows = all_rows(&store);
    assert_eq!(rows.len(), 1, "the prune records itself: {rows:?}");
    assert_eq!(rows[0].action, actions::PRUNE);
    assert!(
        rows[0]
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains('5')),
        "with the count: {:?}",
        rows[0].detail
    );

    // Pruning to a cutoff that removes nothing still records itself: the fact is
    // "a prune ran", and an operator reading the trail wants to know it did.
    let removed = store.audit_prune(0).expect("prune");
    assert_eq!(removed, 0);
    assert_eq!(all_rows(&store).len(), 2);
}

#[test]
fn the_size_guard_warns_only_once_the_trail_is_past_the_limit() {
    let store = RelayStore::open_memory().expect("store");
    store
        .record(&RelayAuditEvent::new(actions::SESSION_CONNECT, AuditOutcome::Ok).device("d"))
        .expect("record");
    assert_eq!(
        store.audit_size_warning(100 * 1024 * 1024).expect("guard"),
        None,
        "a small trail is not worth telling anyone about"
    );
    // A limit of zero bytes is "warn about anything", which is how a test reaches
    // the warning without writing a hundred megabytes.
    let warning = store
        .audit_size_warning(0)
        .expect("guard")
        .expect("a warning past the limit");
    assert!(
        warning.contains("prune it offline"),
        "the warning must say what to do about it: {warning}"
    );
    let (rows, bytes) = store.audit_size().expect("size");
    assert_eq!(rows, 1);
    assert!(bytes > 0, "the size is measured, not guessed: {bytes}");
}

#[test]
fn an_eviction_and_an_expiry_are_recorded_by_the_mailbox_that_caused_them() {
    // A one-message inbox: the second enqueue must evict the first.
    let store = RelayStore::open_memory().expect("store");
    let limits = InboxLimits::from_options(1, 1, 1).expect("limits");
    let inbox = Inbox::new(store.clone(), limits);

    inbox.enqueue("dev_1", b"first", 1_000).expect("enqueue");
    inbox.enqueue("dev_1", b"second", 1_001).expect("enqueue");

    let rows = all_rows(&store);
    let drop = rows
        .iter()
        .find(|row| row.action == actions::INBOX_DROP)
        .expect("the eviction is recorded");
    assert_eq!(drop.outcome, AuditOutcome::Ok);
    assert_eq!(drop.device_id.as_deref(), Some("dev_1"));
    assert!(
        drop.detail.as_deref().is_some_and(|d| d.contains('1')),
        "with the count: {:?}",
        drop.detail
    );

    // Two days on, the TTL (one day) has passed: the sweep expires the survivor
    // and records that too.
    let expired = inbox
        .sweep(1_000 + 2 * 24 * 60 * 60 * 1_000)
        .expect("sweep");
    assert_eq!(expired, 1, "the survivor is past its TTL");
    let rows = all_rows(&store);
    let expire = rows
        .iter()
        .find(|row| row.action == actions::INBOX_EXPIRE)
        .expect("the expiry is recorded");
    assert_eq!(
        expire.outcome,
        AuditOutcome::Expired,
        "an expiry is its own outcome, not a refusal"
    );
    assert!(
        expire.detail.as_deref().is_some_and(|d| d.contains('1')),
        "with the count: {:?}",
        expire.detail
    );
}

#[test]
fn a_detail_that_is_not_a_sentence_is_capped_rather_than_stored() {
    // A reason is a sentence. Capping it here means a pathological identifier —
    // a hostile account id, say — cannot become a place content accumulates.
    let store = RelayStore::open_memory().expect("store");
    store
        .record(
            &RelayAuditEvent::new(actions::REFUSE, AuditOutcome::Refused)
                .detail("x".repeat(arreo_relay::audit::DETAIL_MAX * 4)),
        )
        .expect("record");
    let rows = all_rows(&store);
    let detail = rows[0].detail.as_deref().expect("detail");
    assert!(
        detail.chars().count() <= arreo_relay::audit::DETAIL_MAX + 1,
        "the detail is capped, with a mark: {} chars",
        detail.chars().count()
    );
    assert!(detail.ends_with('…'), "and the cut is visible: {detail:?}");
}
