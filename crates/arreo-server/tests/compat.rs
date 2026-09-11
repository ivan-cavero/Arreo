//! T-0028 compat tests over the real daemon: both directions of the N−1 window,
//! plus the unknown-verb rules, against a live `serve_session` loop.
//!
//! The unit matrix lives in `arreo-core/tests/compat.rs` (negotiation,
//! classification, corpora). These tests prove the *daemon* honors the rules:
//! a refused handshake writes an audit row and leaves no session, an unknown
//! request is answered with the connection left open, and shared verbs behave
//! identically whatever the client offered.

use arreo_core::proto::{codec, Message, VERSION};
use arreo_server::daemon::Daemon;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn temp_socket(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "arreo-compat-{name}-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    ))
}

async fn spawn_daemon(socket: PathBuf) -> tokio::task::JoinHandle<()> {
    let _ = std::fs::remove_file(&socket);
    let daemon = Daemon::new(&socket);
    tokio::spawn(async move {
        let _ = daemon.serve().await;
    })
}

/// A raw client: handshake with an explicit `wants`, then raw frames.
struct Raw {
    reader: tokio::net::unix::OwnedReadHalf,
    writer: tokio::net::unix::OwnedWriteHalf,
    buf: Vec<u8>,
}

impl Raw {
    async fn connect(socket: &PathBuf) -> Self {
        let stream = tokio::net::UnixStream::connect(socket)
            .await
            .expect("connect");
        let (reader, writer) = stream.into_split();
        Self {
            reader,
            writer,
            buf: Vec::new(),
        }
    }

    async fn send(&mut self, message: &Message) {
        let frame = codec::encode_frame(message).expect("encode");
        self.writer.write_all(&frame).await.expect("write");
        self.writer.flush().await.expect("flush");
    }

    async fn send_raw(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).await.expect("write");
        self.writer.flush().await.expect("flush");
    }

    async fn recv(&mut self) -> Message {
        loop {
            if let Ok((message, consumed)) = codec::decode_frame(&self.buf) {
                self.buf.drain(..consumed);
                return message;
            }
            let mut chunk = [0u8; 8192];
            let n = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                self.reader.read(&mut chunk),
            )
            .await
            .expect("read timeout — the daemon hung instead of answering")
            .expect("read");
            assert!(n > 0, "server closed connection");
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

/// A v0 client (offers only v0) against this server keeps working for every
/// shared verb: the window downgrades explicitly via `Welcome.v`.
#[tokio::test]
async fn a_v0_client_keeps_working_for_shared_verbs() {
    let socket = temp_socket("v0client");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut client = Raw::connect(&socket).await;
    client
        .send(&Message::Hello {
            v: 0,
            client: "v0-client".to_string(),
            wants: vec![0],
        })
        .await;
    match client.recv().await {
        Message::Welcome { v, .. } => assert_eq!(v, 0, "the downgrade is explicit"),
        other => panic!("want Welcome, got {other:?}"),
    }
    // The full shared-verb sequence behaves identically.
    client
        .send(&Message::Spawn {
            v: 0,
            id: "a".to_string(),
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "echo hi && sleep 30".to_string()],
            cols: 80,
            rows: 24,
            memory_max: None,
            pids_max: None,
            kill_on_breach: false,
        })
        .await;
    assert!(matches!(client.recv().await, Message::Ok { .. }), "spawn");
    client
        .send(&Message::Panes {
            v: 0,
            panes: vec![],
        })
        .await;
    match client.recv().await {
        Message::Panes { panes, .. } => assert!(panes.iter().any(|p| p.id == "a")),
        other => panic!("want Panes, got {other:?}"),
    }
    client
        .send(&Message::Send {
            v: 0,
            id: "a".to_string(),
            data: "echo v0-ok\n".to_string(),
        })
        .await;
    assert!(matches!(client.recv().await, Message::Ok { .. }), "send");
    client
        .send(&Message::Read {
            v: 0,
            id: "a".into(),
            from_line: 0,
        })
        .await;
    assert!(
        matches!(client.recv().await, Message::Delta { .. }),
        "read answers with a Delta"
    );
}

/// A client offering only a version outside the window is refused loudly: a
/// typed `Error` naming the range, then the connection ends — and the refusal
/// is on the audit trail with no session behind it (T-0028: refused, never
/// partial).
#[tokio::test]
async fn a_client_outside_the_window_is_refused_loudly() {
    let socket = temp_socket("outside");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut client = Raw::connect(&socket).await;
    client
        .send(&Message::Hello {
            v: 99,
            client: "future".to_string(),
            wants: vec![99],
        })
        .await;
    match client.recv().await {
        Message::Error { message, .. } => {
            assert!(
                message.contains("99"),
                "the refusal names what was offered: {message}"
            );
        }
        other => panic!("want a typed Error, got {other:?}"),
    }
    drop(client);
    // The refusal left a row and no session: the audit trail names the refusal,
    // and no pane was created by a connection that never got past Hello.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut db = socket.as_os_str().to_owned();
    db.push(".db");
    let store = arreo_core::store::SessionStore::open(&PathBuf::from(db)).expect("audit store");
    let rows = store
        .audit_by_action(arreo_core::store::actions::AUTH_REJECT, 10)
        .expect("audit rows");
    assert_eq!(rows.len(), 1, "one refused handshake, one audit row");
    assert_eq!(rows[0].outcome, arreo_core::store::AuditOutcome::Refused);
    assert!(
        rows[0].prompt.contains("99")
            || rows[0].detail.as_deref().is_some_and(|d| d.contains("99")),
        "the row names what was offered: {:?}",
        rows[0]
    );
}

/// An unknown request verb is answered with a typed `Error` and the connection
/// stays open: the client sends a shared verb next and it works. No hang, no
/// silent discard of a state-mutating message.
#[tokio::test]
async fn an_unknown_request_is_refused_and_the_session_survives() {
    let socket = temp_socket("unknown-req");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut client = Raw::connect(&socket).await;
    client
        .send(&Message::Hello {
            v: VERSION,
            client: "c".to_string(),
            wants: vec![VERSION],
        })
        .await;
    assert!(matches!(client.recv().await, Message::Welcome { .. }));

    // A v1 verb, hand-framed: {"op": "teleport", "v": 1, "id": "p"}.
    let mut frame = vec![0x84u8];
    frame.extend(rmp_str("op"));
    frame.extend(rmp_str("teleport"));
    frame.extend(rmp_str("v"));
    frame.push(0x01);
    frame.extend(rmp_str("id"));
    frame.extend(rmp_str("p"));
    let mut wire = (frame.len() as u32).to_le_bytes().to_vec();
    wire.extend(frame);
    client.send_raw(&wire).await;

    match tokio::time::timeout(std::time::Duration::from_secs(10), client.recv()).await {
        Ok(Message::Error { message, .. }) => assert!(
            message.contains("teleport"),
            "the refusal names the unknown op: {message}"
        ),
        Ok(other) => panic!("want a typed Error for the unknown verb, got {other:?}"),
        Err(_) => panic!("the daemon hung on an unknown request instead of refusing"),
    }

    // The session survived: a shared verb works immediately after.
    client
        .send(&Message::Panes {
            v: VERSION,
            panes: vec![],
        })
        .await;
    assert!(
        matches!(
            tokio::time::timeout(std::time::Duration::from_secs(10), client.recv()).await,
            Ok(Message::Panes { .. })
        ),
        "the session stays open after refusing an unknown request"
    );
}

fn rmp_str(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    assert!(bytes.len() < 32);
    let mut out = vec![0xa0 | bytes.len() as u8];
    out.extend(bytes);
    out
}
