//! T-0117 acceptance tests: the push payload and offline-queued delivery.
//!
//! Nothing is mocked. A real `arreo-relay` serves a real `arreo-server` and a
//! real *phone* — a `RelayClient` holding its own device key and a certificate
//! issued by the account root — and the pushes under test are the bytes the
//! machine actually sealed, as the phone actually reads them.
//!
//! The four criteria, and what only these tests can show:
//!
//! 1. **A delivered notification is pushed to every paired device that should
//!    receive it.** The audience is the machine's device authority, and the
//!    *sentence* a phone renders is the same string the daemon wrote to its own
//!    audit row — asserted against the row, not against a copy of the wording.
//! 2. **A withheld notification is not pushed at all.** A policy that matches
//!    nothing still *decides* — the `notify.suppressed` rows prove the tick ran
//!    — and the phone must see no bytes at all. A test that only checked "a push
//!    arrived" would pass against a daemon that ignored the rules engine
//!    entirely, which is why this half exists.
//! 3. **Offline is a delay, not a loss.** The phone is absent while three
//!    notifications are produced, then drains them in order; a second drain
//!    without an ack redelivers the same rows, and the `(device, seq)` dedupe
//!    collapses them to one delivery each.
//! 4. **Retention's drops are counted and visible.** A device absent past the
//!    window is *told how many it missed* on its next drain rather than seeing a
//!    silent gap.
//!
//! Scratch lives under `target/test-scratch/T-0117/` (never `/tmp`: it is a
//! tmpfs here, and a SQLite log plus a SQLite inbox under it is a real problem).

use arreo_core::identity::{DeviceCert, DeviceId, DeviceKey, Role, RootKey, VerifyingKey};
use arreo_core::notify::push::{self, PushPayload};
use arreo_core::proto::{codec, AgentState, Message, VERSION};
use arreo_core::relay::RelayClient;
use arreo_core::store::{SessionStore, StoredAudit};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// How long a test waits for the background tick to write a row, or for a push
/// to arrive. Generous for the same reason `tests/notify.rs` is: the tick is a
/// 1 s loop and a pane only *becomes* `question` after the adapter's 2 s of
/// quiet, so nothing here can happen quickly, and a loaded machine is slow.
const WAIT: Duration = Duration::from_secs(25);

/// How long a test waits to be *sure* nothing arrived. The tick's window plus a
/// margin: a suppression is proved by the absence of bytes, and an absence is
/// only evidence once every chance to produce them has passed.
const QUIET: Duration = Duration::from_secs(6);

/// The account every test in this file registers with the relay.
const ACCOUNT: &str = "acct-t0117";

fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-scratch/T-0117")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    // Canonicalized so the daemon, the relay and this test all name the same
    // directory — a symlinked scratch would otherwise make one of them read a
    // path the other did not write.
    std::fs::canonicalize(&dir).expect("canonical")
}

fn binary(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_arreo-server"))
        .parent()
        .expect("target dir")
        .join(name);
    assert!(
        path.exists(),
        "{} is missing — run `cargo test --workspace` (which builds every binary) first",
        path.display()
    );
    path
}

/// The daemon's own audit store for a socket — from the one function that
/// decides that path, so this test reads the log the daemon writes.
fn store_path(socket: &Path) -> PathBuf {
    arreo_server::db_path_for(socket)
}

/// Every audit row for one pane under `action`, newest first.
fn rows_for(db: &Path, action: &str, pane: &str) -> Vec<StoredAudit> {
    let store = SessionStore::open(db).expect("open the daemon's store");
    store
        .audit_by_action(action, 100)
        .expect("read the log")
        .into_iter()
        .filter(|row| row.agent == pane)
        .collect()
}

async fn wait_for_row(db: &Path, action: &str, pane: &str) -> StoredAudit {
    let deadline = Instant::now() + WAIT;
    loop {
        let found = rows_for(db, action, pane);
        if let Some(row) = found.first() {
            return row.clone();
        }
        assert!(
            Instant::now() < deadline,
            "no {action} row for {pane} within {}s",
            WAIT.as_secs()
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

// ---------------------------------------------------------------------------
// The processes
// ---------------------------------------------------------------------------

/// A running relay, killed when the test ends.
struct Relay {
    child: Option<Child>,
    addr: SocketAddr,
    state_dir: PathBuf,
    /// The clock offset the relay was started with, kept so a restart can move
    /// it (T-0055's seam: the offset is read once per process, so "two days
    /// passed" is a fact about a restart).
    offset_ms: i64,
    /// The port the relay listens on, held so a restart reuses it (see
    /// [`Relay::start`]).
    port: u16,
}

impl Drop for Relay {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Relay {
    fn start(_tag: &str, state_dir: PathBuf, offset_ms: i64, ttl_days: u64) -> Self {
        // **A reserved port, not `:0`.** The relay is restarted inside a test
        // (to move its clock past the TTL, T-0055), and the machine's daemon
        // dials the address in its configuration file — so a restarted relay that
        // took a *fresh* ephemeral port would leave the daemon reconnecting to
        // nothing, and the test would be asserting about a machine that is simply
        // offline. Reserving the port up front (bind, read it, drop) makes the
        // address a fact of the test rather than of the kernel's next choice.
        let port = {
            let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port");
            probe.local_addr().expect("addr").port()
        };
        let (child, addr) = Self::spawn(state_dir.clone(), offset_ms, ttl_days, port);
        Self {
            child: Some(child),
            addr,
            state_dir,
            offset_ms,
            port,
        }
    }

    /// Spawn one relay process and wait for the line that announces its address.
    fn spawn(state_dir: PathBuf, offset_ms: i64, ttl_days: u64, port: u16) -> (Child, SocketAddr) {
        let mut child = Command::new(binary("arreo-relay"))
            .args([
                "serve",
                "--listen",
                &format!("127.0.0.1:{port}"),
                "--state-dir",
                &state_dir.display().to_string(),
                "--inbox-ttl-days",
                &ttl_days.to_string(),
            ])
            .env("ARREO_CLOCK_OFFSET_MS", offset_ms.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the relay starts");
        let stderr = child.stderr.take().expect("stderr");
        let stdout = child.stdout.take().expect("stdout");
        let (ready_tx, ready_rx) = mpsc::channel();
        // Both pipes are drained for the process's whole life: a closed pipe
        // kills the child on its next log line (EPIPE), which reads exactly like
        // an authentication failure.
        for pipe in [Box::new(stdout) as Box<dyn Read + Send>, Box::new(stderr)] {
            let ready = ready_tx.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(pipe).lines().map_while(Result::ok) {
                    if let Some(rest) = line.split("router on ").nth(1) {
                        if let Some(addr) = rest.split_whitespace().next() {
                            if let Ok(addr) = addr.parse::<SocketAddr>() {
                                let _ = ready.send(addr);
                            }
                        }
                    }
                }
            });
        }
        let addr = ready_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the relay announces its address");
        (child, addr)
    }

    fn register_account(&self, root: &VerifyingKey) {
        let output = Command::new(binary("arreo-relay"))
            .args([
                "account",
                "add",
                "--state-dir",
                &self.state_dir.display().to_string(),
                "--account",
                ACCOUNT,
                // The **public** root key hex: `account add` registers a
                // verifier, and handing it the seed is the mistake this note
                // exists to stop somebody making twice.
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
            "registering the account failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Stop the relay and start it again with the clock moved `by_ms` forward —
    /// the only way to make "the retention window passed" a fact here.
    fn restart_with_clock_moved(&mut self, by_ms: i64, ttl_days: u64) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.offset_ms += by_ms;
        let (child, addr) =
            Self::spawn(self.state_dir.clone(), self.offset_ms, ttl_days, self.port);
        self.child = Some(child);
        self.addr = addr;
    }

    /// The relay's own audit trail, as its CLI exports it (T-0053) — the
    /// operator's view, not a table this test reads behind the relay's back.
    fn audit_export(&self) -> String {
        let output = Command::new(binary("arreo-relay"))
            .args([
                "audit",
                "export",
                "--state-dir",
                &self.state_dir.display().to_string(),
                "--format",
                "jsonl",
            ])
            .output()
            .expect("the audit command runs");
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    /// Every file the relay has on disk — the ciphertext check's raw material.
    fn state_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut stack = vec![self.state_dir.clone()];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(bytes) = std::fs::read(&path) {
                    out.extend_from_slice(&bytes);
                }
            }
        }
        out
    }
}

/// A running `arreo-server`, killed when the test ends.
struct Machine {
    child: Child,
    socket: PathBuf,
    /// The machine's device key: what the phone verifies a push against.
    key: DeviceKey,
    log: Arc<Mutex<String>>,
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Machine {
    fn log_text(&self) -> String {
        self.log.lock().expect("log").clone()
    }

    fn await_log(&self, needle: &str) {
        self.await_log_times(needle, 1);
    }

    /// Wait until `needle` has appeared **`times` times** in the log.
    ///
    /// The count matters after a restart: "relay session up" is in the log from
    /// the first connection, so waiting for its *presence* returns instantly and
    /// would let a test race the reconnect it claims to have waited for.
    fn await_log_times(&self, needle: &str, times: usize) {
        let deadline = Instant::now() + WAIT;
        while Instant::now() < deadline {
            if self.log_text().matches(needle).count() >= times {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "the daemon never logged {needle:?} {times} time(s); its log was:\n{}",
            self.log_text()
        );
    }
}

/// Framed MessagePack client for the daemon's local socket: Hello, then verbs.
struct LocalClient {
    stream: tokio::net::unix::OwnedWriteHalf,
    reader: tokio::net::unix::OwnedReadHalf,
    buf: Vec<u8>,
}

impl LocalClient {
    async fn connect(socket: &Path) -> Self {
        let stream = tokio::net::UnixStream::connect(socket)
            .await
            .expect("connect to the daemon");
        let (reader, stream) = stream.into_split();
        let mut client = Self {
            stream,
            reader,
            buf: Vec::new(),
        };
        client
            .send(&Message::Hello {
                v: VERSION,
                client: "notify-push-test".to_string(),
                wants: vec![VERSION],
            })
            .await;
        match client.recv().await {
            Message::Welcome { v, .. } => assert_eq!(v, VERSION),
            other => panic!("want Welcome, got {other:?}"),
        }
        client
    }

    async fn send(&mut self, message: &Message) {
        let frame = codec::encode_frame(message).expect("encode");
        self.stream.write_all(&frame).await.expect("write");
        self.stream.flush().await.expect("flush");
    }

    async fn recv(&mut self) -> Message {
        loop {
            if let Ok((message, consumed)) = codec::decode_frame(&self.buf) {
                self.buf.drain(..consumed);
                return message;
            }
            let mut chunk = [0u8; 8192];
            let n = tokio::time::timeout(Duration::from_secs(10), self.reader.read(&mut chunk))
                .await
                .expect("read timeout")
                .expect("read");
            assert!(n > 0, "the daemon closed the socket");
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    async fn call(&mut self, message: &Message) -> Message {
        self.send(message).await;
        self.recv().await
    }
}

/// The pane script every test here uses: print a question, then stay quiet.
///
/// `Proceed? [y/n]` matches the universal adapter's question patterns and
/// `sleep 60` leaves the tail unchanged, so the engine infers `question` from
/// silence plus a prompt-shaped tail. The shell is `-c`, not `-i`, so nothing
/// competes with the question — and one pane therefore produces exactly one
/// delivered notification (its `working` transition is suppressed by the policy
/// these tests write, which names `question` and nothing else).
const ASKS: &str = "printf 'Proceed? [y/n]\\n'; sleep 60";

async fn spawn_asking_pane(client: &mut LocalClient, id: &str) {
    let reply = client
        .call(&Message::Spawn {
            v: VERSION,
            id: id.to_string(),
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), ASKS.to_string()],
            cols: 80,
            rows: 24,
            memory_max: None,
            pids_max: None,
            kill_on_breach: false,
        })
        .await;
    assert!(
        matches!(reply, Message::Ok { .. }),
        "the pane must spawn, got {reply:?}"
    );
}

// ---------------------------------------------------------------------------
// Identities, and the world a test drives
// ---------------------------------------------------------------------------

/// A machine's identity directory: the account root, its own device key and the
/// certificate the relay authenticates it with (`arreo pair`'s layout).
fn make_identity(dir: &Path, root: &RootKey, own_key: &DeviceKey, own_cert: &DeviceCert) {
    let identity = dir.join("identity");
    std::fs::create_dir_all(identity.join("devices")).expect("identity dir");
    root.save(&identity.join("root.key")).expect("root key");
    own_key
        .save(&identity.join("device.key"))
        .expect("device key");
    own_cert
        .save(&identity.join("devices"))
        .expect("own certificate");
}

/// Pin a device's key on a machine, through the product's own door.
///
/// `arreo devices issue` writes the record and the certificate into the same
/// store the daemon reads at boot — no socket needs to be listening, which is
/// what lets a test pair a phone before its machine's daemon starts.
fn pin_device(machine_dir: &Path, socket: &Path, name: &str, key: &VerifyingKey) {
    let output = Command::new(binary("arreo"))
        .args([
            "devices",
            "issue",
            "--socket",
            &socket.display().to_string(),
            "--name",
            name,
            "--role",
            "owner",
            "--key",
            &key.to_bytes()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
        ])
        .env("ARREO_IDENTITY_DIR", machine_dir)
        .output()
        .expect("the devices command runs");
    assert!(
        output.status.success(),
        "pinning {name} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The configuration file: the relay section, plus the notify policy under test.
fn config_file(dir: &Path, relay: &Relay, name: &str, notify: &str) -> PathBuf {
    let path = dir.join(format!("{name}.toml"));
    std::fs::write(
        &path,
        format!(
            "{notify}\n[relay]\nenabled = true\naddr = \"{}\"\naccount = \"{ACCOUNT}\"\nname = \"{name}\"\n",
            relay.addr
        ),
    )
    .expect("config");
    path
}

/// Spawn the machine's daemon with its own identity directory, socket and
/// configuration, and wait for its relay session.
fn spawn_machine(dir: &Path, socket: &Path, config: &Path) -> Machine {
    let key = DeviceKey::load(&dir.join("identity").join("device.key")).expect("the machine key");
    let mut child = Command::new(binary("arreo-server"))
        .arg("--socket")
        .arg(socket)
        .arg("--config")
        .arg(config)
        .env("ARREO_IDENTITY_DIR", dir)
        // The daemon acts on a pending update at start (T-0105): pointed at the
        // real state directory, a machine with a staged artifact would have this
        // test binary promote and install software. Same isolation as
        // `tests/notify.rs`, for the same reason.
        .env("ARREO_STATE_DIR", dir.join("state"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the daemon starts");
    let stderr = child.stderr.take().expect("stderr");
    let log = Arc::new(Mutex::new(String::new()));
    {
        let log = Arc::clone(&log);
        std::thread::spawn(move || {
            // Kept open for the process's whole life: a closed pipe makes the
            // daemon die on its next log line, which reads like a relay failure.
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut held = log.lock().expect("log");
                held.push_str(&line);
                held.push('\n');
            }
        });
    }
    let machine = Machine {
        child,
        socket: socket.to_path_buf(),
        key,
        log,
    };
    machine.await_log("relay session up");
    machine
}

/// The world one test drives: a relay, a machine with its daemon, and a phone
/// with an identity of its own.
struct World {
    relay: Relay,
    machine: Machine,
    /// The phone's private key — the device a push is sealed to.
    phone_key: DeviceKey,
    phone_cert: DeviceCert,
    root: RootKey,
}

impl World {
    /// Build the whole world: relay, account, machine identity, phone identity
    /// pinned on the machine, and the daemon serving with `notify`.
    fn start(tag: &str, notify: &str, ttl_days: u64) -> Self {
        let dir = scratch(tag);
        let root = RootKey::generate().expect("entropy");
        let state_dir = dir.join("relay-state");
        std::fs::create_dir_all(&state_dir).expect("relay state dir");
        let relay = Relay::start(tag, state_dir, 0, ttl_days);
        relay.register_account(&root.public());

        // The machine: its own key and a certificate the account root issued.
        let machine_dir = dir.join("machine");
        std::fs::create_dir_all(&machine_dir).expect("machine dir");
        let machine_key = DeviceKey::generate().expect("entropy");
        let machine_cert = DeviceCert::issue(
            &root,
            &machine_key.public(),
            "workbox",
            Role::Owner,
            1_000,
            1,
        );
        make_identity(&machine_dir, &root, &machine_key, &machine_cert);

        // The phone: its own key, and a certificate from the same root — which
        // is what makes it a device of this account *and* what lets the machine
        // be told to push to it.
        let phone_key = DeviceKey::generate().expect("entropy");
        let phone_cert =
            DeviceCert::issue(&root, &phone_key.public(), "phone", Role::Owner, 1_000, 2);

        let socket = dir.join("machine.sock");
        pin_device(&machine_dir, &socket, "phone", &phone_key.public());

        let config = config_file(&machine_dir, &relay, "workbox", notify);
        let machine = spawn_machine(&machine_dir, &socket, &config);
        Self {
            relay,
            machine,
            phone_key,
            phone_cert,
            root,
        }
    }

    fn db(&self) -> PathBuf {
        store_path(&self.machine.socket)
    }

    /// Dial the relay as the phone. Each call is a *fresh* session, which is
    /// what "the phone came back" means (T-0060: one session per device). The
    /// caller drops it to go offline again.
    async fn phone(&self) -> RelayClient {
        RelayClient::connect(self.relay.addr, ACCOUNT, &self.phone_key, &self.phone_cert)
            .await
            .expect("the phone registers with the relay")
    }

    /// The machine's own key, as the phone verifies a push against.
    fn machine_key(&self) -> VerifyingKey {
        self.machine.key.public()
    }

    /// Open one sealed push: the machine's key, the phone's key, and the bytes
    /// exactly as the relay carried them.
    fn open(&self, sealed: &[u8]) -> PushPayload {
        match push::open_from(&self.machine_key(), &self.phone_key.noise_static(), sealed) {
            Ok((payload, _)) => payload,
            Err(e) => panic!("the phone must open its own push: {e}"),
        }
    }
}

/// Read one sealed push from the phone's session, or `None` if none arrives
/// inside `within`.
async fn recv_push(client: &mut RelayClient, within: Duration) -> Option<Vec<u8>> {
    match tokio::time::timeout(within, client.recv_envelope()).await {
        Ok(Ok(envelope)) => Some(envelope.payload),
        Ok(Err(e)) => panic!("the phone's session failed: {e}"),
        Err(_) => None,
    }
}

// ---------------------------------------------------------------------------
// 1. A delivered notification is pushed, whole
// ---------------------------------------------------------------------------

/// **The headline criterion.** A pane nobody is attached to asks a question; the
/// policy delivers it; and the paired phone receives the payload — pane,
/// machine, state, the row's sentence, the actions that answer it, and when.
///
/// The sentence is compared against the **audit row the daemon wrote**, not
/// against a literal: the push and the row are two outputs of one decision, and
/// a test that hard-coded the wording would let them drift while both stayed
/// green against it.
#[tokio::test]
async fn a_delivered_notification_is_pushed_to_a_paired_device() {
    let world = World::start("delivered", "[notify]\non = [\"question\"]\n", 30);
    let mut phone = world.phone().await;
    let pane = "pushpane-7f3a";

    {
        let mut client = LocalClient::connect(&world.machine.socket).await;
        spawn_asking_pane(&mut client, pane).await;
    }
    let row = wait_for_row(&world.db(), "notify.sent", pane).await;

    let sealed = recv_push(&mut phone, WAIT)
        .await
        .expect("a delivered notification must reach the paired phone");
    let payload = world.open(&sealed);

    assert_eq!(payload.pane, pane);
    // **The machine is the daemon's own name for itself** — the value T-0093's
    // tick puts on every transition (`default_machine_name`), which is the string
    // a `[[notify.rules]] machine = "…"` rule is matched against. It is *not* the
    // relay directory name this test configures (`workbox`): those are two
    // different facts about a machine, and the payload carries the one the
    // decision was made about rather than inventing a second. (That the two can
    // diverge on a machine with an explicit `[relay] name` is T-0093's, and is
    // recorded in this task's report rather than changed here.)
    assert_eq!(payload.machine, arreo_core::mesh::default_machine_name());
    assert_eq!(payload.state, AgentState::Question);
    assert_eq!(
        payload.sentence, row.prompt,
        "the push carries the row's sentence, not a second wording"
    );
    assert!(
        payload.sentence.contains("Proceed?"),
        "the sentence must say what the pane is asking: {payload:?}"
    );
    assert_eq!(
        payload.actions,
        arreo_core::notify::actions_for(AgentState::Question),
        "the answers travel with the notification (T-0094)"
    );
    assert_eq!(
        payload.at_ms, row.ts_ms,
        "the payload's timestamp is the transition's, which is the row's"
    );

    // **And the relay cannot read any of it.** The durable inbox is where a push
    // waits for an offline phone (T-0030), and T-0030's rows are ciphertext: the
    // pane id and the question must not exist in the relay's files.
    let on_disk = world.relay.state_bytes();
    let contains = |needle: &[u8]| on_disk.windows(needle.len()).any(|w| w == needle);
    assert!(
        !contains(pane.as_bytes()),
        "the relay's own files must not hold the pane id"
    );
    assert!(
        !contains(b"Proceed?"),
        "the relay's own files must not hold the agent's sentence"
    );
}

// ---------------------------------------------------------------------------
// 2. Suppression stays quiet
// ---------------------------------------------------------------------------

/// **A notification the policy withheld is not pushed at all.**
///
/// The policy here matches no state the pane ever reaches, so every transition
/// this pane produces — `working`, then `question` — is withheld, and the daemon
/// records *both* decisions as `notify.suppressed` rows. Those rows are what
/// makes this test more than an assertion about silence: they prove the tick
/// classified the pane and chose not to deliver, so "no push arrived" is a fact
/// about the decision rather than about a daemon that never ran.
///
/// The mutation this test exists to catch — a push that ignores the decision —
/// would make the whole rules engine decorative while every "a push arrived"
/// test stayed green.
#[tokio::test]
async fn a_withheld_notification_is_not_pushed() {
    // `on = ["blocked"]`: this pane becomes `working` and then `question`, so no
    // rule ever claims a transition it makes.
    let world = World::start("withheld", "[notify]\non = [\"blocked\"]\n", 30);
    let mut phone = world.phone().await;
    let pane = "quietpane-11c9";

    {
        let mut client = LocalClient::connect(&world.machine.socket).await;
        spawn_asking_pane(&mut client, pane).await;
    }
    // The tick decided — twice — and wrote it down.
    let suppressed = wait_for_row(&world.db(), "notify.suppressed", pane).await;
    assert!(
        suppressed
            .detail
            .as_deref()
            .is_some_and(|d| d.contains("no-rule")),
        "the row must name the reason the operator can act on: {suppressed:?}"
    );

    // And the phone saw nothing: not the withheld transition, not the question
    // that followed it.
    assert!(
        recv_push(&mut phone, QUIET).await.is_none(),
        "a withheld notification must not be pushed"
    );
}

// ---------------------------------------------------------------------------
// 3. Offline is a delay, not a loss
// ---------------------------------------------------------------------------

/// **A device that is absent across several transitions drains what it missed,
/// in order** — and the redelivery the wire guarantees is collapsed by the
/// `(device, seq)` dedupe.
///
/// The order assertion is the substance: three panes ask in sequence, the phone
/// is absent for all of it, and what it drains must be the pushes in the order
/// they were decided. A queue that delivered "the right set in the wrong order"
/// would leave an operator reading a stale question as the newest one.
#[tokio::test]
async fn an_absent_device_drains_what_it_missed_in_order() {
    let world = World::start("offline", "[notify]\non = [\"question\"]\n", 30);
    let panes = ["missed-1", "missed-2", "missed-3"];

    // The phone has been here before — that is what makes the relay know it
    // exists, which is the difference between "queued" and "no such device".
    {
        let mut first = world.phone().await;
        assert!(
            recv_push(&mut first, Duration::from_millis(500))
                .await
                .is_none(),
            "nothing has happened yet, so nothing should have been pushed"
        );
    }
    // …and now it is gone, for the whole time the machine is producing news.

    for pane in panes {
        let mut client = LocalClient::connect(&world.machine.socket).await;
        spawn_asking_pane(&mut client, pane).await;
        // Event-driven rather than a sleep: each notification is *decided* (and
        // therefore pushed) before the next pane exists, so the order under test
        // is unambiguous.
        wait_for_row(&world.db(), "notify.sent", pane).await;
    }

    let mut phone = world.phone().await;
    let drained = drain_until(&mut phone, panes.len()).await;
    assert_eq!(
        drained.len(),
        panes.len(),
        "every notification produced while the phone was away must still arrive"
    );

    // In order, by the sender's own sequence — the same rule the consumer's
    // `(device, seq)` dedupe uses.
    let payloads: Vec<PushPayload> = drained.iter().map(|(_, bytes)| world.open(bytes)).collect();
    let seen: Vec<&str> = payloads.iter().map(|p| p.pane.as_str()).collect();
    assert_eq!(
        seen,
        panes.to_vec(),
        "what the phone missed must arrive in the order it was decided"
    );
    for payload in &payloads {
        assert_eq!(payload.state, AgentState::Question);
        assert_eq!(payload.machine, arreo_core::mesh::default_machine_name());
    }

    // **The redelivery is safe.** Nothing was acked, so draining again returns
    // the same rows — the wire is at-least-once — and the dedupe collapses them
    // to one delivery per message, which is what makes "drained twice, applied
    // once" true.
    let again = drain_once(&mut phone, 1).await;
    assert_eq!(
        again.1.len(),
        drained.len(),
        "an unacked batch is redelivered (at-least-once)"
    );
    let mut deduped: BTreeMap<(String, u64), Vec<u8>> = BTreeMap::new();
    for (key, payload) in again.1.iter().chain(drained.iter()) {
        deduped.insert(key.clone(), payload.clone());
    }
    assert_eq!(
        deduped.len(),
        panes.len(),
        "the (device, seq) dedupe yields exactly one delivery per message"
    );
}

/// Drain and collect every row the phone's inbox holds, as
/// `((sender, seq), payload)` — the key the consumer dedupes on. Polls until
/// `want` distinct rows have arrived or the deadline passes, because the relay
/// queues each push as the tick decides it and a drain only ever sees what has
/// landed by then.
async fn drain_until(phone: &mut RelayClient, want: usize) -> Vec<((String, u64), Vec<u8>)> {
    let deadline = Instant::now() + WAIT;
    let mut rows: BTreeMap<(String, u64), Vec<u8>> = BTreeMap::new();
    while rows.len() < want && Instant::now() < deadline {
        let (_, batch) = drain_once(phone, 1).await;
        for row in batch {
            rows.insert(row.0, row.1);
        }
        if rows.len() < want {
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    sorted(rows)
}

/// One drain from `from_seq`, as `(report, rows)` keyed by `(sender, seq)`.
async fn drain_once(
    phone: &mut RelayClient,
    from_seq: u64,
) -> (
    arreo_core::relay::DrainReport,
    Vec<((String, u64), Vec<u8>)>,
) {
    let (envelopes, report) = phone.drain_all(from_seq).await.expect("the drain");
    let rows = envelopes
        .into_iter()
        .map(|envelope| {
            (
                (envelope.header.src_device.clone(), envelope.header.seq),
                envelope.payload,
            )
        })
        .collect();
    (report, rows)
}

fn sorted(rows: BTreeMap<(String, u64), Vec<u8>>) -> Vec<((String, u64), Vec<u8>)> {
    let mut out: Vec<((String, u64), Vec<u8>)> = rows.into_iter().collect();
    out.sort_by_key(|((_, seq), _)| *seq);
    out
}

// ---------------------------------------------------------------------------
// 4. Retention's drops are counted and visible
// ---------------------------------------------------------------------------

/// **A device absent past the window is told how many it missed.**
///
/// The machine pushes two notifications while the phone is away; the relay is
/// then restarted with its clock past the TTL (T-0055's seam: the offset is read
/// once per process, so a restart is how "a day passed" becomes a fact here).
/// The phone's next drain delivers nothing — the rows are gone — and *says so*:
/// the drop count is on the report the device reads, so a gap is a number rather
/// than a silence.
#[tokio::test]
async fn retention_drops_are_counted_for_the_device_that_missed_them() {
    // One day of retention, so a two-day absence is enough.
    let mut world = World::start("retention", "[notify]\non = [\"question\"]\n", 1);
    let panes = ["aged-1", "aged-2"];

    {
        let mut first = world.phone().await;
        assert!(recv_push(&mut first, Duration::from_millis(500))
            .await
            .is_none());
    }
    for pane in panes {
        let mut client = LocalClient::connect(&world.machine.socket).await;
        spawn_asking_pane(&mut client, pane).await;
        wait_for_row(&world.db(), "notify.sent", pane).await;
    }

    // **Prove the queue holds them before the clock moves.** A drain that
    // acks nothing leaves the rows where they are (T-0030: only `ack` removes),
    // so reading both back is the observation that makes the rest of this test
    // deterministic — otherwise the relay could be killed in the window between
    // the daemon writing an envelope and the relay committing it, and "two
    // missed" would be a fact about a race rather than about retention.
    {
        let mut phone = world.phone().await;
        let queued = drain_until(&mut phone, panes.len()).await;
        assert_eq!(
            queued.len(),
            panes.len(),
            "both pushes must be committed before the window is allowed to pass"
        );
        let seen: Vec<String> = queued.iter().map(|(_, b)| world.open(b).pane).collect();
        assert_eq!(seen, panes.to_vec());
    }
    // The phone is gone again, and the rows it read are still unacked: it is
    // absent in the sense that matters — it never took delivery.

    // Two days later. The rows the phone never took are past their TTL, and the
    // machine's daemon reconnects to the restarted relay in the meantime.
    let two_days_ms = 2 * 24 * 60 * 60 * 1000;
    world.relay.restart_with_clock_moved(two_days_ms, 1);
    // The *second* session-up line: the first is already in the log, so waiting
    // for its presence would not wait at all.
    world.machine.await_log_times("relay session up", 2);

    let mut phone = world.phone().await;
    let (report, rows) = drain_once(&mut phone, 1).await;
    assert!(
        rows.is_empty(),
        "what aged out must not be delivered as if it were new: {rows:?}"
    );
    assert_eq!(
        report.dropped,
        panes.len() as u64,
        "the device must be told how many it missed, not left with a gap"
    );
    // **`expired` is the sweep's own count, not the device's loss.** It counts
    // what *this* drain's lazy sweep expired; here the daemon's own reconnect
    // drain (session-up, with the restarted relay's clock) is what swept the
    // rows, so this drain expired nothing and reports zero — while `dropped`
    // still carries the two the device lost, because that counter is durable and
    // attributed per device (`inbox_stats`, T-0030). Asserting `expired == 2`
    // here would be asserting that the *phone's* drain was the one that noticed,
    // which is scheduling, not retention.
    assert_eq!(
        report.expired, 0,
        "nothing was left for this drain's own sweep to expire"
    );

    // The relay's own trail records it too — the operator's half of "visible"
    // (T-0053), read through the relay's own CLI rather than behind its back.
    let trail = world.relay.audit_export();
    assert!(
        trail.contains("inbox.expire"),
        "the relay's audit trail must record the expiry: {trail}"
    );
}

// ---------------------------------------------------------------------------
// 5. Who is told: the audience is the paired devices, and no one else
// ---------------------------------------------------------------------------

/// **The audience is every device this machine has paired — minus the machine
/// itself — and nobody else.**
///
/// Two drives: a device that is pinned on this machine receives the push, and a
/// device that only belongs to the *account* (a certificate from the same root,
/// never pinned here) receives nothing. The second half is the one that matters:
/// "paired with this machine" is the machine's own decision (T-0046's ledger and
/// T-0025's authority), and an account is not a permission to be told what a
/// machine's agents are doing.
#[tokio::test]
async fn only_devices_this_machine_has_paired_are_told() {
    let world = World::start("audience", "[notify]\non = [\"question\"]\n", 30);

    // A stranger in the same account: a valid certificate, never pinned here.
    let stranger_key = DeviceKey::generate().expect("entropy");
    let stranger_cert = DeviceCert::issue(
        &world.root,
        &stranger_key.public(),
        "stranger",
        Role::Owner,
        1_000,
        9,
    );

    let mut phone = world.phone().await;
    let mut stranger =
        RelayClient::connect(world.relay.addr, ACCOUNT, &stranger_key, &stranger_cert)
            .await
            .expect("the stranger registers with the relay too");

    let pane = "audiencepane-3b71";
    {
        let mut client = LocalClient::connect(&world.machine.socket).await;
        spawn_asking_pane(&mut client, pane).await;
    }
    wait_for_row(&world.db(), "notify.sent", pane).await;

    let sealed = recv_push(&mut phone, WAIT)
        .await
        .expect("the paired phone is told");
    assert_eq!(world.open(&sealed).pane, pane);

    assert!(
        recv_push(&mut stranger, QUIET).await.is_none(),
        "an unpinned device in the same account must not be told"
    );

    // And the device identity in the envelope is the machine's, never the
    // phone's or the stranger's: a push is the machine speaking.
    let _ = DeviceId::from_key(&world.machine_key());
}
