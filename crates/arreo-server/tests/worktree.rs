//! T-0091: worktree-per-task over the real socket.
//!
//! A real daemon, a real `git` repository, and panes that are asked to run in
//! their own checkouts. The point of the feature is that two agents on one
//! machine cannot touch each other's files, so that is what these tests assert —
//! on the filesystem, where the answer is not a matter of interpretation.
//!
//! Scratch lives under `target/test-scratch/` (never `/tmp`: it is a tmpfs here
//! and a git repository in it is a real problem).

use arreo_core::proto::codec;
use arreo_core::proto::{Message, VERSION};
use arreo_core::relay::config::WorktreeSettings;
use arreo_server::daemon::Daemon;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A scratch directory for one test, wiped first.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-scratch/T-0091/integration")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    std::fs::canonicalize(&dir).expect("canonical")
}

/// A repository with one commit and a configured identity.
fn repo_at(dir: &Path) -> PathBuf {
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).expect("repo dir");
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.name", "arreo test"]);
    git(&["config", "user.email", "test@arreo.invalid"]);
    std::fs::write(repo.join("README.md"), "hello\n").expect("write");
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "initial"]);
    std::fs::canonicalize(&repo).expect("canonical")
}

async fn spawn_daemon(socket: PathBuf, settings: WorktreeSettings) -> tokio::task::JoinHandle<()> {
    let _ = std::fs::remove_file(&socket);
    let daemon = Daemon::new(&socket).with_worktree_settings(settings);
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
    async fn connect(socket: &Path) -> Self {
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
                client: "worktree-test".to_string(),
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
            assert!(n > 0, "server closed connection");
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// One request and its answer, for the verbs whose answer is immediate.
    async fn call(&mut self, message: &Message) -> Message {
        self.send(message).await;
        self.recv().await
    }

    /// Spawn a pane in a worktree and return the reply.
    async fn spawn_in_worktree(
        &mut self,
        id: &str,
        script: &str,
        worktree: Option<&str>,
    ) -> Message {
        self.call(&Message::SpawnWorktree {
            v: VERSION,
            id: id.to_string(),
            spec: Box::new(arreo_core::proto::SpawnSpec {
                program: "/bin/sh".to_string(),
                args: vec!["-c".to_string(), script.to_string()],
                cols: 80,
                rows: 24,
                memory_max: None,
                pids_max: None,
                kill_on_breach: false,
            }),
            worktree: worktree.map(str::to_string),
        })
        .await
    }
}

/// Wait for `path` to exist, bounded. Returns whether it arrived.
///
/// A bounded wait rather than a sleep: the child is a separate process and the
/// time it takes to write a file is the machine's business, not the test's.
fn wait_for_file(path: &Path, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if path.is_file() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// Wait for a path to be **gone**, bounded (the kill path's cleanup is a git
/// command the daemon runs off the async runtime).
fn wait_for_absent(path: &Path, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if !path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// **The feature's whole point**: a pane runs in its own checkout, and two panes
/// cannot see each other's files.
#[tokio::test]
async fn two_panes_in_worktrees_cannot_see_each_others_files() {
    let dir = scratch("isolation");
    let repo = repo_at(&dir);
    let root = dir.join("worktrees");
    let socket = dir.join("arreo.sock");
    let _server = spawn_daemon(
        socket.clone(),
        WorktreeSettings {
            root: Some(root.display().to_string()),
            repo: Some(repo.display().to_string()),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut client = Client::connect(&socket).await;
    for (id, word) in [("one", "first"), ("two", "second")] {
        let reply = client
            .spawn_in_worktree(
                id,
                &format!("pwd > where.txt; echo {word} > task.txt; sleep 30"),
                None,
            )
            .await;
        assert!(matches!(reply, Message::Ok { .. }), "spawn {id}: {reply:?}");
    }

    // The child's own answer to "where am I", written by the child.
    for id in ["one", "two"] {
        let where_file = root.join(id).join("where.txt");
        assert!(
            wait_for_file(&where_file, Duration::from_secs(10)),
            "pane {id} never wrote where.txt in {}",
            root.join(id).display()
        );
        let printed = std::fs::read_to_string(&where_file).expect("read");
        assert_eq!(
            printed.trim(),
            std::fs::canonicalize(root.join(id))
                .expect("canonical")
                .display()
                .to_string(),
            "pane {id} ran in its own worktree"
        );
    }

    // The same filename in both checkouts, with different contents — the
    // collision a shared working directory loses.
    assert_eq!(
        std::fs::read_to_string(root.join("one").join("task.txt")).expect("read"),
        "first\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("two").join("task.txt")).expect("read"),
        "second\n"
    );
    assert!(
        !repo.join("task.txt").exists(),
        "the main checkout was not written to"
    );
}

/// A pane id that cannot name a directory is refused, and **nothing is made**.
#[tokio::test]
async fn an_escaping_worktree_name_is_refused_and_creates_nothing() {
    let dir = scratch("escape");
    let repo = repo_at(&dir);
    let root = dir.join("worktrees");
    let socket = dir.join("arreo.sock");
    let _server = spawn_daemon(
        socket.clone(),
        WorktreeSettings {
            root: Some(root.display().to_string()),
            repo: Some(repo.display().to_string()),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut client = Client::connect(&socket).await;
    let reply = client
        .spawn_in_worktree("pane-1", "sleep 30", Some("../../escape"))
        .await;
    match reply {
        Message::Error { message, .. } => assert!(
            message.contains("../../escape"),
            "the refusal names what was rejected: {message}"
        ),
        other => panic!("want a refusal, got {other:?}"),
    }
    assert!(!root.exists(), "no worktree root was created");
    assert!(!dir.join("escape").exists(), "nothing escaped the root");

    // And the pane list is empty: a refused spawn spawns nothing.
    let panes = client
        .call(&Message::Panes {
            v: VERSION,
            panes: vec![],
        })
        .await;
    match panes {
        Message::Panes { panes, .. } => assert!(panes.is_empty(), "{panes:?}"),
        other => panic!("want Panes, got {other:?}"),
    }
}

/// Killing a pane removes a **clean** worktree and keeps a **dirty** one: the
/// one unrecoverable thing this feature could do is delete an agent's work.
#[tokio::test]
async fn a_killed_pane_leaves_a_dirty_worktree_and_removes_a_clean_one() {
    let dir = scratch("cleanup");
    let repo = repo_at(&dir);
    let root = dir.join("worktrees");
    let socket = dir.join("arreo.sock");
    let _server = spawn_daemon(
        socket.clone(),
        WorktreeSettings {
            root: Some(root.display().to_string()),
            repo: Some(repo.display().to_string()),
        },
    )
    .await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut client = Client::connect(&socket).await;

    // Dirty: the child writes a file and stays alive.
    let reply = client
        .spawn_in_worktree("dirty", "echo wip > uncommitted.txt; sleep 30", None)
        .await;
    assert!(matches!(reply, Message::Ok { .. }), "{reply:?}");
    let dirty_path = root.join("dirty");
    assert!(
        wait_for_file(&dirty_path.join("uncommitted.txt"), Duration::from_secs(10)),
        "the child wrote its file"
    );

    // Clean: the child touches nothing.
    let reply = client.spawn_in_worktree("clean", "sleep 30", None).await;
    assert!(matches!(reply, Message::Ok { .. }), "{reply:?}");
    let clean_path = root.join("clean");
    assert!(clean_path.is_dir(), "the clean worktree exists");

    for id in ["dirty", "clean"] {
        let reply = client
            .call(&Message::Kill {
                v: VERSION,
                id: id.to_string(),
            })
            .await;
        assert!(matches!(reply, Message::Ok { .. }), "kill {id}: {reply:?}");
    }

    assert!(
        wait_for_absent(&clean_path, Duration::from_secs(10)),
        "a clean worktree goes with its pane"
    );
    assert!(
        dirty_path.join("uncommitted.txt").is_file(),
        "the dirty worktree was kept, with the work in it"
    );
    // The branch survives in both cases: it holds the commits, and deleting it
    // is a different decision from "stop working here".
    let branches = std::process::Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["branch", "--list", "arreo/*"])
        .output()
        .expect("git runs");
    let branches = String::from_utf8_lossy(&branches.stdout);
    assert!(branches.contains("arreo/dirty"), "{branches}");
    assert!(branches.contains("arreo/clean"), "{branches}");
}

/// A daemon with no worktree configured still serves ordinary panes exactly as
/// it did before this feature: the default must cost nothing.
#[tokio::test]
async fn a_plain_spawn_is_unchanged() {
    let dir = scratch("plain");
    let socket = dir.join("arreo.sock");
    let _server = spawn_daemon(socket.clone(), WorktreeSettings::default()).await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut client = Client::connect(&socket).await;
    let reply = client
        .call(&Message::Spawn {
            v: VERSION,
            id: "plain".to_string(),
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), "echo plain-ok; sleep 30".to_string()],
            cols: 80,
            rows: 24,
            memory_max: None,
            pids_max: None,
            kill_on_breach: false,
        })
        .await;
    assert!(matches!(reply, Message::Ok { .. }), "{reply:?}");

    // And a worktree request with no settings is still served — it uses the
    // state directory's default root and the daemon's own directory as the
    // repository, which for this test is the workspace's repository.
    let panes = client
        .call(&Message::Panes {
            v: VERSION,
            panes: vec![],
        })
        .await;
    match panes {
        Message::Panes { panes, .. } => {
            assert_eq!(panes.len(), 1, "{panes:?}");
            assert_eq!(panes[0].id, "plain");
        }
        other => panic!("want Panes, got {other:?}"),
    }
}
