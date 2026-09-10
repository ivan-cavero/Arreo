//! T-0005 failing-first probes: daemon serves, CLI attaches.
//!
//! Spins a real daemon on a temp socket per test (tokio). No mocks — the
//! client in these tests speaks the same JSONL the CLI speaks.

use arreo_server::daemon::Daemon;
use arreo_server::protocol::{Request, Response};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn temp_socket(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!("arreo-test-{}-{}.sock", name, std::process::id()))
}

async fn spawn_daemon(socket: PathBuf) -> tokio::task::JoinHandle<()> {
    let _ = std::fs::remove_file(&socket);
    let daemon = Daemon::new(&socket);
    tokio::spawn(async move {
        let _ = daemon.serve().await;
    })
}

struct Client {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl Client {
    async fn connect(socket: &PathBuf) -> Self {
        let stream = UnixStream::connect(socket).await.expect("connect");
        let (reader, writer) = stream.into_split();
        Self {
            reader: BufReader::new(reader),
            writer,
        }
    }

    async fn send(&mut self, request: &Request) {
        let mut line = serde_json::to_string(request).unwrap();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await.unwrap();
        self.writer.flush().await.unwrap();
    }

    async fn recv(&mut self) -> Response {
        let mut line = String::new();
        self.reader.read_line(&mut line).await.expect("read");
        serde_json::from_str(&line).expect("valid response")
    }
}

fn spawn_req(id: &str) -> Request {
    Request::Spawn {
        v: 0,
        id: id.to_string(),
        program: "/bin/sh".to_string(),
        args: vec![
            "-c".to_string(),
            "echo hello-attach && sleep 30".to_string(),
        ],
        cols: 80,
        rows: 24,
    }
}

#[tokio::test]
async fn spawn_list_send_attach_same_truth() {
    let socket = temp_socket("basic");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut client = Client::connect(&socket).await;
    client.send(&spawn_req("pane-1")).await;
    assert!(matches!(client.recv().await, Response::Ok { .. }));

    client.send(&Request::List { v: 0 }).await;
    match client.recv().await {
        Response::Panes { panes, .. } => {
            assert!(panes.iter().any(|p| p.id == "pane-1" && p.alive));
        }
        other => panic!("want panes, got {other:?}"),
    }

    // Attach from_line 0: must see the echo.
    client
        .send(&Request::Attach {
            v: 0,
            id: "pane-1".to_string(),
            from_line: 0,
        })
        .await;
    let mut saw_hello = false;
    for _ in 0..50 {
        match client.recv().await {
            Response::Output { lines, .. } => {
                if lines.iter().any(|l| l.contains("hello-attach")) {
                    saw_hello = true;
                    break;
                }
            }
            Response::Exited { .. } => break,
            other => panic!("want output, got {other:?}"),
        }
    }
    assert!(saw_hello, "attach streams pane output");
}

#[tokio::test]
async fn two_clients_see_same_truth() {
    let socket = temp_socket("two");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut a = Client::connect(&socket).await;
    let mut b = Client::connect(&socket).await;
    a.send(&spawn_req("shared")).await;
    assert!(matches!(a.recv().await, Response::Ok { .. }));

    // Both attach; both must observe the same line.
    for client in [&mut a, &mut b] {
        client
            .send(&Request::Attach {
                v: 0,
                id: "shared".to_string(),
                from_line: 0,
            })
            .await;
    }
    for (name, client) in [("a", &mut a), ("b", &mut b)] {
        let mut saw = false;
        for _ in 0..50 {
            match client.recv().await {
                Response::Output { lines, .. } => {
                    if lines.iter().any(|l| l.contains("hello-attach")) {
                        saw = true;
                        break;
                    }
                }
                Response::Exited { .. } => break,
                other => panic!("client {name}: want output, got {other:?}"),
            }
        }
        assert!(saw, "client {name} sees the same truth");
    }
}

#[tokio::test]
async fn detach_reattach_keeps_scrollback() {
    let socket = temp_socket("reattach");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut client = Client::connect(&socket).await;
    client
        .send(&Request::Spawn {
            v: 0,
            id: "re".to_string(),
            program: "/bin/sh".to_string(),
            args: vec![
                "-c".to_string(),
                "echo line-one && echo line-two && sleep 30".to_string(),
            ],
            cols: 80,
            rows: 24,
        })
        .await;
    assert!(matches!(client.recv().await, Response::Ok { .. }));

    // First attach: read until both lines seen, note cursor, drop client.
    client
        .send(&Request::Attach {
            v: 0,
            id: "re".to_string(),
            from_line: 0,
        })
        .await;
    let mut cursor = 0;
    for _ in 0..50 {
        match client.recv().await {
            Response::Output {
                lines, from_line, ..
            } => {
                cursor = from_line + lines.len();
                if lines.iter().any(|l| l.contains("line-two")) {
                    break;
                }
            }
            Response::Exited { .. } => break,
            other => panic!("want output, got {other:?}"),
        }
    }
    assert!(cursor >= 2, "cursor advanced past both lines");
    drop(client);

    // Reattach from 0: full scrollback intact (server kept the Pane).
    let mut client2 = Client::connect(&socket).await;
    client2
        .send(&Request::Attach {
            v: 0,
            id: "re".to_string(),
            from_line: 0,
        })
        .await;
    let mut all = Vec::new();
    for _ in 0..50 {
        match tokio::time::timeout(std::time::Duration::from_secs(2), client2.recv()).await {
            Ok(Response::Output { lines, .. }) => {
                all.extend(lines);
                if all.iter().any(|l| l.contains("line-two")) {
                    break;
                }
            }
            Ok(Response::Exited { .. }) | Err(_) => break,
            Ok(other) => panic!("want output, got {other:?}"),
        }
    }
    assert!(
        all.iter().any(|l| l.contains("line-one")),
        "line-one survives: {all:?}"
    );
    assert!(
        all.iter().any(|l| l.contains("line-two")),
        "line-two survives: {all:?}"
    );
}

#[tokio::test]
async fn unknown_pane_and_version_are_loud_errors() {
    let socket = temp_socket("errors");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut client = Client::connect(&socket).await;
    client
        .send(&Request::Attach {
            v: 0,
            id: "nope".to_string(),
            from_line: 0,
        })
        .await;
    assert!(matches!(client.recv().await, Response::Error { .. }));

    client.send(&Request::List { v: 99 }).await;
    match client.recv().await {
        Response::Error { message, .. } => assert!(message.contains("unsupported version")),
        other => panic!("want version error, got {other:?}"),
    }
}

#[tokio::test]
async fn kill_terminates_and_reaps_the_child() {
    let socket = temp_socket("kill");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut client = Client::connect(&socket).await;
    client
        .send(&Request::Spawn {
            v: 0,
            id: "victim".to_string(),
            program: "/bin/sleep".to_string(),
            args: vec!["60".to_string()],
            cols: 80,
            rows: 24,
        })
        .await;
    assert!(matches!(client.recv().await, Response::Ok { .. }));

    // Resolve the child's PID via panes? Not exposed — instead assert via
    // list (alive first), kill, then list (gone) + attach errors.
    client.send(&Request::List { v: 0 }).await;
    match client.recv().await {
        Response::Panes { panes, .. } => {
            assert!(panes.iter().any(|p| p.id == "victim" && p.alive));
        }
        other => panic!("want panes, got {other:?}"),
    }
    client
        .send(&Request::Kill {
            v: 0,
            id: "victim".to_string(),
        })
        .await;
    assert!(matches!(client.recv().await, Response::Ok { .. }));
    client.send(&Request::List { v: 0 }).await;
    match client.recv().await {
        Response::Panes { panes, .. } => {
            assert!(!panes.iter().any(|p| p.id == "victim"), "gone: {panes:?}");
        }
        other => panic!("want panes, got {other:?}"),
    }
    // Second kill is a loud error, not silent success.
    client
        .send(&Request::Kill {
            v: 0,
            id: "victim".to_string(),
        })
        .await;
    assert!(matches!(client.recv().await, Response::Error { .. }));
}

/// Regression for the T-0009 chaos find: `Pane::spawn` (fork) hung when
/// called on a multi-threaded tokio worker. The daemon now spawns via
/// `spawn_blocking` — this test runs the whole serve+spawn cycle on a
/// multi-thread runtime, where the old code deadlocked.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn spawn_answers_on_multithread_runtime() {
    let socket = temp_socket("mt");
    let _server = spawn_daemon(socket.clone()).await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut client = Client::connect(&socket).await;
    client.send(&spawn_req("mt-pane")).await;
    let reply = tokio::time::timeout(std::time::Duration::from_secs(10), client.recv())
        .await
        .expect("spawn answers (no fork deadlock)");
    assert!(matches!(reply, Response::Ok { .. }), "got {reply:?}");
}
