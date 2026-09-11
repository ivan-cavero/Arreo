//! T-0051 acceptance tests: two real daemons, one real relay, one message.
//!
//! Nothing is mocked. Two `arreo-server` processes run with their own identity
//! directories, their own sockets and their own configuration files; a real
//! `arreo-relay` serves them on loopback; and the message one machine sends the
//! other travels through the relay as ciphertext.
//!
//! The relay is exercised as a *binary* (never a linked library) so the AGPL
//! crate stays out of this crate's dependency graph (§7/T-0035).

use arreo_core::identity::{DeviceCert, DeviceId, DeviceKey, Role, RootKey, VerifyingKey};
use arreo_core::proto::{codec, Message, VERSION};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
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

/// Everything a test needs to talk about one machine.
struct Machine {
    dir: PathBuf,
    socket: PathBuf,
    device_id: DeviceId,
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
    fn log_text(&self) -> String {
        self.log.lock().expect("log").clone()
    }

    /// Wait until the daemon's socket answers, so a test never races its boot.
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

    /// Ask the daemon something over its own socket — the local path, which must
    /// keep working whatever the relay is doing.
    fn local_panes(&self) -> Vec<String> {
        let mut stream = std::os::unix::net::UnixStream::connect(&self.socket).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        send(&mut stream, &hello());
        assert!(
            matches!(recv(&mut stream), Message::Welcome { .. }),
            "the local socket must still speak the protocol"
        );
        send(
            &mut stream,
            &Message::Panes {
                v: VERSION,
                panes: vec![],
            },
        );
        match recv(&mut stream) {
            Message::Panes { panes, .. } => panes.into_iter().map(|pane| pane.id).collect(),
            other => panic!("expected a pane list, got {other:?}"),
        }
    }
}

fn hello() -> Message {
    Message::Hello {
        v: VERSION,
        client: "test".to_string(),
        wants: vec![VERSION],
    }
}

fn send(stream: &mut std::os::unix::net::UnixStream, message: &Message) {
    let frame = codec::encode_frame(message).expect("encode");
    stream.write_all(&frame).expect("write");
    stream.flush().expect("flush");
}

fn recv(stream: &mut std::os::unix::net::UnixStream) -> Message {
    let mut buf = Vec::new();
    loop {
        if let Ok((message, consumed)) = codec::decode_frame(&buf) {
            let _ = consumed;
            return message;
        }
        let mut chunk = [0u8; 8192];
        let read = stream.read(&mut chunk).expect("read");
        assert!(read > 0, "the daemon closed the socket");
        buf.extend_from_slice(&chunk[..read]);
    }
}

/// A machine's identity directory: the account root, its own device key and the
/// certificate the relay will authenticate it with.
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

/// Pin a peer's key the way an operator does, through the product's own door.
///
/// Using `arreo devices issue` rather than writing certificate files is
/// deliberate, but not because the gate would refuse a file: the authority's
/// index accepts a verifying certificate file with no store row, and the gate
/// goes through that same index (`DeviceAuthority::device`), so both doors agree
/// by construction. The reason is that a hand-written file is a state no product
/// command produces — it carries no serial bookkeeping and no durable record, so
/// revocation and retirement have nothing to name. A test should set up what the
/// product sets up.
fn pin_peer(socket: &Path, dir: &Path, name: &str, key: &VerifyingKey) {
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
        .env("ARREO_IDENTITY_DIR", dir)
        .output()
        .expect("the devices command runs");
    assert!(
        output.status.success(),
        "pinning {name} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write_config(path: &Path, relay_addr: SocketAddr, account: &str, peer: Option<&DeviceId>) {
    write_config_named(path, relay_addr, account, peer, None);
}

/// The same config, with a directory name (T-0056). `None` leaves the machine to
/// default to its hostname, which is what an operator who does not care gets.
fn write_config_named(
    path: &Path,
    relay_addr: SocketAddr,
    account: &str,
    peer: Option<&DeviceId>,
    name: Option<&str>,
) {
    let peer = peer.map_or(String::new(), |peer| {
        format!("peer = \"{}\"\n", peer.display_id())
    });
    let name = name.map_or(String::new(), |name| format!("name = \"{name}\"\n"));
    std::fs::write(
        path,
        format!(
            "[relay]\nenabled = true\naddr = \"{relay_addr}\"\naccount = \"{account}\"\n{peer}{name}"
        ),
    )
    .expect("config");
}

fn spawn_machine(tag: &str, dir: PathBuf, socket: PathBuf, config: Option<&Path>) -> Machine {
    let mut command = Command::new(binary("arreo-server"));
    command
        .arg("--socket")
        .arg(&socket)
        .env("ARREO_IDENTITY_DIR", &dir);
    if let Some(config) = config {
        command.arg("--config").arg(config);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the daemon starts");

    let stderr = child.stderr.take().expect("stderr");
    let log = Arc::new(Mutex::new(String::new()));
    {
        let log = Arc::clone(&log);
        std::thread::spawn(move || {
            // Keep reading for the process's whole life: dropping the pipe would
            // make the daemon die on its next log line (EPIPE), which reads like
            // an authentication failure.
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut held = log.lock().expect("log");
                held.push_str(&line);
                held.push('\n');
            }
        });
    }
    let _ = tag;
    Machine {
        dir,
        socket,
        device_id: DeviceId::parse("00000000000000000000000000000000").expect("placeholder"),
        child,
        log,
    }
}

/// A running relay, for the two-daemon tests.
struct Relay {
    child: Child,
    addr: SocketAddr,
    state_dir: PathBuf,
    log: Arc<Mutex<String>>,
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
            "arreo-relay-daemon-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&state_dir);
        std::fs::create_dir_all(&state_dir).expect("state dir");
        let mut child = Command::new(binary("arreo-relay"))
            .args([
                "serve",
                "--listen",
                "127.0.0.1:0",
                "--state-dir",
                &state_dir.display().to_string(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the relay starts");
        let stderr = child.stderr.take().expect("stderr");
        let log = Arc::new(Mutex::new(String::new()));
        let (ready_tx, ready_rx) = mpsc::channel();
        {
            let log = Arc::clone(&log);
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if let Some(rest) = line.split("router on ").nth(1) {
                        if let Some(addr) = rest.split_whitespace().next() {
                            if let Ok(addr) = addr.parse::<SocketAddr>() {
                                let _ = ready_tx.send(addr);
                            }
                        }
                    }
                    let mut held = log.lock().expect("log");
                    held.push_str(&line);
                    held.push('\n');
                }
            });
        }
        let addr = ready_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the relay announces its address");
        Self {
            child,
            addr,
            state_dir,
            log,
        }
    }

    fn log_text(&self) -> String {
        self.log.lock().expect("log").clone()
    }

    fn register_account(&self, account_id: &str, root: &VerifyingKey) {
        let output = Command::new(binary("arreo-relay"))
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

    fn state_bytes(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.state_dir).expect("state dir") {
            let path = entry.expect("entry").path();
            if path.is_file() {
                out.push((path.clone(), std::fs::read(&path).expect("read")));
            }
        }
        out
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// The headline criterion: two real daemons exchange a message through a real
/// relay, the relay sees only ciphertext, and both local sockets keep working.
#[test]
fn two_daemons_exchange_a_message_through_the_relay() {
    let root = RootKey::generate().expect("entropy");
    let account = "acct-1";
    let relay = Relay::start("exchange");
    relay.register_account(account, &root.public());

    // Two machines, each with its own identity directory, each pinning the
    // other's certificate — the pairing a real deployment does once.
    let base = std::env::temp_dir().join(format!("arreo-daemon-relay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (a_dir, b_dir) = (base.join("a"), base.join("b"));
    std::fs::create_dir_all(&a_dir).expect("scratch");
    std::fs::create_dir_all(&b_dir).expect("scratch");

    let a_key = DeviceKey::generate().expect("entropy");
    let b_key = DeviceKey::generate().expect("entropy");
    let a_cert = DeviceCert::issue(&root, &a_key.public(), "machine-a", Role::Owner, 1_000, 1);
    let b_cert = DeviceCert::issue(&root, &b_key.public(), "machine-b", Role::Owner, 1_000, 2);
    let a_id = a_cert.device().clone();
    let b_id = b_cert.device().clone();

    make_identity(&a_dir, &root, &a_key, &a_cert);
    make_identity(&b_dir, &root, &b_key, &b_cert);
    // Each machine pins the other, before the daemon starts so the store is
    // already there when the authority loads.
    pin_peer(&base.join("a.sock"), &a_dir, "machine-b", &b_key.public());
    pin_peer(&base.join("b.sock"), &b_dir, "machine-a", &a_key.public());

    // A probes B; B only serves — but B must still be *connected* to the relay,
    // because the relay only routes to a device that is live.
    let a_config = base.join("a.toml");
    let b_config = base.join("b.toml");
    write_config(&a_config, relay.addr, account, Some(&b_id));
    write_config(&b_config, relay.addr, account, None);

    let mut machine_a = spawn_machine("a", a_dir.clone(), base.join("a.sock"), Some(&a_config));
    let mut machine_b = spawn_machine("b", b_dir.clone(), base.join("b.sock"), Some(&b_config));
    machine_a.device_id = a_id.clone();
    machine_b.device_id = b_id.clone();
    machine_a.await_socket();
    machine_b.await_socket();

    // B has a pane whose id is the marker: it is what the probe's answer will
    // carry, so the relay must never see it.
    let marker = "ARREO-DAEMON-MARKER-4b91";
    {
        let mut stream =
            std::os::unix::net::UnixStream::connect(&machine_b.socket).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        send(&mut stream, &hello());
        assert!(matches!(recv(&mut stream), Message::Welcome { .. }));
        send(
            &mut stream,
            &Message::Spawn {
                v: VERSION,
                id: marker.to_string(),
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "sleep 30".to_string()],
                cols: 80,
                rows: 24,
                memory_max: None,
                pids_max: None,
                kill_on_breach: false,
            },
        );
        assert!(
            matches!(recv(&mut stream), Message::Ok { .. }),
            "pane spawned"
        );
    }

    // Wait for A's probe to report B's panes. That report *is* the message
    // exchange: A opened an encrypted session to B through the relay and asked
    // it a question.
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut answer = None;
    while Instant::now() < deadline {
        let log = machine_a.log_text();
        if let Some(line) = log
            .lines()
            .find(|line| line.contains("relay peer") && line.contains("reports"))
        {
            answer = Some(line.to_string());
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let answer = answer.unwrap_or_else(|| {
        panic!(
            "A never reported a peer answer.\nA's log:\n{}\nB's log:\n{}\nrelay log:\n{}",
            machine_a.log_text(),
            machine_b.log_text(),
            relay.log_text()
        )
    });
    assert!(
        answer.contains(&b_id.display_id()),
        "the answer must name the peer: {answer}"
    );
    assert!(
        answer.contains("1 pane"),
        "B's pane list must have reached A: {answer}"
    );
    // B authenticated A: the per-verb gate and the audit row need the identity.
    assert!(
        machine_b.log_text().contains("relay peer")
            && machine_b.log_text().contains("authenticated"),
        "B must log the peer it authenticated:\n{}",
        machine_b.log_text()
    );

    // The relay carried ciphertext: the marker is in neither its state nor its
    // logs, while both daemons know it.
    for (path, bytes) in relay.state_bytes() {
        assert!(
            !contains(&bytes, marker.as_bytes()),
            "the plaintext reached {} — the relay must not hold what it routes",
            path.display()
        );
    }
    let relay_log = relay.log_text();
    assert!(
        !relay_log.contains(marker),
        "the plaintext reached the relay's log"
    );
    assert!(
        relay_log.contains("authenticated"),
        "the relay logs its sessions, so the check above is not vacuous"
    );
    assert!(
        machine_b.local_panes().contains(&marker.to_string()),
        "B still serves its own socket"
    );
    assert!(
        machine_a.local_panes().is_empty(),
        "A still serves its own socket, and has no panes of its own"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// A relay that cannot be reached must not stop the local daemon, and the
/// reason must be visible rather than silent.
#[test]
fn a_relay_failure_leaves_the_local_daemon_serving() {
    let root = RootKey::generate().expect("entropy");
    let base = std::env::temp_dir().join(format!("arreo-daemon-norelay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let dir = base.join("a");
    std::fs::create_dir_all(&dir).expect("scratch");
    let key = DeviceKey::generate().expect("entropy");
    let cert = DeviceCert::issue(&root, &key.public(), "machine-a", Role::Owner, 1_000, 1);
    make_identity(&dir, &root, &key, &cert);

    // A configuration pointing at a port nothing is listening on.
    let config = base.join("a.toml");
    write_config(
        &config,
        "127.0.0.1:1".parse().expect("addr"),
        "acct-1",
        None,
    );

    let machine = spawn_machine("a", dir.clone(), base.join("a.sock"), Some(&config));
    machine.await_socket();
    // The local API works, and the relay failure is reported with the address.
    assert!(machine.local_panes().is_empty());
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut reported = false;
    while Instant::now() < deadline {
        if machine.log_text().contains("relay registration failed") {
            reported = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        reported,
        "a relay that cannot be reached must be reported:\n{}",
        machine.log_text()
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// With no configuration the daemon is exactly what it was: the relay is off,
/// and nothing about the local socket changes.
#[test]
fn no_configuration_means_no_relay() {
    let root = RootKey::generate().expect("entropy");
    let base = std::env::temp_dir().join(format!("arreo-daemon-noconf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let dir = base.join("a");
    std::fs::create_dir_all(&dir).expect("scratch");
    let key = DeviceKey::generate().expect("entropy");
    let cert = DeviceCert::issue(&root, &key.public(), "machine-a", Role::Owner, 1_000, 1);
    make_identity(&dir, &root, &key, &cert);

    let machine = spawn_machine("a", dir.clone(), base.join("a.sock"), None);
    machine.await_socket();
    assert!(machine.local_panes().is_empty());
    assert!(
        !machine.log_text().contains("relay"),
        "no configuration must mean no relay, and nothing said about one:\n{}",
        machine.log_text()
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// An incomplete `[relay]` section is a loud exit, not a silent no-op: an
/// operator who asked for the remote path must not quietly fail to get it.
#[test]
fn an_incomplete_relay_configuration_is_refused() {
    let base = std::env::temp_dir().join(format!("arreo-daemon-badconf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).expect("scratch");
    let config = base.join("bad.toml");
    std::fs::write(&config, "[relay]\nenabled = true\naccount = \"acct-1\"\n").expect("config");

    let output = Command::new(binary("arreo-server"))
        .args([
            "--socket",
            &base.join("a.sock").display().to_string(),
            "--config",
            &config.display().to_string(),
        ])
        .env("ARREO_IDENTITY_DIR", base.join("identity"))
        .output()
        .expect("the daemon runs");
    assert!(
        !output.status.success(),
        "an incomplete config must not start"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("addr"),
        "the refusal must name the missing field: {stderr}"
    );

    let _ = std::fs::remove_dir_all(&base);
}

/// The daemon asserts its directory row on connect (T-0056).
///
/// A real daemon, a real relay, and the row read back over the wire by another
/// device in the account: the criterion is that a machine is listed by the relay
/// *because the machine said so*, so the proof has to be the relay's answer and
/// not the daemon's log line.
#[tokio::test]
async fn a_daemon_registers_itself_in_the_account_directory() {
    let root = RootKey::generate().expect("entropy");
    let base = std::env::temp_dir().join(format!("arreo-daemon-dir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let dir = base.join("a");
    std::fs::create_dir_all(&dir).expect("scratch");
    let key = DeviceKey::generate().expect("entropy");
    let cert = DeviceCert::issue(&root, &key.public(), "machine-a", Role::Owner, 1_000, 1);
    make_identity(&dir, &root, &key, &cert);

    let relay = Relay::start("directory");
    relay.register_account("acct-1", &root.public());
    let config = base.join("a.toml");
    write_config_named(&config, relay.addr, "acct-1", None, Some("the-workbox"));

    let machine = spawn_machine("a", dir.clone(), base.join("a.sock"), Some(&config));
    machine.await_socket();

    // Read the directory as a *different* device in the account: a peer that
    // reads the directory is exactly the use case, and using the daemon's own
    // identity would replace its session (the relay keeps one per device).
    let reader_key = DeviceKey::generate().expect("entropy");
    let reader_cert = DeviceCert::issue(
        &root,
        &reader_key.public(),
        "reader",
        Role::Viewer,
        1_000,
        9,
    );
    let reader = arreo_core::relay::session::RelaySession::dial(
        relay.addr,
        "acct-1",
        &reader_key,
        &reader_cert,
    )
    .await
    .expect("the reader registers");

    let mut listed = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        listed = reader
            .machines(false)
            .await
            .expect("the relay answers")
            .machines;
        if !listed.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(
        listed.len(),
        1,
        "the daemon's row must be in the directory: {listed:?}; its log was:\n{}",
        machine.log_text()
    );
    assert_eq!(listed[0].name.as_str(), "the-workbox");
    assert_eq!(
        listed[0].machine_id,
        arreo_core::mesh::MachineId::from_key(&root.public()),
        "the row is keyed by the machine's root key — the key a device pinned when it paired"
    );
    // The daemon says what it was granted, so an operator does not have to read
    // the relay to find out what this machine is called. Polled rather than read
    // once: the log arrives on the daemon's stderr and the reader thread that
    // collects it is a step behind the directory write this test just observed,
    // so a single read is a race the row assertion above cannot see.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !machine.log_text().contains("the-workbox") {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        machine.log_text().contains("the-workbox"),
        "the daemon must log the granted name; its log was:\n{}",
        machine.log_text()
    );

    drop(reader);
    let _ = std::fs::remove_dir_all(&base);
}

/// A relay that refuses the directory write does not stop the daemon serving
/// locally: remote reach is not a prerequisite for local work.
#[tokio::test]
async fn a_refused_directory_write_leaves_the_daemon_serving() {
    let root = RootKey::generate().expect("entropy");
    let base = std::env::temp_dir().join(format!("arreo-daemon-nodir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let dir = base.join("a");
    std::fs::create_dir_all(&dir).expect("scratch");
    let key = DeviceKey::generate().expect("entropy");
    let cert = DeviceCert::issue(&root, &key.public(), "machine-a", Role::Owner, 1_000, 1);
    make_identity(&dir, &root, &key, &cert);

    // The account is the daemon's own (authentication succeeds), but the name it
    // asks for is not a name the directory's rule accepts (`!` is not lowercase,
    // a digit or a hyphen). The write is refused; the daemon must keep serving.
    let relay = Relay::start("refused-dir");
    relay.register_account("acct-1", &root.public());
    let config = base.join("a.toml");
    write_config_named(&config, relay.addr, "acct-1", None, Some("workbox!"));

    let machine = spawn_machine("a", dir.clone(), base.join("a.sock"), Some(&config));
    machine.await_socket();

    // Local work still works: the socket answers panes.
    assert!(machine.local_panes().is_empty());
    let wait = Instant::now() + Duration::from_secs(10);
    while Instant::now() < wait && !machine.log_text().contains("would not register") {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        machine.log_text().contains("would not register"),
        "a refused directory write must be said out loud; the log was:\n{}",
        machine.log_text()
    );

    let _ = std::fs::remove_dir_all(&base);
}
