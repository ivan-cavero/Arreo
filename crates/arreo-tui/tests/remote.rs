//! T-0032: the TUI's client against a daemon on another machine, through a real
//! relay.
//!
//! Nothing here is mocked. A real `arreo-relay` process serves a real QUIC
//! listener; a real `arreo-server` is connected to it and owns real panes; the
//! client under test is `arreo_tui::client::Client`, the same type the TUI
//! binary uses, speaking the same `Message` frames it speaks to a Unix socket.
//!
//! `arreo-relay` and `arreo-server` are spawned as *processes* rather than
//! linked, because the dependency rule forbids `arreo-tui` from depending on
//! either (AGENTS.md): the point of T-0032 is that the remote path is the same
//! client over a different transport, not that the TUI learns what a relay is.
//!
//! Run `cargo test --workspace` (which builds every binary) before this file, or
//! `cargo build -p arreo-relay -p arreo-server`: `cargo test -p arreo-tui` alone
//! does not build another package's binary.

use arreo_core::identity::{DeviceCert, DeviceId, DeviceKey, Role, RootKey};
use arreo_core::proto::{codec, Message, VERSION};
use arreo_tui::client::{Client, Target};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// A binary from the workspace, built by `cargo test --workspace`.
fn binary(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary path");
    path.pop(); // deps/
    path.pop(); // debug/
    path.push(name);
    assert!(
        path.exists(),
        "{} is missing — run `cargo test --workspace` (which builds every binary) first",
        path.display()
    );
    path
}

/// A process whose stderr is drained for its whole life.
///
/// The drain is not tidiness: a closed pipe makes the child die on its next log
/// line (EPIPE), which looks exactly like an authentication failure.
struct Process {
    child: Child,
    log: Arc<Mutex<String>>,
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Process {
    fn spawn(mut command: Command, watch: Option<&'static str>) -> (Self, Option<SocketAddr>) {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().expect("the process starts");
        let stderr = child.stderr.take().expect("stderr");
        let log = Arc::new(Mutex::new(String::new()));
        let (ready_tx, ready_rx) = mpsc::channel();
        {
            let log = Arc::clone(&log);
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if let Some(watch) = watch {
                        if let Some(rest) = line.split(watch).nth(1) {
                            if let Some(addr) = rest.split_whitespace().next() {
                                if let Ok(addr) = addr.parse::<SocketAddr>() {
                                    let _ = ready_tx.send(addr);
                                }
                            }
                        }
                    }
                    let mut held = log.lock().expect("log");
                    held.push_str(&line);
                    held.push('\n');
                }
            });
        }
        let addr = watch.map(|_| {
            ready_rx
                .recv_timeout(Duration::from_secs(20))
                .expect("the process announces its address")
        });
        (Self { child, log }, addr)
    }

    fn log_text(&self) -> String {
        self.log.lock().expect("log").clone()
    }

    /// Wait until the log contains `needle` — for a daemon that is up but does
    /// not print an address to parse.
    fn await_log(&self, needle: &str, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if self.log_text().contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("{what} never appeared.\nlog:\n{}", self.log_text());
    }
}

/// A relay, an account root, and a machine (the "peer") with panes.
struct Fixture {
    /// Held for their lives: dropping either ends the route this test exercises.
    #[allow(dead_code)]
    relay: Process,
    #[allow(dead_code)]
    machine: Process,
    relay_addr: SocketAddr,
    account: String,
    /// The peer machine's device id — what the client attaches to.
    peer: DeviceId,
    /// The peer's key, which the client pins as its server key.
    peer_key: DeviceKey,
    /// The account root, so a test can issue another device its certificate the
    /// way the product does (a role is a property of the certificate).
    root: RootKey,
    base: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

impl Fixture {
    fn start(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "arreo-tui-remote-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("scratch");

        let state_dir = base.join("relay");
        std::fs::create_dir_all(&state_dir).expect("relay state");
        let mut relay_cmd = Command::new(binary("arreo-relay"));
        relay_cmd.args([
            "serve",
            "--listen",
            "127.0.0.1:0",
            "--state-dir",
            &state_dir.display().to_string(),
        ]);
        let (relay, relay_addr) = Process::spawn(relay_cmd, Some("router on "));
        let relay_addr = relay_addr.expect("the relay publishes its address");

        let root = RootKey::generate().expect("entropy");
        let account = "acct-tui".to_string();
        let output = Command::new(binary("arreo-relay"))
            .args([
                "account",
                "add",
                "--state-dir",
                &state_dir.display().to_string(),
                "--account",
                &account,
                "--root-key",
                &hex(root.public().to_bytes()),
            ])
            .output()
            .expect("the account command runs");
        assert!(
            output.status.success(),
            "registering the account failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // The machine's identity: its own key and certificate, plus the account
        // root (which issues certificates on this machine in the real product).
        let peer_key = DeviceKey::generate().expect("entropy");
        let peer_cert = DeviceCert::issue(&root, &peer_key.public(), "peer", Role::Owner, 1_000, 1);
        let peer = peer_cert.device().clone();
        let peer_dir = base.join("peer");
        std::fs::create_dir_all(peer_dir.join("identity").join("devices")).expect("identity");
        root.save(&peer_dir.join("identity").join("root.key"))
            .expect("root key");
        peer_key
            .save(&peer_dir.join("identity").join("device.key"))
            .expect("device key");
        peer_cert
            .save(&peer_dir.join("identity").join("devices"))
            .expect("certificate");

        let socket = peer_dir.join("peer.sock");
        // The peer must have the client pinned before the daemon starts, so the
        // authority's index already carries it when the handshake resolves it.
        let client_key_path = base.join("client").join("identity").join("device.key");
        std::fs::create_dir_all(client_key_path.parent().expect("parent")).expect("client dir");
        let client_key = DeviceKey::generate().expect("entropy");
        client_key.save(&client_key_path).expect("client key");
        pin(&socket, &peer_dir, "tui", &client_key.public(), Role::Owner);

        let config = base.join("peer.toml");
        std::fs::write(
            &config,
            format!("[relay]\nenabled = true\naddr = \"{relay_addr}\"\naccount = \"{account}\"\n"),
        )
        .expect("config");
        let mut machine_cmd = Command::new(binary("arreo-server"));
        machine_cmd
            .arg("--socket")
            .arg(&socket)
            .arg("--config")
            .arg(&config)
            .env("ARREO_IDENTITY_DIR", &peer_dir);
        let (machine, _) = Process::spawn(machine_cmd, None);
        machine.await_log("serving on", "the peer daemon's socket");
        // The session to the relay is a background task; without it the relay has
        // nobody to route to and the client's stream would go nowhere.
        machine.await_log("relay session up", "the peer's relay session");

        // The client's certificate and the server key it pinned: what
        // `arreo pair --join` writes on the joining side.
        let client_dir = base.join("client").join("identity");
        let client_id = DeviceId::from_key(&client_key.public());
        let client_cert =
            DeviceCert::issue(&root, &client_key.public(), "tui", Role::Owner, 1_000, 2);
        assert_eq!(client_cert.device(), &client_id);
        client_cert.save(&client_dir.join("devices")).expect("cert");
        std::fs::write(
            client_dir.join("server.key"),
            format!("{}\n", hex(peer_key.public().to_bytes())),
        )
        .expect("server key");

        Self {
            relay,
            machine,
            relay_addr,
            account,
            peer,
            peer_key,
            root,
            base,
        }
    }

    /// The client's identity directory (what `--identity` names).
    fn client_dir(&self) -> PathBuf {
        self.base.join("client").join("identity")
    }

    /// A second client identity, pinned on the peer with `role`.
    ///
    /// The peer's daemon is already running, which is the ordinary case for an
    /// operator granting access later — and the reason `DeviceAuthority::device`
    /// reloads on a miss (T-0033).
    fn viewer(&self, role: Role) -> (DeviceId, PathBuf) {
        let key = DeviceKey::generate().expect("entropy");
        let cert = DeviceCert::issue(&self.root, &key.public(), "viewer", role, 1_000, 9);
        let dir = self.base.join(format!("viewer-{}", cert.device().as_str()));
        std::fs::create_dir_all(dir.join("devices")).expect("viewer dir");
        key.save(&dir.join("device.key")).expect("viewer key");
        cert.save(&dir.join("devices")).expect("viewer certificate");
        std::fs::write(
            dir.join("server.key"),
            format!("{}\n", hex(self.peer_key.public().to_bytes())),
        )
        .expect("pinned server key");
        pin(
            &self.peer_socket(),
            &self.base.join("peer"),
            "viewer",
            &key.public(),
            role,
        );
        (cert.device().clone(), dir)
    }

    /// A target pointing at the peer, as `identity`.
    fn target_for(&self, identity: &Path) -> Target {
        Target::remote(
            self.relay_addr,
            &self.account,
            &self.peer.display_id(),
            identity,
        )
        .expect("the identity is complete")
    }

    /// The peer's local socket, for the parity control run.
    fn peer_socket(&self) -> PathBuf {
        self.base.join("peer").join("peer.sock")
    }

    /// A target pointing at the peer through the relay.
    fn target(&self) -> Target {
        Target::remote(
            self.relay_addr,
            &self.account,
            &self.peer.display_id(),
            &self.client_dir(),
        )
        .expect("the client identity is complete")
    }

    /// Spawn a pane on the peer through its own socket, and return its id.
    fn pane(&self, id: &str, script: &str) -> String {
        let mut stream =
            std::os::unix::net::UnixStream::connect(self.peer_socket()).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        send(&mut stream, &hello());
        assert!(matches!(recv(&mut stream), Message::Welcome { .. }));
        send(
            &mut stream,
            &Message::Spawn {
                v: VERSION,
                id: id.to_string(),
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), script.to_string()],
                cols: 80,
                rows: 24,
                memory_max: None,
                pids_max: None,
                kill_on_breach: false,
            },
        );
        match recv(&mut stream) {
            Message::Ok { .. } => id.to_string(),
            other => panic!("spawn failed: {other:?}"),
        }
    }
}

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Pin a device on a machine's store, through the product's own door.
///
/// The role is the *store's* answer to "what may this device do" — the
/// certificate carries one too, but the authority decides through the store row,
/// so a test that wants a viewer must pin one (pinning an owner and issuing a
/// viewer certificate would test the wrong thing).
fn pin(
    socket: &Path,
    identity_dir: &Path,
    name: &str,
    key: &arreo_core::identity::VerifyingKey,
    role: Role,
) {
    let output = Command::new(binary("arreo"))
        .args([
            "devices",
            "issue",
            "--socket",
            &socket.display().to_string(),
            "--name",
            name,
            "--role",
            role.as_str(),
            "--key",
            &hex(key.to_bytes()),
        ])
        .env("ARREO_IDENTITY_DIR", identity_dir)
        .output()
        .expect("the devices command runs");
    assert!(
        output.status.success(),
        "pinning {name} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
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
        if let Ok((message, _)) = codec::decode_frame(&buf) {
            return message;
        }
        let mut chunk = [0u8; 8192];
        let read = stream.read(&mut chunk).expect("read");
        assert!(read > 0, "the daemon closed the socket");
        buf.extend_from_slice(&chunk[..read]);
    }
}

/// Read one pane's lines from `from_line`, over whatever `conn` is.
async fn read_lines(conn: &mut Client, id: &str, from_line: usize) -> Vec<String> {
    match conn
        .call(&Message::Read {
            v: VERSION,
            id: id.to_string(),
            from_line,
        })
        .await
        .expect("read")
    {
        Message::Delta { lines, .. } => lines,
        other => panic!("read {id} from {from_line}: {other:?}"),
    }
}

/// The headline criterion: the same client, the same verbs, over the relay.
#[tokio::test]
async fn the_client_reaches_a_remote_daemon_and_sees_the_same_pane() {
    let fixture = Fixture::start("parity");
    fixture.pane("remote-pane", "printf 'alpha\\nbeta\\ngamma\\n'; sleep 30");

    // Let the pane produce its output before either client reads.
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Remote: the client dials the relay as this device, opens a stream to the
    // peer, and completes the Noise handshake with the peer's pinned key.
    let mut remote = Client::connect_to(&fixture.target())
        .await
        .expect("the remote session opens");

    let panes = match remote
        .call(&Message::Panes {
            v: VERSION,
            panes: vec![],
        })
        .await
        .expect("panes")
    {
        Message::Panes { panes, .. } => panes,
        other => panic!("panes: {other:?}"),
    };
    assert!(
        panes.iter().any(|pane| pane.id == "remote-pane"),
        "the remote machine's pane is listed: {panes:?}"
    );

    // Parity on the wire: the same frame sequence against the peer's own socket
    // must decode to the same lines for the same pane.
    let mut local = Client::connect(&fixture.peer_socket())
        .await
        .expect("the local session opens");
    let local_panes = match local
        .call(&Message::Panes {
            v: VERSION,
            panes: vec![],
        })
        .await
        .expect("panes")
    {
        Message::Panes { panes, .. } => panes,
        other => panic!("panes: {other:?}"),
    };
    assert_eq!(
        panes.len(),
        local_panes.len(),
        "the same daemon lists the same panes over either transport"
    );

    let over_relay = read_lines(&mut remote, "remote-pane", 0).await;
    let over_socket = read_lines(&mut local, "remote-pane", 0).await;
    assert_eq!(
        over_relay, over_socket,
        "the same pane decodes identically over the relay and over the socket"
    );
    // And the content is the pane's, not an empty success.
    let joined = over_relay.join("\n");
    assert!(
        joined.contains("alpha") && joined.contains("gamma"),
        "the pane's output crossed the relay: {over_relay:?}"
    );

    // A read is not a write: the peer must not have logged a keystroke, and the
    // session itself must be on the peer's record. Read as the operator's own
    // table rather than as JSON — this crate has no JSON dependency and does not
    // need one to see an action name.
    let audit = Command::new(binary("arreo"))
        .args(["audit", "--limit", "50", "--socket"])
        .arg(fixture.peer_socket())
        .env("ARREO_IDENTITY_DIR", fixture.base.join("peer"))
        .output()
        .expect("audit runs");
    let audit = String::from_utf8_lossy(&audit.stdout).to_string();
    assert!(
        audit.contains("session.connect"),
        "the remote session is on the peer's record:\n{audit}"
    );
    // Reads are deliberately not audited (a trail that logs every poll is a
    // trail nobody reads, T-0033), so the only rows this test leaves are the
    // session's own. The absence of a keystroke is the assertion below.
    let writes: Vec<&str> = audit
        .lines()
        .filter(|line| line.contains(" send "))
        .collect();
    assert!(
        writes.is_empty(),
        "no keystroke was sent, so no send row may exist: {writes:?}"
    );
}

/// Resume semantics: reading from the cursor replays an uninterrupted
/// transcript exactly, and the lines that arrived in between are delivered.
///
/// This is the property the acceptance criterion is about — "no duplicated line,
/// no gap" — proven on the wire rather than by screenshot, and proven the way the
/// TUI actually consumes it: one long-lived session, incremental `Read{from_line}`
/// per pane, the cursor owned by the UI.
#[tokio::test]
async fn resume_from_the_cursor_replays_without_duplication_or_gaps() {
    let fixture = Fixture::start("resume");
    // A pane that keeps talking, so there is something to lose between reads.
    let pane = fixture.pane(
        "chatty",
        "i=0; while [ $i -lt 400 ]; do echo line-$i; i=$((i+1)); sleep 0.05; done; sleep 60",
    );
    tokio::time::sleep(Duration::from_millis(800)).await;

    let mut conn = Client::connect_to(&fixture.target())
        .await
        .expect("the session opens");

    // The control: everything the pane has produced so far, from the beginning.
    let control = read_lines(&mut conn, &pane, 0).await;
    assert!(
        control.len() >= 5,
        "the pane must have produced something: {control:?}"
    );
    let cursor = control.len();

    // Let the pane talk while nobody is reading — the gap a drop would create.
    tokio::time::sleep(Duration::from_millis(400)).await;

    // Resume from the cursor: exactly what an interrupted client does.
    let rest = read_lines(&mut conn, &pane, cursor).await;
    assert!(
        !rest.is_empty(),
        "the resumed read must deliver the lines that arrived while nobody read"
    );

    // 1. Nothing was duplicated: the cursor is what prevents it.
    let mut transcript = control.clone();
    transcript.extend(rest.iter().cloned());
    let mut seen = std::collections::HashSet::new();
    for line in &transcript {
        assert!(
            seen.insert(line.clone()),
            "line {line:?} was delivered twice: {transcript:?}"
        );
    }
    // 2. Nothing was skipped: a fresh read of the same span sees the same lines,
    //    in the same order, so the resume continued the transcript rather than
    //    starting a second one.
    let whole = read_lines(&mut conn, &pane, 0).await;
    let overlap = transcript.len().min(whole.len());
    assert_eq!(
        transcript[..overlap],
        whole[..overlap],
        "the resumed transcript must match a single uninterrupted read, line for line"
    );
    // 3. And the resumed read carried the pane past where the cursor was — a
    //    resumed read that returned nothing would satisfy (1) and (2) trivially.
    assert!(
        transcript.len() > cursor,
        "the resumed read must extend the transcript: cursor={cursor} transcript={}",
        transcript.len()
    );
}

/// A drop is a reconnect, not a lost session: after the client's connection dies,
/// a *fresh* session to the same peer is accepted promptly, and the transcript it
/// builds from the UI's cursor is byte-identical to a run that never dropped.
///
/// This is the case T-0032 could not pass on its own, and the reason T-0054
/// exists. The client's connection is killed the way a real one dies — the QUIC
/// endpoint closes, so the relay sees the device leave — and the reconnect must
/// then be accepted *without* waiting for the far end to notice a dead stream.
/// Before the departure notice, the far end kept the dead stream and handed the
/// new handshake to it; the measurement in T-0032's evidence is that reconnect
/// taking more than 60 s. Now the relay tells the peer the device left, the peer
/// ends that stream, and the next connection is served by a fresh one.
#[tokio::test]
async fn a_dropped_connection_reconnects_promptly_and_resumes_exactly() {
    let fixture = Fixture::start("drop");
    let pane = fixture.pane(
        "chatty",
        "i=0; while [ $i -lt 400 ]; do echo line-$i; i=$((i+1)); sleep 0.05; done; sleep 60",
    );
    tokio::time::sleep(Duration::from_millis(800)).await;

    // The control: read everything so far, then close cleanly.
    let control = {
        let mut conn = Client::connect_to(&fixture.target())
            .await
            .expect("the control session opens");
        let lines = read_lines(&mut conn, &pane, 0).await;
        conn.close().await;
        lines
    };
    assert!(
        control.len() >= 5,
        "the pane must have produced something: {control:?}"
    );

    // The session under test: read, then *kill* it the way a crash does.
    let (first, cursor) = {
        let mut conn = Client::connect_to(&fixture.target())
            .await
            .expect("the session opens");
        let lines = read_lines(&mut conn, &pane, 0).await;
        assert!(!lines.is_empty(), "the first read returned nothing");
        let cursor = lines.len();
        // Abrupt: the relay sees the connection go away without a goodbye, which
        // is what makes this the drop case rather than a tidy close.
        conn.kill();
        (lines, cursor)
    };

    // The reconnect must be accepted promptly. The deadline is deliberately far
    // below the >60 s that T-0032 measured without the departure notice, and
    // above the relay's own connect budget, so a regression to "wait for the far
    // end to give up" fails here instead of merely being slow.
    let started = Instant::now();
    let mut resumed = Client::connect_to(&fixture.target())
        .await
        .unwrap_or_else(|e| {
            panic!(
                "the reconnect was not accepted: {e}\nrelay log:\n{}\npeer log:\n{}",
                fixture.relay.log_text(),
                fixture.machine.log_text()
            )
        });
    let reconnect_ms = started.elapsed().as_millis() as u64;
    assert!(
        reconnect_ms < 10_000,
        "the reconnect took {reconnect_ms} ms; a peer that is told its peer left must \
         accept the next connection promptly"
    );

    // The lines printed while nothing was connected are delivered, and the
    // transcript is exactly what an uninterrupted read would have produced.
    let rest = read_lines(&mut resumed, &pane, cursor).await;
    let mut transcript = first.clone();
    transcript.extend(rest.iter().cloned());
    let mut seen = std::collections::HashSet::new();
    for line in &transcript {
        assert!(
            seen.insert(line.clone()),
            "line {line:?} was delivered twice after the reconnect: {transcript:?}"
        );
    }
    let overlap = control.len().min(transcript.len());
    assert_eq!(
        transcript[..overlap],
        control[..overlap],
        "the reconnected transcript must match an uninterrupted read, line for line"
    );
    assert!(
        transcript.len() > cursor,
        "the reconnected read must deliver what arrived during the drop: \
         cursor={cursor} transcript={}",
        transcript.len()
    );
    println!("drop: reconnected in {reconnect_ms} ms and resumed from line {cursor}");
}
/// A device without operator permission gets a typed error and no keystroke —
/// over the relay, where the decision is the peer daemon's and not the relay's.
///
/// This is the other half of "remote input is attributable": attribution says
/// *who* acted, authorization says whether they may. Both are decided on the
/// machine that owns the pane, which is what makes a relay-routed session no
/// more privileged than a local one.
#[tokio::test]
async fn a_viewer_is_refused_with_a_typed_error_and_no_keystroke() {
    let fixture = Fixture::start("viewer");
    let pane = fixture.pane("guarded", "printf 'guarded-output\\n'; sleep 60");
    tokio::time::sleep(Duration::from_millis(400)).await;

    let (viewer_id, viewer_dir) = fixture.viewer(Role::Viewer);
    let mut viewer = Client::connect_to(&fixture.target_for(&viewer_dir))
        .await
        .expect("a viewer may connect: the refusal is per verb, not per session");

    // A read is allowed: a viewer is meant to see.
    let lines = read_lines(&mut viewer, &pane, 0).await;
    assert!(
        lines.join("\n").contains("guarded-output"),
        "a viewer may read the pane: {lines:?}"
    );

    // A write is refused, and the refusal is *typed* — an error the caller can
    // act on, not a dropped message and not a silent success.
    let reply = viewer
        .call(&Message::Send {
            v: VERSION,
            id: pane.clone(),
            data: "echo not-allowed\n".to_string(),
        })
        .await
        .expect("the session answers");
    match reply {
        Message::Error { message, .. } => assert!(
            !message.trim().is_empty(),
            "the refusal must say why: {message:?}"
        ),
        other => panic!("a viewer's send must be refused with a typed error, got {other:?}"),
    }

    // And no keystroke reached the pane: the marker the refused send would have
    // echoed is not in the pane's scrollback.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let after = read_lines(&mut viewer, &pane, 0).await;
    assert!(
        !after.join("\n").contains("not-allowed"),
        "the refused send must not have reached the pane: {after:?}"
    );

    // The peer's own record shows the session and the refusal, and no send.
    let audit = Command::new(binary("arreo"))
        .args(["audit", "--limit", "50", "--socket"])
        .arg(fixture.peer_socket())
        .env("ARREO_IDENTITY_DIR", fixture.base.join("peer"))
        .output()
        .expect("audit runs");
    let audit = String::from_utf8_lossy(&audit.stdout).to_string();
    assert!(
        audit.contains(&viewer_id.display_id()),
        "the viewer's session is attributed to its device: {audit}"
    );
    let sends: Vec<&str> = audit.lines().filter(|l| l.contains(" send ")).collect();
    assert!(
        sends.is_empty(),
        "a refused send is not a send that happened: {sends:?}"
    );
}
