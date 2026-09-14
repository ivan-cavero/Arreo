//! T-0014 failing-first probes: MessagePack socket API v1.
//!
//! Same style as the T-0005 daemon tests (real daemon, temp socket), but
//! speaking framed `Message` with a Hello→Welcome handshake. Every verb in
//! the criterion gets an integration test over the real socket here.

use arreo_core::proto::codec;
use arreo_core::proto::{AgentState, Message, VERSION};
use arreo_server::daemon::Daemon;
use std::path::PathBuf;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn temp_socket(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("arreo-api-{name}-{}.sock", std::process::id()))
}

async fn spawn_daemon(socket: PathBuf) -> tokio::task::JoinHandle<()> {
    let _ = std::fs::remove_file(&socket);
    let daemon = Daemon::new(&socket);
    tokio::spawn(async move {
        let _ = daemon.serve().await;
    })
}

/// Framed MessagePack client: Hello handshake on connect, then request loop.
struct Client {
    stream: tokio::net::unix::OwnedWriteHalf,
    reader: tokio::net::unix::OwnedReadHalf,
    buf: Vec<u8>,
}

impl Client {
    async fn connect(socket: &PathBuf) -> Self {
        let stream = tokio::net::UnixStream::connect(socket)
            .await
            .expect("connect");
        let (reader, stream) = stream.into_split();
        let mut client = Self {
            stream,
            reader,
            buf: Vec::new(),
        };
        client
            .send(&Message::Hello {
                v: VERSION,
                client: "api-test".to_string(),
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
            let n = tokio::time::timeout(
                std::time::Duration::from_secs(10),
                self.reader.read(&mut chunk),
            )
            .await
            .expect("read timeout")
            .expect("read");
            assert!(n > 0, "server closed connection");
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

fn spawn_msg(id: &str) -> Message {
    Message::Spawn {
        v: VERSION,
        id: id.to_string(),
        program: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), "echo hello-api && sleep 30".to_string()],
        cols: 80,
        rows: 24,
        memory_max: None,
        pids_max: None,
        kill_on_breach: false,
    }
}

#[tokio::test]
async fn hello_handshake_and_version_rejection() {
    let socket = temp_socket("hello");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Good handshake covered by Client::connect. Bad version: raw frame.
    let stream = tokio::net::UnixStream::connect(&socket)
        .await
        .expect("connect");
    let (mut reader, mut writer) = stream.into_split();
    let bad = Message::Hello {
        v: 99,
        client: "x".to_string(),
        wants: vec![99],
    };
    let frame = codec::encode_frame(&bad).expect("encode");
    writer.write_all(&frame).await.expect("write");
    let mut buf = vec![0u8; 4096];
    let mut acc = Vec::new();
    let reply = loop {
        let n = reader.read(&mut buf).await.expect("read");
        acc.extend_from_slice(&buf[..n]);
        if let Ok((message, _)) = codec::decode_frame(&acc) {
            break message;
        }
    };
    assert!(
        matches!(reply, Message::Error { .. }),
        "loud rejection: {reply:?}"
    );
}

#[tokio::test]
async fn spawn_panes_read_send_split() {
    let socket = temp_socket("verbs");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut client = Client::connect(&socket).await;

    client.send(&spawn_msg("a")).await;
    assert!(matches!(client.recv().await, Message::Ok { .. }));

    // read: snapshot of current text.
    client
        .send(&Message::Attach {
            v: VERSION,
            id: "a".to_string(),
            from_line: 0,
        })
        .await;
    let mut saw_hello = false;
    for _ in 0..50 {
        match client.recv().await {
            Message::Delta { lines, .. } | Message::Snapshot { lines, .. } => {
                if lines.iter().any(|l| l.contains("hello-api")) {
                    saw_hello = true;
                    break;
                }
            }
            Message::Exited { .. } => break,
            other => panic!("want delta, got {other:?}"),
        }
    }
    // NOTE: attach OWNS this connection (F4, T-0009) — further verbs need a
    // fresh connection. Open a second client for control verbs.
    let mut control = Client::connect(&socket).await;

    // send: input reaches the shell (echo it back via a marker).
    control
        .send(&Message::Send {
            v: VERSION,
            id: "a".to_string(),
            data: "echo back-marker-1\r".to_string(),
        })
        .await;
    assert!(matches!(control.recv().await, Message::Ok { .. }));
    assert!(saw_hello);

    // split: second pane, both listed.
    control
        .send(&Message::Spawn {
            v: VERSION,
            id: "b".to_string(),
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "sleep 30".to_string()],
            cols: 80,
            rows: 24,
            memory_max: None,
            pids_max: None,
            kill_on_breach: false,
        })
        .await;
    assert!(matches!(control.recv().await, Message::Ok { .. }));
    control
        .send(&Message::Panes {
            v: VERSION,
            panes: vec![],
        })
        .await;
    match control.recv().await {
        Message::Panes { panes, .. } => {
            assert!(panes.iter().any(|p| p.id == "a"));
            assert!(panes.iter().any(|p| p.id == "b"));
        }
        other => panic!("want panes, got {other:?}"),
    }
}

#[tokio::test]
async fn wait_watches_state_with_timeout() {
    let socket = temp_socket("wait");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut client = Client::connect(&socket).await;

    // Pane that asks a question after a beat.
    client
        .send(&Message::Spawn {
            v: VERSION,
            id: "q".to_string(),
            program: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "sleep 0.5; printf 'May I proceed? [y/n] '; sleep 30".to_string(),
            ],
            cols: 80,
            rows: 24,
            memory_max: None,
            pids_max: None,
            kill_on_breach: false,
        })
        .await;
    assert!(matches!(client.recv().await, Message::Ok { .. }));

    // wait for question: server watches the state engine, answers on match.
    client
        .send(&Message::Wait {
            v: VERSION,
            id: "q".to_string(),
            state: AgentState::Question,
            timeout_ms: 15_000,
        })
        .await;
    match tokio::time::timeout(std::time::Duration::from_secs(20), client.recv()).await {
        Ok(Message::StateEvent { state, .. }) => {
            assert_eq!(state, AgentState::Question);
        }
        Ok(other) => panic!("want question event, got {other:?}"),
        Err(_) => panic!("wait timed out though the pane asked"),
    }

    // wait for a state that never comes: timeout is loud, not a hang.
    let mut control = Client::connect(&socket).await;
    control
        .send(&Message::Wait {
            v: VERSION,
            id: "q".to_string(),
            state: AgentState::Done,
            timeout_ms: 500,
        })
        .await;
    match control.recv().await {
        Message::Error { message, .. } => assert!(message.contains("timeout")),
        other => panic!("want timeout error, got {other:?}"),
    }
}

#[tokio::test]
async fn metrics_reports_tree_truth() {
    let socket = temp_socket("metrics");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut client = Client::connect(&socket).await;

    client.send(&spawn_msg("m")).await;
    assert!(matches!(client.recv().await, Message::Ok { .. }));
    client
        .send(&Message::MetricsReq {
            v: VERSION,
            id: "m".to_string(),
        })
        .await;
    match client.recv().await {
        Message::Metrics {
            rss_bytes, pids, ..
        } => {
            assert!(rss_bytes > 0, "RSS observed");
            assert!(pids >= 1, "at least the child");
        }
        other => panic!("want metrics, got {other:?}"),
    }
    // Unknown pane: loud error.
    client
        .send(&Message::MetricsReq {
            v: VERSION,
            id: "ghost".to_string(),
        })
        .await;
    assert!(matches!(client.recv().await, Message::Error { .. }));
}

#[tokio::test]
async fn spawn_with_budget_attaches_guard_or_errors_loudly() {
    // On cgroup-less boxes Guard::create fails → daemon must answer Error
    // (loud), never Ok-then-unenforced (lying). On delegated boxes it
    // answers Ok. Either way the contract holds: no silent unenforced pane.
    let socket = temp_socket("budget");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut client = Client::connect(&socket).await;
    client
        .send(&Message::Spawn {
            v: VERSION,
            id: "guarded".to_string(),
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "sleep 30".to_string()],
            cols: 80,
            rows: 24,
            memory_max: Some(256 * 1024 * 1024),
            pids_max: Some(32),
            kill_on_breach: false,
        })
        .await;
    match client.recv().await {
        Message::Ok { .. } => {
            // Guard live: pane listed, breach poll runs (no breach expected).
            client
                .send(&Message::Panes {
                    v: VERSION,
                    panes: vec![],
                })
                .await;
            match client.recv().await {
                Message::Panes { panes, .. } => {
                    assert!(panes.iter().any(|p| p.id == "guarded" && p.alive));
                }
                other => panic!("want panes, got {other:?}"),
            }
        }
        Message::Error { message, .. } => {
            assert!(
                message.contains("enforce"),
                "loud enforce failure, not silent: {message}"
            );
        }
        other => panic!("want Ok or enforce Error, got {other:?}"),
    }
}

/// **A line a program writes in two pieces arrives as one line, and never
/// twice** (T-0088).
///
/// The delta path is line-indexed: the daemon slices what the pane reports and
/// advances a cursor, and the client can only append what it is sent. The pane
/// also has to report the *unterminated* trailing line, because a prompt without
/// a newline is the product's flagship signal — so the two facts have to agree:
/// a partial is delivered (the operator sees it), and its completion is
/// delivered too (the operator is not shown half a line for ever). The old code
/// materialised the partial into the ring, which gave one written line two
/// entries; this drives the sequence through the real daemon and asserts the
/// joined result.
#[tokio::test]
async fn a_line_written_in_two_pieces_arrives_once_and_whole() {
    let socket = temp_socket("partial-line");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut control = Client::connect(&socket).await;
    control
        .send(&Message::Spawn {
            v: VERSION,
            id: "slow".to_string(),
            program: "/bin/sh".to_string(),
            // One line, written in two pieces with a pause between them — the
            // shape any slow-writing program has, and the one that used to split.
            args: vec![
                "-c".to_string(),
                "printf 'half-one|aaaa'; sleep 1; printf 'bbbb\\n'; sleep 30".to_string(),
            ],
            cols: 80,
            rows: 24,
            memory_max: None,
            pids_max: None,
            kill_on_breach: false,
        })
        .await;
    assert!(matches!(control.recv().await, Message::Ok { .. }));

    // A client that cannot tell a partial from a terminated line re-attaches
    // from `cursor - 1` while the tail is a partial (the cursor rule
    // `stream_attach` documents: a partial sits at the index it will occupy once
    // terminated, so re-fetching it is what lets the completion arrive whole).
    let mut cursor = 0usize;
    let mut seen: Vec<String> = Vec::new();
    for _ in 0..4 {
        let mut attach = Client::connect(&socket).await;
        attach
            .send(&Message::Attach {
                v: VERSION,
                id: "slow".to_string(),
                from_line: cursor,
            })
            .await;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
        loop {
            if std::time::Instant::now() > deadline {
                break;
            }
            match tokio::time::timeout(std::time::Duration::from_millis(700), attach.recv()).await {
                Ok(Message::Delta { lines, .. }) | Ok(Message::Snapshot { lines, .. }) => {
                    // Append is only sound for a line the client had already
                    // fetched as a *partial*: drop it, then re-add the whole
                    // slice (the daemon re-sends from `cursor`).
                    if lines
                        .first()
                        .is_some_and(|line| line.contains("half-one") && !line.contains("bbbb"))
                        && seen.last().is_some_and(|last| last == &lines[0])
                    {
                        seen.pop();
                    }
                    seen.extend(lines);
                    cursor = seen.len();
                }
                Ok(Message::Exited { .. }) => break,
                Ok(_) => {}
                Err(_) => break,
            }
        }
        if seen.iter().any(|line| line.contains("half-one|aaaabbbb")) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }

    assert!(
        seen.iter().any(|line| line == "half-one|aaaabbbb"),
        "the two pieces arrived as one line; the client saw {seen:?}"
    );
    // The line the **ring** holds is one line: a fresh reader that never saw the
    // partial gets the whole thing once. (The streaming client above fetches the
    // partial and then the same slot again — that is the cursor rule, and it is
    // why the completion is a replacement in the client's last slot rather than
    // a second line appended beside it.)
    let mut reader = Client::connect(&socket).await;
    reader
        .send(&Message::Attach {
            v: VERSION,
            id: "slow".to_string(),
            from_line: 0,
        })
        .await;
    let mut fresh: Vec<String> = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while std::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), reader.recv()).await {
            Ok(Message::Delta { lines, .. }) | Ok(Message::Snapshot { lines, .. }) => {
                fresh.extend(lines)
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let half: Vec<&String> = fresh.iter().filter(|l| l.contains("half-one")).collect();
    assert_eq!(
        half,
        vec![&"half-one|aaaabbbb".to_string()],
        "a fresh reader sees the line once, whole: {fresh:?}"
    );
    // And never the materialised fragments the old ring held.
    assert!(
        !fresh.iter().any(|line| line == "half-one|aaaa"),
        "the ring never held the half-line as its own line: {fresh:?}"
    );
}
