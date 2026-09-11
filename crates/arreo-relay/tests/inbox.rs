//! T-0030 acceptance tests: the durable per-device inbox.
//!
//! Split deliberately in two. The bounds, expiry and cursor arithmetic are
//! properties of the store, so they are driven directly against a real SQLite
//! file (fast, and precise about which rule broke). Durability across a crash is
//! a property of the *process*, so it runs the real `arreo-relay` binary, sends
//! while the destination is offline, `kill -9`s the relay, restarts it, and
//! drains — the same shape T-0024/T-0029 established: an in-process test cannot
//! prove anything about a restart.

use arreo_core::identity::{DeviceCert, DeviceKey, Role, RootKey, VerifyingKey};
use arreo_core::relay::{Incoming, Outcome, RelayClient};
use arreo_relay::inbox::{
    Inbox, InboxError, InboxLimits, DEFAULT_MAX_MB, DEFAULT_MAX_MESSAGES, DEFAULT_TTL_DAYS,
};
use arreo_relay::store::RelayStore;
use arreo_relay::Directory;
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A store on a real file, so the WAL and the transaction behavior are real.
fn scratch(tag: &str) -> (PathBuf, RelayStore) {
    let dir = std::env::temp_dir().join(format!(
        "arreo-inbox-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let store = RelayStore::open(&dir.join("relay.db")).expect("store");
    (dir, store)
}

fn limits(ttl_ms: i64, max_messages: u64, max_bytes: u64) -> InboxLimits {
    InboxLimits {
        ttl_ms,
        max_messages,
        max_bytes,
    }
}

/// The defaults are the numbers the criteria name, and they are load-bearing.
#[test]
fn the_default_bounds_are_the_stated_numbers() {
    let limits = InboxLimits::default();
    assert_eq!(limits.ttl_ms, 30 * 24 * 60 * 60 * 1000, "30 days");
    assert_eq!(limits.max_messages, 10_000);
    assert_eq!(limits.max_bytes, 64 * 1024 * 1024);
    assert_eq!(DEFAULT_TTL_DAYS, 30);
    assert_eq!(DEFAULT_MAX_MESSAGES, 10_000);
    assert_eq!(DEFAULT_MAX_MB, 64);

    // The operator's flags are validated rather than trusted.
    assert!(
        InboxLimits::from_options(0, 10, 1).is_err(),
        "TTL 0 is refused"
    );
    assert!(
        InboxLimits::from_options(366, 10, 1).is_err(),
        "TTL > 365 is refused"
    );
    assert!(
        InboxLimits::from_options(30, 0, 1).is_err(),
        "a zero message bound is refused"
    );
    assert!(
        InboxLimits::from_options(30, 10, 0).is_err(),
        "a zero byte bound is refused"
    );
    assert!(
        InboxLimits::from_options(1, 1, 1).is_ok(),
        "the smallest sane set is accepted"
    );
}

/// The rows are opaque: a column that could hold a key, a pairing code or agent
/// state is a bug, and the schema is where it would appear.
#[test]
fn the_inbox_holds_no_meaningful_columns() {
    let (dir, store) = scratch("schema");
    let columns = store.columns("inbox").expect("columns");
    assert_eq!(
        columns,
        vec![
            "device_id",
            "seq",
            "received_at_ms",
            "expires_at_ms",
            "bytes"
        ],
        "the inbox row is bookkeeping plus one opaque blob, and nothing else"
    );
    for column in &columns {
        for forbidden in [
            "key", "secret", "token", "code", "grant", "agent", "prompt", "pane", "sender", "src",
        ] {
            assert!(
                !column.contains(forbidden),
                "column {column:?} suggests the relay is storing {forbidden:?}"
            );
        }
    }
    let stats = store.columns("inbox_stats").expect("columns");
    assert!(
        stats.contains(&"dropped_total".to_string())
            && stats.contains(&"expired_total".to_string()),
        "drops must be countable: {stats:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The bound is enforced *before* the write: a full inbox never exceeds it, and
/// eviction is oldest-first.
#[test]
fn a_full_inbox_evicts_oldest_first_and_counts_it() {
    let (dir, store) = scratch("evict");
    // Three messages of 10 bytes, and room for exactly three.
    let inbox = Inbox::new(store, limits(60_000, 3, 1024));
    for index in 0..3u8 {
        let enqueued = inbox
            .enqueue("dev_a", &[index; 10], 1_000)
            .expect("enqueue");
        assert_eq!(enqueued.seq, index as u64 + 1);
        assert!(
            enqueued.evicted.is_empty(),
            "no eviction while there is room"
        );
    }

    // The fourth evicts the first.
    let fourth = inbox.enqueue("dev_a", &[9u8; 10], 1_000).expect("enqueue");
    assert_eq!(fourth.seq, 4);
    assert_eq!(fourth.evicted, vec![1], "the oldest goes first");
    assert_eq!(fourth.queued, 3, "the bound is never exceeded");
    assert_eq!(fourth.dropped_total, 1);

    let stats = inbox.stats("dev_a").expect("stats");
    assert_eq!(stats.queued, 3);
    assert_eq!(stats.dropped_total, 1, "the eviction is counted, not lost");

    // And the drain reports the drop once.
    let drained = inbox.drain("dev_a", 1, 100, 2_000).expect("drain");
    assert_eq!(
        drained
            .messages
            .iter()
            .map(|(seq, _)| *seq)
            .collect::<Vec<_>>(),
        vec![2, 3, 4],
        "what survives is the newest, in order"
    );
    assert_eq!(drained.dropped, 1, "the drop is reported on the next drain");
    let again = inbox.drain("dev_a", 1, 100, 2_000).expect("drain");
    assert_eq!(again.dropped, 0, "a reported drop is not reported twice");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The byte bound bites independently of the message count.
#[test]
fn the_byte_bound_evicts_too() {
    let (dir, store) = scratch("bytes");
    // Room for two 100-byte messages, whatever the message count allows.
    let inbox = Inbox::new(store, limits(60_000, 1_000, 200));
    inbox.enqueue("dev_a", &[1u8; 100], 1_000).expect("first");
    let second = inbox.enqueue("dev_a", &[2u8; 100], 1_000).expect("second");
    assert_eq!(second.queued, 2, "exactly the budget");

    let third = inbox.enqueue("dev_a", &[3u8; 100], 1_000).expect("third");
    assert_eq!(third.evicted, vec![1], "the byte bound evicted the oldest");
    assert!(inbox.stats("dev_a").expect("stats").bytes <= 200);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A message larger than the whole budget is refused with a typed error rather
/// than evicting everything else to make room for it.
#[test]
fn a_message_larger_than_the_budget_is_refused() {
    let (dir, store) = scratch("too-large");
    let inbox = Inbox::new(store, limits(60_000, 100, 128));
    inbox.enqueue("dev_a", &[1u8; 64], 1_000).expect("fits");
    let refused = inbox.enqueue("dev_a", &[2u8; 4096], 1_000);
    assert!(
        matches!(refused, Err(InboxError::TooLarge { .. })),
        "an oversized message must be refused: {refused:?}"
    );
    // And the queue it did not disturb is still intact.
    let stats = inbox.stats("dev_a").expect("stats");
    assert_eq!(stats.queued, 1);
    assert_eq!(stats.dropped_total, 0, "nothing was evicted for it");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Expiry is wall-clock from `received_at`, counted, and lazy as well as swept.
#[test]
fn expiry_is_counted_and_lazy() {
    let (dir, store) = scratch("expiry");
    let inbox = Inbox::new(store, limits(50, 100, 4096));
    inbox.enqueue("dev_a", b"first", 1_000).expect("first");
    inbox.enqueue("dev_a", b"second", 1_000).expect("second");

    // Before the TTL, nothing expires.
    assert_eq!(inbox.sweep(1_040).expect("sweep"), 0);
    // After it, both do — counted per device.
    assert_eq!(inbox.sweep(1_100).expect("sweep"), 2);
    let stats = inbox.stats("dev_a").expect("stats");
    assert_eq!(stats.queued, 0);
    assert_eq!(stats.expired_total, 2, "expiry is a counted drop");
    assert_eq!(stats.dropped_total, 2);

    // Lazy: a drain sweeps what the hourly pass has not reached yet.
    inbox.enqueue("dev_a", b"third", 2_000).expect("third");
    let drained = inbox.drain("dev_a", 1, 100, 2_100).expect("drain");
    assert!(
        drained.messages.is_empty(),
        "the expired message is not delivered"
    );
    assert_eq!(drained.expired, 1, "the lazy sweep reports it");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Exactly-once *at the consumer*: unacked rows come back, and the consumer's
/// `(device, seq)` dedupe is what collapses the redelivery to one delivery.
#[test]
fn an_unacked_message_is_redelivered_and_the_cursor_deduplicates() {
    let (dir, store) = scratch("exactly-once");
    let inbox = Inbox::new(store, InboxLimits::default());
    for index in 0..3u8 {
        inbox.enqueue("dev_a", &[index], 1_000).expect("enqueue");
    }

    // First drain: three messages, no ack.
    let first = inbox.drain("dev_a", 1, 100, 2_000).expect("drain");
    assert_eq!(first.messages.len(), 3);
    assert_eq!(first.next_seq, 4);

    // The consumer "dies" before acking, so a second drain sees them again —
    // that is the at-least-once wire.
    let second = inbox.drain("dev_a", 1, 100, 2_000).expect("drain");
    assert_eq!(second.messages.len(), 3, "unacked rows are redelivered");

    // A consumer that dedupes by (device, seq) delivers each exactly once.
    let mut seen = std::collections::BTreeSet::new();
    let mut delivered = Vec::new();
    for batch in [&first, &second] {
        for (seq, payload) in &batch.messages {
            if seen.insert((*seq, payload.clone())) {
                delivered.push(*seq);
            }
        }
    }
    assert_eq!(delivered, vec![1, 2, 3], "one delivery per message");

    // Acking advances the cursor and removes the rows, so the next drain is
    // empty — and a duplicate ack is harmless.
    assert_eq!(inbox.ack("dev_a", 3).expect("ack"), 3);
    assert_eq!(inbox.ack("dev_a", 3).expect("ack again"), 0);
    let after = inbox.drain("dev_a", 4, 100, 2_000).expect("drain");
    assert!(after.messages.is_empty(), "acked rows are gone");
    let _ = std::fs::remove_dir_all(&dir);
}

// ---- the process-level half: durability across a real crash ------------------

/// A relay process, restarted in place so the state directory persists.
struct Relay {
    child: Child,
    addr: SocketAddr,
    state_dir: PathBuf,
    extra: Vec<String>,
    /// The clock offset this relay was started with, in milliseconds. Part of the
    /// harness because "time passed" is a fact about the relay's view (T-0055),
    /// and a restart is how a test changes it.
    clock_offset_ms: i64,
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.state_dir);
    }
}

impl Relay {
    fn start(tag: &str, extra: &[&str]) -> Self {
        let state_dir = std::env::temp_dir().join(format!(
            "arreo-inbox-proc-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&state_dir);
        std::fs::create_dir_all(&state_dir).expect("scratch");
        let mut relay = Self {
            child: Self::spawn(&state_dir, extra, 0),
            addr: "0.0.0.0:0".parse().expect("placeholder"),
            state_dir,
            extra: extra.iter().map(|s| s.to_string()).collect(),
            clock_offset_ms: 0,
        };
        relay.addr = Self::await_addr(&mut relay.child);
        relay
    }

    fn spawn(state_dir: &std::path::Path, extra: &[&str], clock_offset_ms: i64) -> Child {
        let mut args = vec![
            "serve".to_string(),
            "--listen".to_string(),
            "127.0.0.1:0".to_string(),
            "--state-dir".to_string(),
            state_dir.display().to_string(),
        ];
        args.extend(extra.iter().map(|s| s.to_string()));
        Command::new(env!("CARGO_BIN_EXE_arreo-relay"))
            .args(&args)
            .env(arreo_relay::CLOCK_OFFSET_ENV, clock_offset_ms.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the relay starts")
    }

    fn await_addr(child: &mut Child) -> SocketAddr {
        let stderr = child.stderr.take().expect("stderr");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // Keep reading after the address is found, and keep the handle alive:
            // dropping the pipe would make the relay die on its next log line
            // (EPIPE), which looks exactly like an authentication failure.
            let mut announced = false;
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if !announced {
                    if let Some(rest) = line.split("router on ").nth(1) {
                        if let Some(addr) = rest.split_whitespace().next() {
                            if let Ok(addr) = addr.parse::<SocketAddr>() {
                                let _ = tx.send(addr);
                                announced = true;
                            }
                        }
                    }
                }
            }
        });
        rx.recv_timeout(Duration::from_secs(20))
            .expect("the relay announces its address")
    }

    /// `kill -9` and restart against the same state directory.
    fn crash_and_restart(&mut self) {
        self.restart_with_clock(self.clock_offset_ms);
    }

    /// Restart with the relay's clock moved by `offset_ms` from the wall clock.
    ///
    /// A restart is the honest way to move a process's clock: the offset is read
    /// once, so every clock read in a run shifts together, and a *running* relay
    /// whose clock could jump would be one whose retention cannot be reasoned
    /// about (see `arreo_relay::directory::now_ms`). The store is untouched, so
    /// what changes is only how old the relay believes its rows are — which is
    /// exactly what a long absence is, from the inbox's point of view.
    fn restart_with_clock(&mut self, offset_ms: i64) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let extra: Vec<&str> = self.extra.iter().map(String::as_str).collect();
        self.child = Self::spawn(&self.state_dir, &extra, offset_ms);
        self.clock_offset_ms = offset_ms;
        self.addr = Self::await_addr(&mut self.child);
    }

    fn register_account(&self, account_id: &str, root: &VerifyingKey) {
        let output = Command::new(env!("CARGO_BIN_EXE_arreo-relay"))
            .args([
                "account",
                "add",
                "--state-dir",
                &self.state_dir.display().to_string(),
                "--account",
                account_id,
                "--root-key",
                &root
                    .to_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
            ])
            .output()
            .expect("the account command runs");
        assert!(
            output.status.success(),
            "registering an account failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn device(root: &RootKey, name: &str, serial: u64) -> (DeviceKey, DeviceCert) {
    let key = DeviceKey::generate().expect("entropy");
    let cert = DeviceCert::issue(root, &key.public(), name, Role::Owner, 1_000, serial);
    (key, cert)
}

/// The criterion in one test: offline is normal, the queue survives a crash, and
/// the device drains it in order exactly once.
#[tokio::test]
async fn an_offline_device_is_queued_durably_and_drains_after_a_crash() {
    let mut relay = Relay::start("durable", &[]);
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());

    let (bob_key, bob_cert) = device(&root, "bob", 1);
    let bob_id = bob_cert.device().clone();
    let (alice_key, alice_cert) = device(&root, "alice", 2);

    // Bob connects once so the relay knows him, then goes away: the queue is for
    // a device that is *known* and offline, not for a stranger.
    {
        let bob = RelayClient::connect(relay.addr, "acct-1", &bob_key, &bob_cert)
            .await
            .expect("bob authenticates");
        drop(bob);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Three messages while he is offline. Each must be *queued*, not refused.
    let mut alice = RelayClient::connect(relay.addr, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice authenticates");
    for index in 0..3u8 {
        let seq = alice
            .send(&bob_id, format!("offline-{index}").as_bytes())
            .await
            .expect("send");
        match alice.next().await.expect("answered") {
            Incoming::Status {
                seq: reported,
                outcome,
            } => {
                assert_eq!(reported, seq);
                assert!(
                    matches!(outcome, Outcome::Queued { .. }),
                    "an offline destination must be queued, got {outcome:?}"
                );
            }
            other => panic!("expected a status, got {other:?}"),
        }
    }

    // The relay dies hard, with those three messages on disk and nothing acked.
    relay.crash_and_restart();

    // Bob returns and drains.
    let mut bob = RelayClient::connect(relay.addr, "acct-1", &bob_key, &bob_cert)
        .await
        .expect("bob reconnects after the restart");
    let (messages, report) = bob.drain_all(1).await.expect("drain");
    let payloads: Vec<String> = messages
        .iter()
        .map(|envelope| String::from_utf8_lossy(&envelope.payload).to_string())
        .collect();
    assert_eq!(
        payloads,
        vec!["offline-0", "offline-1", "offline-2"],
        "the queued messages survive the crash and arrive in seq order"
    );
    assert_eq!(report.delivered, 3);
    assert_eq!(
        report.dropped, 0,
        "nothing was dropped, and the report says so"
    );
    assert_eq!(report.next_seq, 4);

    // Ack, then drain again: nothing comes back.
    bob.ack(3).await.expect("ack");
    let (again, report) = bob.drain_all(4).await.expect("drain again");
    assert!(again.is_empty(), "acked messages are not redelivered");
    assert_eq!(report.delivered, 0);
}

/// A drain that is repeated without an ack redelivers — and the consumer's
/// cursor is what turns that into exactly-once.
#[tokio::test]
async fn a_drain_without_an_ack_redelivers() {
    let relay = Relay::start("redeliver", &[]);
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());

    let (bob_key, bob_cert) = device(&root, "bob", 1);
    let bob_id = bob_cert.device().clone();
    {
        let bob = RelayClient::connect(relay.addr, "acct-1", &bob_key, &bob_cert)
            .await
            .expect("bob authenticates");
        drop(bob);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (alice_key, alice_cert) = device(&root, "alice", 2);
    let mut alice = RelayClient::connect(relay.addr, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice authenticates");
    alice.send(&bob_id, b"one").await.expect("send");
    let _ = alice.next().await.expect("queued");

    // Drain twice without acking: both see the message (at-least-once).
    let mut bob = RelayClient::connect(relay.addr, "acct-1", &bob_key, &bob_cert)
        .await
        .expect("bob reconnects");
    let (first, _) = bob.drain_all(1).await.expect("first drain");
    let (second, _) = bob.drain_all(1).await.expect("second drain");
    assert_eq!(first.len(), 1);
    assert_eq!(second.len(), 1, "an unacked message is redelivered");

    // With the cursor advanced and the ack sent, it stops.
    bob.ack(1).await.expect("ack");
    let (third, _) = bob.drain_all(2).await.expect("third drain");
    assert!(third.is_empty());
}

/// The CLI refuses bounds that make no sense, loudly and before serving.
#[test]
fn the_cli_refuses_impossible_inbox_bounds() {
    let state_dir = std::env::temp_dir().join(format!("arreo-inbox-bad-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state_dir);
    std::fs::create_dir_all(&state_dir).expect("scratch");
    for (flag, value) in [
        ("--inbox-ttl-days", "0"),
        ("--inbox-ttl-days", "366"),
        ("--inbox-ttl-days", "not-a-number"),
        ("--inbox-max-messages", "0"),
        ("--inbox-max-mb", "0"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_arreo-relay"))
            .args([
                "serve",
                "--state-dir",
                &state_dir.display().to_string(),
                flag,
                value,
            ])
            .output()
            .expect("the relay runs");
        assert!(!output.status.success(), "{flag} {value} must be refused");
    }
    let _ = std::fs::remove_dir_all(&state_dir);
}

/// The account registry and the directory keep working while the inbox exists:
/// the migration that added the inbox did not disturb v2's tables.
#[test]
fn the_inbox_migration_leaves_the_directory_intact() {
    let (dir, store) = scratch("migration");
    let root = RootKey::generate().expect("entropy");
    let directory = Directory::new(store.clone());
    directory
        .create_account("acct-1", &root.public(), 1_000)
        .expect("account");
    assert_eq!(store.schema_version().expect("version"), 3);
    assert!(store
        .columns("machine")
        .expect("columns")
        .contains(&"machine_id".to_string()));
    assert!(store
        .columns("inbox")
        .expect("columns")
        .contains(&"bytes".to_string()));
    let _ = std::fs::remove_dir_all(&dir);
}

/// One day, in the units the inbox measures in.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// The `reattach_after_absence_s` budget, read from `perf-budget.toml`.
///
/// Read rather than copied, because a test with its own constant is a second
/// source of truth for one fact — and the file is the law (`bench` reads it the
/// same way, with the same no-TOML-dependency parse).
fn reattach_budget_ms() -> i64 {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("perf-budget.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with("reattach_after_absence_s"))
        .expect("perf-budget.toml has a reattach_after_absence_s row");
    let seconds: i64 = line
        .split("target")
        .nth(1)
        .and_then(|rest| rest.split('=').nth(1))
        .and_then(|value| value.trim().split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|value| value.parse().ok())
        .expect("the row has a numeric target");
    seconds * 1000
}

/// §3.14 in one test: a machine that was away for weeks comes back, is usable
/// again in seconds, drains what accumulated in order and exactly once, and is
/// *told* about what the retention window dropped.
///
/// The absence is simulated by moving the relay's clock across a restart rather
/// than by sleeping (T-0055): a real fifteen-day wait is not a test, and a short
/// TTL with a real sleep is worse than either — slow, and still not the window it
/// claims to exercise.
#[tokio::test]
async fn a_machine_that_was_away_for_weeks_reattaches_and_drains_once() {
    // A ten-day window, so the two halves are distinguishable: a five-day
    // absence is inside it, a twenty-five-day one is past it.
    let mut relay = Relay::start("absence", &["--inbox-ttl-days", "10"]);
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());

    let (alice_key, alice_cert) = device(&root, "alice", 1);
    let (bob_key, bob_cert) = device(&root, "bob", 2);
    let alice_id = alice_cert.device().clone();

    // Alice connects once so the relay knows her — the queue is for a device that
    // is *known* and offline, not for a stranger — then leaves for five days.
    {
        let alice = RelayClient::connect(relay.addr, "acct-1", &alice_key, &alice_cert)
            .await
            .expect("alice authenticates");
        drop(alice);
    }
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Bob sends three messages while she is away.
    let mut bob = RelayClient::connect(relay.addr, "acct-1", &bob_key, &bob_cert)
        .await
        .expect("bob authenticates");
    for index in 0..3u8 {
        let seq = bob
            .send(&alice_id, format!("while-away-{index}").as_bytes())
            .await
            .expect("send");
        match bob.next().await.expect("answered") {
            Incoming::Status {
                seq: reported,
                outcome,
            } => {
                assert_eq!(reported, seq);
                assert!(
                    matches!(outcome, Outcome::Queued { .. }),
                    "an absent machine's mail must be queued, got {outcome:?}"
                );
            }
            other => panic!("expected a status, got {other:?}"),
        }
    }

    // Five days pass (the relay's clock moves; nothing else does).
    relay.restart_with_clock(5 * DAY_MS);

    // Alice returns. The reattach is the dial, the authentication and the drain,
    // timed against the budget the file states.
    let started = Instant::now();
    let mut alice = RelayClient::connect(relay.addr, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice reattaches after five days away");
    let (messages, report) = alice.drain_all(1).await.expect("drain");
    let reattach_ms = started.elapsed().as_millis() as i64;

    let payloads: Vec<String> = messages
        .iter()
        .map(|envelope| String::from_utf8_lossy(&envelope.payload).to_string())
        .collect();
    assert_eq!(
        payloads,
        vec!["while-away-0", "while-away-1", "while-away-2"],
        "what accumulated is delivered in seq order"
    );
    assert_eq!(report.delivered, 3);
    assert_eq!(report.dropped, 0, "inside the window nothing was dropped");
    assert_eq!(report.expired, 0);
    assert_eq!(report.next_seq, 4);

    let budget_ms = reattach_budget_ms();
    assert!(
        reattach_ms < budget_ms,
        "the reattach took {reattach_ms} ms, over the {budget_ms} ms budget"
    );
    println!("absence: reattached after 5 days in {reattach_ms} ms (budget {budget_ms} ms)");

    // Re-running the drained batch re-executes nothing: without an ack the same
    // messages come back (at-least-once), and the ack is what makes it once.
    let (again, _) = alice.drain_all(1).await.expect("drain again");
    assert_eq!(
        again.len(),
        3,
        "an unacked drain redelivers — the contract is at-least-once plus a cursor"
    );
    alice.ack(3).await.expect("ack");
    let (after_ack, report) = alice.drain_all(4).await.expect("drain after ack");
    assert!(
        after_ack.is_empty(),
        "acked messages are not redelivered: {after_ack:?}"
    );
    assert_eq!(report.delivered, 0);

    // Now the window: twenty more days pass with two more messages waiting.
    // Bob reconnects too — the restart that moved the clock ended his session,
    // and a test that assumed otherwise would be testing a socket, not retention.
    drop(alice);
    drop(bob);
    let mut bob = RelayClient::connect(relay.addr, "acct-1", &bob_key, &bob_cert)
        .await
        .expect("bob reconnects after the restart");
    for index in 0..2u8 {
        let seq = bob
            .send(&alice_id, format!("too-late-{index}").as_bytes())
            .await
            .expect("send");
        match bob.next().await.expect("answered") {
            Incoming::Status {
                seq: reported,
                outcome,
            } => {
                assert_eq!(reported, seq);
                assert!(matches!(outcome, Outcome::Queued { .. }), "{outcome:?}");
            }
            other => panic!("expected a status, got {other:?}"),
        }
    }
    relay.restart_with_clock(25 * DAY_MS);

    // Alice returns to nothing but an honest count: the window took the messages,
    // and the relay says so rather than reporting an empty queue.
    let mut alice = RelayClient::connect(relay.addr, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice reattaches after a month away");
    let (late, report) = alice.drain_all(4).await.expect("drain after the window");
    assert!(
        late.is_empty(),
        "messages past the retention window are not delivered: {late:?}"
    );
    assert_eq!(
        report.expired, 2,
        "the two that expired are counted, not silently lost: {report:?}"
    );
    assert_eq!(report.queued, 0);
}
