//! T-0046 acceptance tests: per-machine device trust, against a real daemon.
//!
//! The rule and the ledger have unit tests (`arreo-core`'s `mesh::trust`,
//! `mesh::ledger` and the store). What only a real daemon can show is the
//! **gate**: a device that authenticates perfectly and is still refused because
//! this machine never granted it — and the device that was granted when it was
//! issued getting in.
//!
//! **Run these with `cargo test --workspace`** (or build both crates first):
//! `cargo test -p arreo-server` does not build the `arreo` binary these tests
//! spawn, so a CLI change would be silently tested against a stale binary.
//!
//! ## What is *not* covered here, and why
//!
//! The **boot backfill** (T-0046's migration: a machine that upgrades has devices
//! pinned and no grants, so the daemon grants them once) has no test in this file.
//! It is covered by seven unit tests on the ledger itself
//! (`arreo_core::mesh::ledger`), and end to end by the transcript in
//! `.loop/evidence/T-0046/backfill.txt`.
//!
//! It has no test *here* because it needs a store written before the daemon
//! boots, and this harness does not deliver that: a store the test (or a CLI the
//! test spawns) writes is **not visible** to a daemon spawned afterwards — the
//! daemon opens a fresh, empty database at the same path, its own `read_dir`
//! showing a different inode for the same name, and it bootstraps a new root key
//! beside a `root.key` it also cannot see. The identical sequence run from a
//! shell works (the transcript above), so this is an artifact of the test
//! environment rather than of the product; the shell transcript is the e2e proof
//! until someone can explain the mechanism.

use arreo_core::identity::authority::{sidecar_db, Layout};
use arreo_core::identity::{DeviceAuthority, DeviceId, DeviceKey, Role, RootKey};
use arreo_core::mesh::TrustLedger;
use arreo_core::proto::{codec, Message, VERSION};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// One machine: a real daemon, its identity directory, and what it logged.
struct Machine {
    dir: PathBuf,
    socket: PathBuf,
    child: Child,
    log: Arc<Mutex<String>>,
}

impl Drop for Machine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Machine {
    /// The scratch identity layout.
    ///
    /// Built explicitly rather than with `Layout::for_socket`, because that
    /// resolves `identity_root()` from `ARREO_IDENTITY_DIR` **in this process** —
    /// and setting that here would be a data race with every other test in the
    /// binary. The daemon child gets it through its own environment; the test
    /// builds the same three paths by hand.
    fn layout_for(dir: &std::path::Path, socket: &std::path::Path) -> Layout {
        Layout {
            root_key: dir.join("identity").join("root.key"),
            cert_dir: dir.join("identity").join("devices"),
            store: sidecar_db(socket),
        }
    }

    fn scratch(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "arreo-trust-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("identity")).expect("scratch identity dir");
        let socket = dir.join("arreo.sock");
        (dir, socket)
    }

    /// A daemon on its own scratch identity, ready for remote sessions.
    fn start(tag: &str) -> Self {
        let (dir, socket) = Self::scratch(tag);
        Self::spawn(dir, socket)
    }

    fn spawn(dir: PathBuf, socket: PathBuf) -> Self {
        let mut child = Command::new(binary("arreo-server"))
            .arg("--socket")
            .arg(&socket)
            .env("ARREO_IDENTITY_DIR", &dir)
            .env("ARREO_TRANSPORT_TEST_LISTEN", "127.0.0.1:0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the daemon starts");

        let log = Arc::new(Mutex::new(String::new()));
        {
            let sink = Arc::clone(&log);
            let stderr = child.stderr.take().expect("stderr");
            std::thread::spawn(move || {
                // Drain for the process's whole life: dropping the pipe would
                // kill the daemon on its next log line (EPIPE), which reads like
                // an authentication failure.
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let mut held = sink.lock().expect("log");
                    held.push_str(&line);
                    held.push('\n');
                }
            });
        }
        let machine = Self {
            dir,
            socket,
            child,
            log,
        };
        machine.await_socket();
        machine
    }

    fn await_socket(&self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&self.socket).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "the daemon never served {}; its log was:\n{}",
            self.socket.display(),
            self.log_text()
        );
    }

    fn log_text(&self) -> String {
        self.log.lock().expect("log").clone()
    }

    fn layout(&self) -> Layout {
        Self::layout_for(&self.dir, &self.socket)
    }

    fn root(&self) -> RootKey {
        RootKey::load_or_generate(&self.dir.join("identity").join("root.key")).expect("root key")
    }

    /// This machine's own ledger, as the test sees it.
    fn ledger(&self) -> TrustLedger {
        TrustLedger::open(
            &sidecar_db(&self.socket),
            &self.dir.join("identity").join("root.key"),
            "test-machine".to_string(),
        )
        .expect("ledger")
    }

    /// Pin a device **without granting it** — the state a phone paired to a
    /// *different* machine is in here: it holds a certificate this account
    /// issued, and this machine has never decided anything about it.
    ///
    /// Pinned with `DeviceAuthority` rather than through the CLI, because the CLI
    /// now grants on issue (that is the point of T-0046's grant-on-issue) and this
    /// state must not be reachable through the product's own doors. It is
    /// reachable in reality: any device pinned before this code existed looks
    /// exactly like this.
    fn pin_only(&self, name: &str, role: Role, key: &DeviceKey) {
        let mut authority = DeviceAuthority::load(self.layout()).expect("authority");
        authority
            .issue(name, role, &key.public())
            .expect("pin the device");
    }

    fn remote_addr(&self) -> std::net::SocketAddr {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Some(addr) = self
                .log_text()
                .lines()
                .find_map(|line| line.split("listening on ").nth(1))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|addr| addr.parse().ok())
            {
                return addr;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "the daemon never opened the loopback seam; its log was:\n{}",
            self.log_text()
        );
    }

    /// Pin a device **through the product's own door** (`arreo devices issue`),
    /// which is how every operator and every test adds one.
    fn issue_via_cli(&self, name: &str, role: &str, key: &DeviceKey) {
        let output = Command::new(binary("arreo"))
            .args([
                "devices",
                "issue",
                "--socket",
                &self.socket.display().to_string(),
                "--name",
                name,
                "--role",
                role,
                "--key",
                &key.public_hex(),
            ])
            .env("ARREO_IDENTITY_DIR", &self.dir)
            .output()
            .expect("the CLI runs");
        assert!(
            output.status.success(),
            "issuing {name} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Open a remote session as `key` and return the reply to `Hello`.
    ///
    /// The reply is what the gate produces: `Welcome` when the machine accepts
    /// the session, an `Error` when it refuses the verb.
    fn hello_as(&self, key: &DeviceKey, id: &DeviceId) -> Message {
        self.converse(key, id, &[])
            .into_iter()
            .next()
            .expect("a reply to Hello")
    }

    /// As [`Machine::converse`], but `between` runs after the handshake and before
    /// the messages — the shape a test needs to change this machine's state
    /// *while a session is live*.
    fn converse_interleaved<'a>(
        &self,
        key: &DeviceKey,
        id: &DeviceId,
        messages: &[Message],
        between: impl FnOnce() + 'a,
    ) -> Vec<Message> {
        self.talk(key, id, messages, Some(Box::new(between)))
    }

    /// Open a session, say Hello, then send each message in turn, returning every
    /// reply. Stops early if the session is refused or closed — a refused
    /// handshake ends the session, and pretending otherwise would make the test
    /// invent replies.
    fn converse(&self, key: &DeviceKey, id: &DeviceId, messages: &[Message]) -> Vec<Message> {
        self.talk(key, id, messages, None)
    }

    fn talk(
        &self,
        key: &DeviceKey,
        id: &DeviceId,
        messages: &[Message],
        between: Option<Box<dyn FnOnce() + '_>>,
    ) -> Vec<Message> {
        let root = self.root();
        let addr = self.remote_addr();
        let log_text = self.log_text();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let endpoint = arreo_core::transport::client_endpoint().expect("client endpoint");
            let mut channel = match arreo_core::transport::open_session(
                &endpoint,
                addr,
                &key.noise_static(),
                &id.display_id(),
                &root.public(),
            )
            .await
            {
                Ok(channel) => channel,
                Err(e) => panic!(
                    "the remote session opens: {e}\nthe daemon's log was:\n{}",
                    log_text
                ),
            };
            let mut replies = Vec::new();
            let mut buf = Vec::new();
            let mut to_send: Vec<Message> = vec![Message::Hello {
                v: VERSION,
                client: "trust-test".to_string(),
                wants: vec![VERSION],
            }];
            to_send.extend(messages.iter().cloned());
            let mut between = between;
            for message in to_send {
                let frame = codec::encode_frame(&message).expect("encode");
                {
                    use tokio::io::AsyncWriteExt;
                    if channel.write_all(&frame).await.is_err() {
                        break;
                    }
                    if channel.flush().await.is_err() {
                        break;
                    }
                }
                match read_reply(&mut channel, &mut buf).await {
                    Some(reply) => {
                        // A refused handshake or a closed session ends the
                        // conversation; there is nothing more to ask.
                        let refused_hello = matches!(reply, Message::Error { .. })
                            && matches!(message, Message::Hello { .. });
                        let welcomed = matches!(reply, Message::Welcome { .. })
                            && matches!(message, Message::Hello { .. });
                        replies.push(reply);
                        if refused_hello {
                            break;
                        }
                        // The hook fires once, *after* the handshake is answered
                        // and before the first verb: "change this machine's state
                        // while the session is live". A session refused at Hello
                        // never gets here, so the hook cannot muddy that case.
                        if welcomed {
                            if let Some(hook) = between.take() {
                                // Deliberately blocking: the CLI is a child
                                // process, and the session must not be served
                                // while it runs.
                                hook();
                            }
                        }
                    }
                    None => break,
                }
            }
            replies
        })
    }

    /// This machine's trust rows from its audit log, as an operator reads them.
    fn trust_audit(&self) -> Vec<serde_json::Value> {
        let output = Command::new(binary("arreo"))
            .args(["audit", "--json", "--socket"])
            .arg(&self.socket)
            .env("ARREO_IDENTITY_DIR", &self.dir)
            .output()
            .expect("the CLI runs");
        assert!(
            output.status.success(),
            "audit --json failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let value: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("audit --json is JSON");
        value["rows"]
            .as_array()
            .expect("rows")
            .iter()
            .filter(|row| {
                row["action"]
                    .as_str()
                    .is_some_and(|action| action.starts_with("trust."))
            })
            .cloned()
            .collect()
    }

    /// Run a CLI command against this machine, expecting it to succeed.
    fn cli(&self, args: &[&str]) {
        let output = Command::new(binary("arreo"))
            .args(args)
            .arg("--socket")
            .arg(&self.socket)
            .env("ARREO_IDENTITY_DIR", &self.dir)
            .output()
            .expect("the CLI runs");
        assert!(
            output.status.success(),
            "`arreo {}` failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn text(message: &Message) -> String {
        match message {
            Message::Error { message, .. } => message.clone(),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }
}

/// Read one framed reply from an open channel, or `None` when the peer closed it.
async fn read_reply(
    channel: &mut arreo_core::transport::SecureChannel,
    buf: &mut Vec<u8>,
) -> Option<Message> {
    use tokio::io::AsyncReadExt;
    loop {
        if let Ok((reply, consumed)) = codec::decode_frame(buf) {
            buf.drain(..consumed);
            return Some(reply);
        }
        let mut chunk = [0u8; 8192];
        match channel.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(read) => buf.extend_from_slice(&chunk[..read]),
        }
    }
}

/// The ordinary path, which must keep working: a device this machine just
/// granted access to gets in.
///
/// This is the test that caught the first wiring attempt — pinned devices used
/// to be usable by dint of being pinned, and a gate that refuses them breaks
/// every operator who adds a device after the daemon started.
#[test]
fn a_device_issued_after_boot_can_use_the_machine() {
    let machine = Machine::start("issued");
    let key = DeviceKey::generate().expect("entropy");
    let id = DeviceId::from_key(&key.public());
    machine.issue_via_cli("phone", "owner", &key);

    let welcome = machine.hello_as(&key, &id);
    assert!(
        matches!(welcome, Message::Welcome { .. }),
        "a device this machine just issued must be able to connect: {welcome:?}"
    );
    // And the grant is a fact in the ledger, not an accident of the session.
    let granted = machine.ledger().devices().expect("rows");
    assert_eq!(granted.len(), 1, "{granted:?}");
    assert_eq!(granted[0].device, id);
    assert_eq!(granted[0].role, Role::Owner, "the issued role is the grant");
    assert!(granted[0].is_live());
}

/// **The write path's real safety property** (T-0059): the CLI changes trust while
/// a daemon is *running*, and the daemon acts on it without a restart.
///
/// This is what "one writer" was really about. There is one source of truth (the
/// machine's store) and the daemon reads it rather than caching a grant, so an
/// operator cutting and restoring access sees the effect immediately — and the
/// alternative design (a socket verb) would have needed a running daemon to
/// administer a machine whose daemon may be the very thing that is broken.
#[test]
fn the_cli_cuts_and_restores_a_running_daemons_access() {
    let machine = Machine::start("cli-live");
    let key = DeviceKey::generate().expect("entropy");
    let id = DeviceId::from_key(&key.public());
    machine.issue_via_cli("phone", "owner", &key);
    assert!(
        matches!(machine.hello_as(&key, &id), Message::Welcome { .. }),
        "it starts out trusted:\n{}",
        machine.log_text()
    );

    let name = arreo_core::mesh::default_machine_name();
    let machine_id = arreo_core::mesh::MachineId::from_key(&machine.root().public());
    // Cut this machine's grant, with the daemon still running.
    machine.cli(&["devices", "revoke", &id.display_id(), "--machine", &name]);
    let message = Machine::text(&machine.hello_as(&key, &id));
    assert!(
        message.contains("revoked") || message.contains("no grant"),
        "a grant cut while the daemon runs must take effect at once: {message}"
    );

    // Restore it with `machines trust` — the command the refusal just printed.
    machine.cli(&[
        "machines",
        "trust",
        &id.display_id(),
        "--role",
        "operator",
        "--yes",
    ]);
    assert!(
        matches!(machine.hello_as(&key, &id), Message::Welcome { .. }),
        "and a grant made while the daemon runs must take effect at once:\n{}",
        machine.log_text()
    );

    // The trail shows both changes, from the console, naming the machine.
    let rows = machine.trust_audit();
    let actions: Vec<&str> = rows
        .iter()
        .filter_map(|row| row["action"].as_str())
        .collect();
    assert_eq!(
        actions,
        // In the order an operator reads them (`arreo audit` prints oldest first,
        // which is the opposite of the store's newest-first API): the pairing
        // default, the cut, the refusal the cut caused, and the re-grant.
        vec!["trust.grant", "trust.revoke", "trust.refuse", "trust.grant"],
        "every change and the refusal between them: {rows:#?}"
    );
    for row in &rows {
        assert_eq!(row["kind"], serde_json::json!("trust"), "{row}");
        let expected = if row["action"] == serde_json::json!("trust.refuse") {
            serde_json::json!("refused")
        } else {
            serde_json::json!("ok")
        };
        assert_eq!(row["outcome"], expected, "{row}");
        assert_eq!(
            row["device"],
            serde_json::json!(id.display_id()),
            "the row is about the device it concerns: {row}"
        );
        assert_eq!(
            row["agent"],
            serde_json::json!(name),
            "the machine it acted on, by name, for a reader: {row}"
        );
        assert!(
            row["detail"]
                .as_str()
                .is_some_and(|detail| detail.contains(machine_id.as_str())),
            "and by id in the detail, so an exported row is unambiguous even \
             when the machine is renamed: {row}"
        );
    }
    // The role is recorded in the operator's word, not the certificate's.
    let regrant = rows.last().expect("the newest row");
    assert!(
        regrant["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("role=operator")),
        "{regrant}"
    );
}

/// A refusal is recorded **once per session**, however many verbs are refused: a
/// client that retries must not be able to fill the operator's log from outside,
/// which would be the cheapest denial of service against an audit trail.
///
/// The refusals here come from **this machine's grant**, not from the account's
/// role: the grant is cut while the session is live, and the client keeps asking.
/// A bare viewer asking to spawn is refused by the *certificate* gate instead —
/// a different fact with a different fix (re-pair), and not this task's row.
#[test]
fn a_refusal_is_recorded_once_per_session() {
    let machine = Machine::start("refusal-once");
    let key = DeviceKey::generate().expect("entropy");
    let id = DeviceId::from_key(&key.public());
    machine.issue_via_cli("phone", "owner", &key);
    let name = arreo_core::mesh::default_machine_name();

    // Read is something an owner may do; after the cut it is refused by the trust
    // gate, and these three attempts are one session.
    let read = Message::Read {
        v: VERSION,
        id: "pane-a".to_string(),
        from_line: 0,
    };
    let replies = machine.converse_interleaved(
        &key,
        &id,
        &[read.clone(), read.clone(), read.clone()],
        || {
            machine.cli(&["devices", "revoke", &id.display_id(), "--machine", &name]);
        },
    );
    assert!(
        matches!(replies.first(), Some(Message::Welcome { .. })),
        "the session opens while the grant is still live: {replies:?}"
    );
    let refusals = replies
        .iter()
        .filter(|reply| matches!(reply, Message::Error { .. }))
        .count();
    assert_eq!(
        refusals, 3,
        "every verb after the cut is refused: {replies:?}"
    );

    let rows = machine.trust_audit();
    let refusals: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["action"] == serde_json::json!("trust.refuse"))
        .collect();
    assert_eq!(
        refusals.len(),
        1,
        "three refusals in one session are one row, not three: {rows:#?}"
    );
    let refusal = refusals[0];
    assert_eq!(
        refusal["outcome"],
        serde_json::json!("refused"),
        "a refusal is not a success: {refusal}"
    );
    assert_eq!(
        refusal["device"],
        serde_json::json!(id.display_id()),
        "the row is about the device that was refused: {refusal}"
    );
    assert!(
        refusal["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("--role operator")
                || detail.contains("no grant")
                || detail.contains("revoked")),
        "and carries the reason the device was given: {}",
        refusal["detail"]
    );
}

/// **The criterion this task exists for.** A device this machine never granted
/// authenticates and is still refused — and the refusal tells the operator the
/// command that fixes it.
#[test]
fn a_pinned_device_with_no_grant_is_refused_with_the_command() {
    let machine = Machine::start("ungranted");
    let key = DeviceKey::generate().expect("entropy");
    let id = DeviceId::from_key(&key.public());
    // Pinned, not granted: the state an older build leaves behind, and the state
    // a phone paired to a *different* machine is in here.
    machine.pin_only("phone", Role::Owner, &key);
    assert!(
        machine.ledger().devices().expect("rows").is_empty(),
        "the pin must not have granted anything"
    );

    let refusal = machine.hello_as(&key, &id);
    let message = Machine::text(&refusal);
    // Printed as well as asserted, so the evidence transcript can quote what a
    // refused device is actually told (`cargo test … -- --nocapture`).
    eprintln!("refused session was told:\n  {message}");
    // The name is the one the daemon uses for itself (its hostname, by default):
    // the operator has to be able to find the machine the message is talking
    // about, so it is the same name `arreo machines list` would show.
    let machine_name = arreo_core::mesh::default_machine_name();
    assert!(
        message.contains(&machine_name),
        "the refusal names the machine ({machine_name}): {message}"
    );
    assert!(
        message.contains("no grant"),
        "and says what is missing: {message}"
    );
    assert!(
        message.contains("arreo machines trust"),
        "and gives the command that fixes it: {message}"
    );
    assert!(
        message.contains(&id.display_id()),
        "naming the device in the spelling the CLI takes back: {message}"
    );
    assert!(
        message.contains("--yes"),
        "including the confirmation the write verb needs: {message}"
    );
}

/// A role that is too low is refused with the role it needs — and observing
/// still works, because a refusal is per verb, not per session.
#[test]
fn a_viewer_is_refused_control_but_not_observation() {
    let machine = Machine::start("viewer");
    let key = DeviceKey::generate().expect("entropy");
    let id = DeviceId::from_key(&key.public());
    machine.issue_via_cli("phone", "viewer", &key);

    // Hello needs Observe, which a viewer has: the session opens.
    let welcome = machine.hello_as(&key, &id);
    assert!(
        matches!(welcome, Message::Welcome { .. }),
        "a viewer must be able to connect: {welcome:?}"
    );

    // The matrix is enforced by the ledger the daemon holds (the per-verb path
    // has its own unit tests); what this asserts is that the *daemon* is using
    // this machine's ledger for it.
    let ledger = machine.ledger();
    assert!(ledger
        .check(&id, arreo_core::identity::role::Verb::Read)
        .is_ok());
    let refusal = ledger
        .check(&id, arreo_core::identity::role::Verb::Spawn)
        .expect_err("a viewer may not spawn");
    let message = refusal.to_string();
    assert!(
        message.contains("--role operator"),
        "the role needed, in the word --role accepts: {message}"
    );
    assert!(message.contains("arreo machines trust"), "{message}");
}

/// A cut grant refuses the next session, and re-granting restores it without a
/// re-pairing — the device's certificate is untouched throughout, which is the
/// difference between "this machine no longer trusts you" and "you are no longer
/// in the account".
#[test]
fn a_revoked_grant_refuses_the_next_session_and_regranting_restores_it() {
    let machine = Machine::start("revoke");
    let key = DeviceKey::generate().expect("entropy");
    let id = DeviceId::from_key(&key.public());
    machine.issue_via_cli("phone", "owner", &key);
    assert!(matches!(
        machine.hello_as(&key, &id),
        Message::Welcome { .. }
    ));

    let ledger = machine.ledger();
    assert!(
        ledger.revoke(&id, 9_000).expect("revoke the grant"),
        "there was a live grant to cut"
    );
    let message = Machine::text(&machine.hello_as(&key, &id));
    assert!(
        message.contains("revoked"),
        "a cut grant must refuse the session and say it was deliberate: {message}"
    );
    assert!(
        message.contains("arreo machines trust"),
        "and say how to restore it: {message}"
    );

    // The device is still pinned: re-granting is all it takes.
    ledger
        .grant(&id, Role::Owner, &id, 10_000)
        .expect("re-grant");
    assert!(
        matches!(machine.hello_as(&key, &id), Message::Welcome { .. }),
        "a re-granted device connects again with the certificate it already had"
    );
}
