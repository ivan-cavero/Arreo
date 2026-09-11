//! T-0033 acceptance tests: the audit trail the *daemon* writes.
//!
//! The core test file (`crates/arreo-core/tests/audit.rs`) covers the store: the
//! schema, redaction, ordering, export and prune. This one covers the half only a
//! real daemon can show — that the actions a client takes are recorded with the
//! acting identity and the right outcome, in the order they happened, and that
//! the operator can read them back with the CLI.
//!
//! **Run these with `cargo test --workspace`** (or build both crates first):
//! `cargo test -p arreo-server` does not build the `arreo` binary these tests
//! spawn, so a CLI change would be silently tested against a stale binary. That
//! trap has cost this repo two debugging cycles already.

use arreo_core::proto::{codec, Message, VERSION};
use std::io::{BufRead, BufReader, Read, Write};
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

/// A real daemon on its own socket and identity directory.
struct Daemon {
    dir: PathBuf,
    socket: PathBuf,
    child: Child,
    log: Arc<Mutex<String>>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Daemon {
    fn start(tag: &str) -> Self {
        Self::spawn(tag, false)
    }

    /// A daemon that also opens the loopback transport seam (T-0023), so a test
    /// can drive a real remote session against it.
    fn start_with_relay_seam(tag: &str) -> Self {
        Self::spawn(tag, true)
    }

    fn spawn(tag: &str, seam: bool) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "arreo-audit-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("identity")).expect("scratch");
        let socket = dir.join("arreo.sock");
        let mut command = Command::new(binary("arreo-server"));
        command
            .arg("--socket")
            .arg(&socket)
            .env("ARREO_IDENTITY_DIR", &dir);
        if seam {
            command.env("ARREO_TRANSPORT_TEST_LISTEN", "127.0.0.1:0");
        }
        let mut child = command
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
                // kill the daemon on its next log line (EPIPE).
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let mut held = sink.lock().expect("log");
                    held.push_str(&line);
                    held.push('\n');
                }
            });
        }
        let daemon = Self {
            dir,
            socket,
            child,
            log,
        };
        daemon.await_socket();
        daemon
    }

    fn await_socket(&self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&self.socket).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the daemon never served {}", self.socket.display());
    }

    /// One client session: connect, run `steps`, and close.
    ///
    /// Each step is `(send, expect_reply)`; the caller decides what it expects so
    /// a step that fails says which one.
    fn session(&self, steps: &[Step]) {
        let mut stream = std::os::unix::net::UnixStream::connect(&self.socket).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        send(&mut stream, &hello());
        assert!(matches!(recv(&mut stream), Message::Welcome { .. }));
        for (message, ok) in steps {
            send(&mut stream, message);
            let reply = recv(&mut stream);
            assert!(ok(&reply), "unexpected reply to {message:?}: {reply:?}");
        }
    }

    /// Attach to `id`, read the first frame, and let the stream go.
    fn attach_once(&self, id: &str) {
        let mut stream = std::os::unix::net::UnixStream::connect(&self.socket).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("timeout");
        send(&mut stream, &hello());
        assert!(matches!(recv(&mut stream), Message::Welcome { .. }));
        send(
            &mut stream,
            &Message::Attach {
                v: VERSION,
                id: id.to_string(),
                from_line: 0,
            },
        );
        let first = recv(&mut stream);
        assert!(
            matches!(first, Message::Delta { .. } | Message::Snapshot { .. }),
            "attach must open with a frame: {first:?}"
        );
    }

    /// `arreo audit --json` against this daemon.
    fn audit_json(&self) -> serde_json::Value {
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
        serde_json::from_slice(&output.stdout).expect("audit --json is JSON")
    }

    fn log_text(&self) -> String {
        self.log.lock().expect("log").clone()
    }

    /// Read the audit log, waiting for `action` to appear (the daemon writes some
    /// rows asynchronously, e.g. the disconnect after a connection closes).
    fn await_row(&self, action: &str) -> serde_json::Value {
        // Generous but bounded: with the pump aborted on drop, the daemon
        // notices within milliseconds; the allowance is for a loaded box, not
        // for the 15 s idle timeout this used to wait for.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let audit = self.audit_json();
            if audit["rows"].as_array().is_some_and(|rows| {
                rows.iter()
                    .any(|row| row["action"] == serde_json::json!(action))
            }) {
                return audit;
            }
            if Instant::now() > deadline {
                panic!(
                    "{action} never appeared in the audit log: {audit}\n\
                     daemon log:\n{}",
                    self.log_text()
                );
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The loopback transport's bound address, once the daemon has announced it.
    fn remote_addr(&self) -> Option<std::net::SocketAddr> {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if let Some(addr) = self
                .log_text()
                .lines()
                .find_map(|line| line.split("listening on ").nth(1))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|addr| addr.parse().ok())
            {
                return Some(addr);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }
}

/// One step of a scripted session: what to send, and what counts as the right
/// answer. Named so the tuple does not have to be spelled out at every call.
type Step = (Message, fn(&Message) -> bool);

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

fn is_ok(reply: &Message) -> bool {
    matches!(reply, Message::Ok { .. })
}

fn is_panes(reply: &Message) -> bool {
    matches!(reply, Message::Panes { .. })
}

/// The `(action, outcome, agent)` rows, oldest first — the sequence a review
/// reads.
fn sequence(audit: &serde_json::Value) -> Vec<(String, String, String)> {
    audit["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| {
            (
                row["action"].as_str().unwrap_or_default().to_string(),
                row["outcome"].as_str().unwrap_or_default().to_string(),
                row["agent"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The headline criterion: the actions a client takes are on record, in order,
/// with the pane they touched and the outcome — and the prompt is redacted.
#[test]
fn a_session_leaves_the_actions_it_took_on_record() {
    let daemon = Daemon::start("sequence");

    // spawn, then a read that must NOT be recorded (an audit trail that logs
    // every poll is a trail nobody reads), then a send. One session, because
    // these verbs are all request/response.
    daemon.session(&[
        (
            Message::Spawn {
                v: VERSION,
                id: "pane-a".to_string(),
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "echo hello; sleep 30".to_string()],
                cols: 80,
                rows: 24,
                memory_max: None,
                pids_max: None,
                kill_on_breach: false,
            },
            is_ok,
        ),
        (
            Message::Panes {
                v: VERSION,
                panes: vec![],
            },
            is_panes,
        ),
        (
            Message::Send {
                v: VERSION,
                id: "pane-a".to_string(),
                data: "echo secret-value-abc123\n".to_string(),
            },
            is_ok,
        ),
    ]);

    // `attach` owns its connection (it streams deltas until the child exits), so
    // it gets its own session — the same shape a real client uses, and the reason
    // the daemon documents attach as a streaming verb.
    daemon.attach_once("pane-a");

    let audit = daemon.audit_json();
    let rows = sequence(&audit);
    let actions: Vec<&str> = rows.iter().map(|(action, _, _)| action.as_str()).collect();

    // The writes are recorded, in the order they happened: the spawn, then the
    // send, then the attach (which came last, in its own session).
    assert_eq!(
        actions,
        vec!["spawn", "send", "attach"],
        "the trail is in write order: {rows:?}"
    );

    // Every write carries the pane it acted on and the outcome.
    for (action, outcome, agent) in &rows {
        assert_eq!(outcome, "ok", "{action} should have succeeded");
        assert_eq!(agent, "pane-a", "{action} must name the pane it acted on");
    }
    // And the actor: a local session's actions are attributed to this machine's
    // operator rather than to the invented "cli" the old prompt log used.
    let devices: Vec<&str> = audit["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| row["device"].as_str().unwrap_or_default())
        .collect();
    assert!(
        devices.iter().all(|device| *device == "local-cli"),
        "a local action is attributed to the operator: {devices:?}"
    );

    // The read is absent: nothing recorded the pane listing.
    assert!(
        !actions
            .iter()
            .any(|action| *action == "panes" || *action == "read"),
        "a read must not be audited: {rows:?}"
    );
}

/// The prompt is redacted in the *real* log, and the operator can see it.
#[test]
fn a_prompt_is_redacted_in_the_real_log() {
    let daemon = Daemon::start("redaction");
    daemon.session(&[
        (
            Message::Spawn {
                v: VERSION,
                id: "pane-b".to_string(),
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "sleep 30".to_string()],
                cols: 80,
                rows: 24,
                memory_max: None,
                pids_max: None,
                kill_on_breach: false,
            },
            is_ok,
        ),
        (
            Message::Send {
                v: VERSION,
                id: "pane-b".to_string(),
                data: "export OPENAI_API_KEY=sk-abc123XYZ4567890abcdef\n".to_string(),
            },
            is_ok,
        ),
    ]);

    let audit = daemon.audit_json();
    let rendered = serde_json::to_string(&audit).expect("json");
    assert!(
        !rendered.contains("sk-abc123XYZ4567890abcdef"),
        "the secret reached the audit log: {rendered}"
    );
    let send_row = audit["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .find(|row| row["action"] == serde_json::json!("send"))
        .expect("a send row");
    assert_eq!(send_row["redacted"], serde_json::json!(true));
}

/// A refusal is on record as a refusal, with the reason — the row a review reads
/// to answer "why could that device not get in".
#[test]
fn a_refused_connection_is_recorded_as_refused() {
    let daemon = Daemon::start("refusal");
    // A device that was never pinned: `authorize` is the path the transports
    // call, and it audits its own refusal.
    let stranger = arreo_core::identity::DeviceKey::generate().expect("entropy");
    let output = Command::new(binary("arreo"))
        .args(["devices", "authorize", &stranger.public_hex(), "--socket"])
        .arg(&daemon.socket)
        .env("ARREO_IDENTITY_DIR", &daemon.dir)
        .output()
        .expect("the CLI runs");
    assert!(
        !output.status.success(),
        "an unknown device must be refused"
    );

    let audit = daemon.audit_json();
    let refused = audit["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .find(|row| row["outcome"] == serde_json::json!("refused"))
        .unwrap_or_else(|| panic!("no refused row: {audit}"));
    assert_eq!(refused["action"], serde_json::json!("auth.reject"));
    assert!(
        refused["prompt"]
            .as_str()
            .unwrap_or_default()
            .contains("no certificate"),
        "the refusal names its reason: {refused}"
    );
}

/// The log is ordered by `(ts_ms, rowid)`, so a review reads it in the order
/// things happened even when several rows share a millisecond.
#[test]
fn the_trail_replays_in_write_order() {
    let daemon = Daemon::start("ordering");
    // Several actions in one session, which will share milliseconds.
    daemon.session(&[
        (
            Message::Spawn {
                v: VERSION,
                id: "pane-c".to_string(),
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "sleep 30".to_string()],
                cols: 80,
                rows: 24,
                memory_max: None,
                pids_max: None,
                kill_on_breach: false,
            },
            is_ok,
        ),
        (
            Message::Send {
                v: VERSION,
                id: "pane-c".to_string(),
                data: "first".to_string(),
            },
            is_ok,
        ),
        (
            Message::Send {
                v: VERSION,
                id: "pane-c".to_string(),
                data: "second".to_string(),
            },
            is_ok,
        ),
        (
            Message::Send {
                v: VERSION,
                id: "pane-c".to_string(),
                data: "third".to_string(),
            },
            is_ok,
        ),
    ]);

    let audit = daemon.audit_json();
    let prompts: Vec<String> = audit["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .filter(|row| row["action"] == serde_json::json!("send"))
        .map(|row| row["prompt"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        prompts,
        vec!["first", "second", "third"],
        "the sends replay in the order they were made: {audit}"
    );
}

/// The operator's read-back carries the fields the review needs, and no more:
/// no key material, no full peer address, and a stable JSON shape.
#[test]
fn the_readback_carries_the_review_fields_and_nothing_sensitive() {
    let daemon = Daemon::start("fields");
    daemon.session(&[(
        Message::Spawn {
            v: VERSION,
            id: "pane-d".to_string(),
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "sleep 30".to_string()],
            cols: 80,
            rows: 24,
            memory_max: None,
            pids_max: None,
            kill_on_breach: false,
        },
        is_ok,
    )]);

    let audit = daemon.audit_json();
    let row = &audit["rows"][0];
    for field in [
        "ts_ms", "action", "kind", "outcome", "device", "agent", "prompt", "redacted",
    ] {
        assert!(
            row.get(field).is_some(),
            "the row must carry {field}: {row}"
        );
    }
    // The spawn row keeps the program it ran, which is what makes it useful.
    assert_eq!(row["agent"], serde_json::json!("pane-d"));
    assert_eq!(row["prompt"], serde_json::json!("/bin/sh"));

    // Nothing sensitive anywhere in the read-back.
    let rendered = serde_json::to_string(&audit).expect("json");
    for forbidden in ["PRIVATE KEY", "sk-", "BEGIN OPENSSH"] {
        assert!(
            !rendered.contains(forbidden),
            "the audit read-back contains {forbidden}: {rendered}"
        );
    }
    // And the daemon's own log has not been used as a content channel.
    assert!(
        !daemon.log_text().contains("secret"),
        "the daemon's log must not carry prompt content: {}",
        daemon.log_text()
    );
}

// ---- the remote half: a send over the network is attributed to the device ----

/// The §3.7 criterion: a remote action is audited **on the machine that owns the
/// pane**, with the acting device's id — not with the invented "cli" the old
/// prompt log used, and not on the relay.
///
/// Driven over the real transport (T-0023's loopback test seam), because the
/// thing under test is what the *transport* records about a session it accepted.
#[test]
fn a_remote_send_is_attributed_to_the_device_that_made_it() {
    use arreo_core::identity::{DeviceKey, RootKey};
    use arreo_core::proto::{codec, Message, VERSION};
    use arreo_core::transport::{client_endpoint, open_session};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let daemon = Daemon::start_with_relay_seam("remote");
    let Some(remote_addr) = daemon.remote_addr() else {
        panic!(
            "the daemon never opened the loopback seam; its log was:\n{}",
            daemon.log_text()
        );
    };

    // A device this daemon has pinned, with its key held here (as a real client
    // would hold it).
    let device = DeviceKey::generate().expect("entropy");
    let device_id = arreo_core::identity::DeviceId::from_key(&device.public()).display_id();
    let pin = Command::new(binary("arreo"))
        .args([
            "devices",
            "issue",
            "--socket",
            &daemon.socket.display().to_string(),
            "--name",
            "phone",
            "--role",
            "owner",
            "--key",
            &device.public_hex(),
        ])
        .env("ARREO_IDENTITY_DIR", &daemon.dir)
        .output()
        .expect("the CLI runs");
    assert!(
        pin.status.success(),
        "pinning failed: {}",
        String::from_utf8_lossy(&pin.stderr)
    );
    // The daemon's root key is the key a paired device pins; the transport
    // authenticates the server with it.
    let root = RootKey::load_or_generate(&daemon.dir.join("identity").join("root.key"))
        .expect("the daemon's root key");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let endpoint = client_endpoint().expect("client endpoint");
        let mut channel = open_session(
            &endpoint,
            remote_addr,
            &device.noise_static(),
            &device_id,
            &root.public(),
        )
        .await
        .expect("the remote session opens");

        // Hello → Welcome, then spawn a pane and send into it, all over the
        // network.
        let mut buf = Vec::new();
        let exchange = async |channel: &mut arreo_core::transport::SecureChannel,
                              buf: &mut Vec<u8>,
                              message: Message|
               -> Message {
            let frame = codec::encode_frame(&message).expect("encode");
            channel.write_all(&frame).await.expect("write");
            channel.flush().await.expect("flush");
            loop {
                // Drain what was consumed: without this the next call decodes the
                // same frame again, which looks exactly like the daemon answering
                // a different question.
                if let Ok((reply, consumed)) = codec::decode_frame(buf) {
                    buf.drain(..consumed);
                    return reply;
                }
                let mut chunk = [0u8; 8192];
                let read = channel.read(&mut chunk).await.expect("read");
                assert!(read > 0, "the daemon closed the session");
                buf.extend_from_slice(&chunk[..read]);
            }
        };
        let welcome = exchange(
            &mut channel,
            &mut buf,
            Message::Hello {
                v: VERSION,
                client: "remote-test".to_string(),
                wants: vec![VERSION],
            },
        )
        .await;
        assert!(matches!(welcome, Message::Welcome { .. }), "{welcome:?}");
        let spawned = exchange(
            &mut channel,
            &mut buf,
            Message::Spawn {
                v: VERSION,
                id: "remote-pane".to_string(),
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), "sleep 30".to_string()],
                cols: 80,
                rows: 24,
                memory_max: None,
                pids_max: None,
                kill_on_breach: false,
            },
        )
        .await;
        assert!(matches!(spawned, Message::Ok { .. }), "{spawned:?}");
        let sent = exchange(
            &mut channel,
            &mut buf,
            Message::Send {
                v: VERSION,
                id: "remote-pane".to_string(),
                // A live-looking secret, so the scan below has something to find
                // if redaction ever stops running on this path.
                data: "export GITHUB_TOKEN=ghp_AAAABBBBCCCCDDDDEEEEFFFF\n".to_string(),
            },
        )
        .await;
        assert!(matches!(sent, Message::Ok { .. }), "{sent:?}");
        // Close the session the way a client that means to stop does, rather
        // than leaving the daemon to infer it from fifteen seconds of silence
        // (the QUIC idle timeout, which is the documented bound for a peer that
        // simply vanishes — a dead process cannot announce itself). Closing the
        // endpoint sends CONNECTION_CLOSE, so the daemon writes its
        // `session.disconnect` row immediately, with the reason it was given.
        drop(channel);
        endpoint.close(0u32.into(), b"test finished");
        // `close` only queues the CONNECTION_CLOSE frame; `wait_idle` is what
        // drives it out (and holds the runtime alive long enough to send it), so
        // the daemon sees a closed peer instead of falling back on the idle
        // timeout.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), endpoint.wait_idle()).await;
    });

    // The daemon notices the closed connection asynchronously, so wait for the
    // disconnect row rather than guessing a delay — a sleep long enough today is
    // a flake tomorrow.
    let audit = daemon.await_row("session.disconnect");
    let rows = audit["rows"].as_array().expect("rows");
    // Every row this session wrote names the device, not "cli" and not "local-cli".
    let remote_rows: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["device"] == serde_json::json!(device_id))
        .collect();
    let actions: Vec<&str> = remote_rows
        .iter()
        .map(|row| row["action"].as_str().unwrap_or_default())
        .collect();
    for expected in ["session.connect", "spawn", "send"] {
        assert!(
            actions.contains(&expected),
            "{expected} must be attributed to the device: {actions:?} (all rows: {audit})"
        );
    }
    // The peer's network is recorded, truncated at write: a /24 answers "did this
    // come from a network I know" without being a location history.
    let connect = remote_rows
        .iter()
        .find(|row| row["action"] == serde_json::json!("session.connect"))
        .expect("a connect row");
    assert_eq!(
        connect["peer"],
        serde_json::json!("127.0.0.0/24"),
        "the peer is stored truncated: {connect}"
    );
    // And the session's end is recorded too, so the trail is a session with a
    // beginning and an end rather than a set of rows that stops.
    assert!(
        rows.iter()
            .any(|row| row["action"] == serde_json::json!("session.disconnect")),
        "the session end is recorded: {audit}"
    );

    // Criterion 4's scan, on the real artifacts: the database file and an export
    // hold no live secret and no full address. Asserted on the machinery a real
    // run produces — the raw bytes on disk and the bytes a script would read —
    // rather than on the rows we already decoded, because the point is that the
    // *stored* form is safe.
    // Every file SQLite writes for this database, not just the `.db`: a row can
    // still be in the write-ahead log, so a scan of the main file alone would
    // pass whether or not redaction ran — a vacuous assertion.
    let mut on_disk = String::new();
    for suffix in ["arreo.sock.db", "arreo.sock.db-wal", "arreo.sock.db-shm"] {
        if let Ok(bytes) = std::fs::read(daemon.dir.join(suffix)) {
            on_disk.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    let db_text = on_disk.as_str();
    assert!(
        db_text.contains("remote-pane"),
        "the scan must be looking at real rows, not an empty file: {db_text}"
    );
    assert!(
        !db_text.contains("ghp_AAAABBBBCCCCDDDDEEEEFFFF"),
        "the live secret must not be on disk; the stored form was: {db_text}"
    );
    assert!(
        db_text.contains("[REDACTED:token]"),
        "the secret's place is marked, so the row is still readable: {db_text}"
    );
    let exported = Command::new(binary("arreo"))
        .args([
            "audit",
            "export",
            "--format",
            "jsonl",
            "--socket",
            &daemon.socket.display().to_string(),
        ])
        .env("ARREO_IDENTITY_DIR", &daemon.dir)
        .output()
        .expect("the CLI runs");
    assert!(
        exported.status.success(),
        "the export runs: {}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let export_text = String::from_utf8_lossy(&exported.stdout);
    assert!(
        !export_text.contains("ghp_AAAABBBBCCCCDDDDEEEEFFFF"),
        "no export may re-introduce the secret: {export_text}"
    );
    // No full address anywhere either: the loopback peer is a /24 in every row
    // and in every export, and the port it connected from is gone.
    let port = remote_addr.port().to_string();
    for (what, text) in [("the log", db_text), ("the export", export_text.as_ref())] {
        assert!(
            !text.contains(&format!("127.0.0.1:{port}")),
            "{what} must not carry a full peer address: {text}"
        );
        assert!(
            !text.contains("127.0.0.1:"),
            "{what} must not carry a peer port at all: {text}"
        );
    }

    // One spelling per device across the whole log: an operator greps one
    // pattern, not two. (`dev_<hex>` is the form the CLI prints everywhere.)
    for row in rows {
        if let Some(device) = row["device"].as_str() {
            if device.is_empty() || device == "local-cli" || device == "daemon" {
                continue;
            }
            assert!(
                device.starts_with("dev_") || device.len() == 32,
                "unexpected device spelling {device:?} in {row}"
            );
        }
    }
    // Specifically: the issue row and the session rows name the same device the
    // same way, so "everything about this device" is one query.
    let issue = rows
        .iter()
        .find(|row| row["action"] == serde_json::json!("device.issue"))
        .expect("the pin is on record");
    assert_eq!(
        issue["device"],
        serde_json::json!(device_id),
        "the pin and the session agree on the device's name: {issue}"
    );
}
