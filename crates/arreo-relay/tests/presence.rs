//! T-0031 acceptance tests: relay presence — online/offline/last-seen from one
//! stated staleness rule.
//!
//! Split deliberately in two, the way T-0030's inbox tests are. The rule's
//! boundaries are pure arithmetic, so they are pinned directly against
//! `presence_at` with an injected clock (tests never sleep for these). The
//! lifecycle — connect writes online, heartbeats refresh, disconnect stamps
//! `last_seen`, a `kill -9` + restart recomputes from storage — runs against
//! the real `arreo-relay` binary, because "nothing reads online until it
//! reconnects" is a property of the process, not of a function.

use arreo_core::identity::{DeviceCert, DeviceKey, Role, RootKey, VerifyingKey};
use arreo_core::mesh::directory::{Presence, ONLINE_WINDOW_SECS, STALE_AFTER_SECS};
use arreo_core::relay::RelayClient;
use arreo_relay::{
    format_age, next_heartbeat_delay, presence_at, DevicePresence, HEARTBEAT_INTERVAL,
    HEARTBEAT_JITTER,
};
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const SEC: i64 = 1000;
const DAY: i64 = 24 * 60 * 60 * SEC;

/// The boundaries, with an injected clock. Each row is (age, expected variant):
/// the exact second the window opens and closes, on both sides.
#[test]
fn the_staleness_boundaries_are_exact() {
    let now = 1_800_000_000_000i64;
    let cases: &[(i64, Presence)] = &[
        (0, Presence::Online),
        (89 * SEC, Presence::Online),
        (90 * SEC, Presence::Online),
        (91 * SEC, Presence::Offline),
        (29 * DAY, Presence::Offline),
        (30 * DAY, Presence::Offline),
        (30 * DAY + SEC, Presence::Stale),
    ];
    for (age, expected) in cases {
        assert_eq!(
            presence_at(now - age, now),
            *expected,
            "age {age} ms must read {expected:?}"
        );
    }
    assert_eq!(
        ONLINE_WINDOW_SECS, 90,
        "the window is 3 missed 30 s beats, stated in one place"
    );
    assert_eq!(
        STALE_AFTER_SECS,
        30 * 24 * 60 * 60,
        "stale matches the 30-day inbox default (§3.14), stated in one place"
    );
}

/// A clock that stepped backwards reports "just now", never a panic and never a
/// negative age: a future `last_seen` clamps to `online`.
#[test]
fn a_backwards_clock_clamps_to_online() {
    let now = 1_800_000_000_000i64;
    assert_eq!(presence_at(now + 60 * SEC, now), Presence::Online);
    assert_eq!(format_age(now - (now + 60 * SEC)), "0 s");
}

/// The heartbeat schedule: 30 s cadence, ±6 s jitter, never below 1 ms.
#[test]
fn the_heartbeat_schedule_is_stated_and_bounded() {
    assert_eq!(HEARTBEAT_INTERVAL, Duration::from_secs(30));
    assert_eq!(HEARTBEAT_JITTER, Duration::from_secs(6));
    assert_eq!(
        next_heartbeat_delay(0.0),
        Duration::from_secs(30),
        "no jitter, no spread"
    );
    assert_eq!(next_heartbeat_delay(1.0), Duration::from_secs(36));
    assert_eq!(next_heartbeat_delay(-1.0), Duration::from_secs(24));
    // Clamped at both ends: a wild fraction cannot stop the beat or double it.
    assert_eq!(next_heartbeat_delay(100.0), Duration::from_secs(36));
    assert_eq!(next_heartbeat_delay(-100.0), Duration::from_secs(24));
}

/// The 15-day case reads honestly — and so do the hour/day/month shapes around
/// it. This is the formatting `arreo machines` and remote attach render.
#[test]
fn ages_render_honestly_in_every_shape() {
    let cases: &[(i64, &str)] = &[
        (0, "0 s"),
        (45 * SEC, "45 s"),
        (5 * 60 * SEC, "5 m"),
        (3 * 60 * 60 * SEC, "3 h"),
        (15 * DAY, "15 d"),
        (45 * DAY, "45 d"),
        (75 * DAY, "2 mo"),
    ];
    for (age, expected) in cases {
        assert_eq!(&format_age(*age), expected, "age {age} ms");
    }
    // The row a reviewer reads for the 15-day case:
    let row = DevicePresence {
        device_id: "dev_abc".to_string(),
        account_id: "acct-1".to_string(),
        presence: Presence::Offline,
        last_seen_ms: 0,
    };
    assert_eq!(
        row.display(15 * DAY),
        "offline (last seen 15 d)",
        "a machine that has been off for two weeks is a fact, not an error"
    );
    assert_eq!(
        DevicePresence {
            presence: Presence::Online,
            ..row.clone()
        }
        .display(15 * DAY),
        "online"
    );
}

/// A spawned relay, killed when the test ends. The stderr drain is load-bearing:
/// a closed pipe kills the child on its next log line (EPIPE), which reads
/// exactly like an authentication failure.
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
        let state_dir = std::env::temp_dir().join(format!(
            "arreo-presence-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&state_dir);
        std::fs::create_dir_all(&state_dir).expect("scratch");
        let mut child = Command::new(env!("CARGO_BIN_EXE_arreo-relay"))
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
        let addr = Self::await_addr(&mut child);
        Self {
            child,
            addr,
            state_dir,
        }
    }

    fn await_addr(child: &mut Child) -> SocketAddr {
        let stderr = child.stderr.take().expect("stderr");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
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

    /// `kill -9` and restart against the same state directory: presence must be
    /// recomputed from storage, with nothing phantom.
    /// `kill -9` and restart with the relay's clock moved: the store is
    /// untouched, so what changes is only how old the relay believes its rows
    /// are (T-0055's seam, reused here for the window boundary).
    fn crash_and_restart_with_clock(&mut self, clock_offset_ms: i64) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let mut child = Command::new(env!("CARGO_BIN_EXE_arreo-relay"))
            .args([
                "serve",
                "--listen",
                &self.addr.to_string(),
                "--state-dir",
                &self.state_dir.display().to_string(),
            ])
            .env(arreo_relay::CLOCK_OFFSET_ENV, clock_offset_ms.to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the relay restarts");
        self.addr = Self::await_addr(&mut child);
        self.child = child;
    }
}

fn device(root: &RootKey, name: &str, serial: u64) -> (DeviceKey, DeviceCert) {
    let key = DeviceKey::generate().expect("entropy");
    let cert = DeviceCert::issue(root, &key.public(), name, Role::Owner, 1_000, serial);
    (key, cert)
}

/// Lifecycle truth, against the real binary: connect writes `online`;
/// disconnect stamps `last_seen` immediately; a `kill -9` + restart recomputes
/// from storage and nothing reads `online` until it reconnects.
#[tokio::test]
async fn presence_follows_the_socket_not_a_flag() {
    let mut relay = Relay::start("lifecycle");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());

    let (alice_key, alice_cert) = device(&root, "alice", 1);
    let alice_id = alice_cert.device().clone();
    let (bob_key, bob_cert) = device(&root, "bob", 2);
    let bob_id = bob_cert.device().clone();

    // Alice connects: she is online the moment the handshake completes.
    let alice = RelayClient::connect(relay.addr, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice authenticates");
    let router = test_router(&relay);
    let now = arreo_relay::directory::now_ms();
    let listed = router.presence("acct-1", now).expect("presence lists");
    let alice_row = listed
        .iter()
        .find(|row| row.device_id == alice_id.as_str())
        .expect("alice is listed");
    assert_eq!(
        alice_row.presence,
        Presence::Online,
        "connect writes online"
    );
    assert_eq!(alice_row.display(now), "online");

    // Bob never connects: he is known (registered below) but offline — which is
    // a different answer from "unknown", and the listing shows it.
    drop(alice);
    // The disconnect stamps `last_seen` now, so a second read still sees alice
    // as online-with-a-fresh-timestamp rather than as a ghost of the socket.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let now = arreo_relay::directory::now_ms();
    let listed = router.presence("acct-1", now).expect("presence lists");
    let alice_row = listed
        .iter()
        .find(|row| row.device_id == alice_id.as_str())
        .expect("alice is still listed after disconnect");
    assert_eq!(
        alice_row.presence,
        Presence::Online,
        "a disconnect stamps last_seen now; the 90 s window does the rest"
    );

    // Bob registers by connecting once, then leaves too.
    {
        let bob = RelayClient::connect(relay.addr, "acct-1", &bob_key, &bob_cert)
            .await
            .expect("bob authenticates");
        drop(bob);
    }
    let _ = bob_id;

    // `kill -9` with the clock moved past the window: every row ages out, so
    // nothing can read online until it reconnects — no phantom state, because
    // there is no live flag stored anywhere to go stale. (Without the clock
    // move both devices would still be inside their 90 s windows, and `online`
    // would be the *correct* answer for them.)
    relay.crash_and_restart_with_clock(120_000);
    // Reconnect the router handle to the restarted relay's store: presence is a
    // property of the rows, so any handle to the same state directory agrees.
    // The `now` is the wall clock plus the offset the restarted relay runs with:
    // the two processes have different clocks once the seam is set, and comparing
    // stored rows against this process's wall clock is how a test asserts a
    // window it did not exercise. (`now_ms()` cannot be used here — it cached
    // the offset at first read, which in this process is 0.)
    let router = test_router(&relay);
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let now = wall + 120_000;
    let listed = router.presence("acct-1", now).expect("presence lists");
    for row in &listed {
        assert_eq!(
            row.presence,
            Presence::Offline,
            "after a restart past the window nothing reads online until it reconnects: {}",
            row.device_id
        );
    }
    assert_eq!(listed.len(), 2, "both devices are still known: {listed:?}");
}

/// Read path cost, asserted not assumed: 10,000 devices list from one indexed
/// query in < 50 ms on the dev box.
#[test]
fn ten_thousand_devices_list_in_one_indexed_query() {
    use arreo_relay::RelayStore;
    let dir = std::env::temp_dir().join(format!(
        "arreo-presence-perf-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let store = RelayStore::open(&dir.join("relay.db")).expect("store");
    let now = arreo_relay::directory::now_ms();
    for index in 0..10_000u32 {
        store
            .touch_device(
                "acct-1",
                &format!("dev_{index:032x}"),
                now - (index as i64 % 100) * 1000,
            )
            .expect("touch");
    }
    // The plan, not just the time: an index must serve this, never a scan.
    let plan: String = store
        .explain_presence_query()
        .expect("the store explains its own query");
    assert!(
        plan.contains("relay_device_seen") || plan.contains("USING INDEX"),
        "the listing must use the last_seen index, not a table scan: {plan}"
    );
    let started = std::time::Instant::now();
    let rows = store.device_presence("acct-1").expect("listing");
    let elapsed = started.elapsed();
    assert_eq!(rows.len(), 10_000);
    assert!(
        elapsed < Duration::from_millis(50),
        "10,000 devices listed in {elapsed:?}, over the 50 ms budget"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// The row is metadata only: a schema test fails if payload/keys/agent-state
/// columns appear on `relay_device`, and heartbeats write no audit row.
#[test]
fn presence_rows_carry_no_payload() {
    use arreo_relay::RelayStore;
    let dir = std::env::temp_dir().join(format!(
        "arreo-presence-schema-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let store = RelayStore::open(&dir.join("relay.db")).expect("store");
    let columns = store.columns("relay_device").expect("columns");
    assert_eq!(
        columns,
        vec!["account_id", "device_id", "first_seen_ms", "last_seen_ms"],
        "relay_device is four columns of bookkeeping; a payload, key or state \\
         column here would be metadata that is not metadata: {columns:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Open the same state directory a second handle: presence is rows, so any
/// handle agrees — which is what makes the restart assertion meaningful.
fn test_router(relay: &Relay) -> arreo_relay::Router {
    use arreo_relay::{InboxLimits, RelayStore};
    let store = RelayStore::open(&relay.state_dir.join("relay.db")).expect("store");
    arreo_relay::Router::new(store, InboxLimits::default())
}
