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

/// Wait for `path` to exist **with the expected contents**, bounded.
///
/// Waiting for existence alone is not enough, and the difference is a flake
/// rather than a nicety: the child's shell creates the file when it applies the
/// redirection and fills it a moment later, so a poll that returns on `is_file()`
/// can read an empty file and compare it against the worktree path. That is a
/// check sampling a fact that is still moving — the same class as T-0088 — and it
/// showed up as one failure in ~6 full workspace runs before this was fixed.
fn wait_for_contents(path: &Path, expected: &str, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if text.trim() == expected {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
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
        let expected = std::fs::canonicalize(root.join(id))
            .expect("canonical")
            .display()
            .to_string();
        assert!(
            wait_for_contents(&where_file, &expected, Duration::from_secs(10)),
            "pane {id} never wrote {expected} into {}",
            where_file.display()
        );
    }

    // The same filename in both checkouts, with different contents — the
    // collision a shared working directory loses. Also content-waited: the child
    // writes these after the `pwd` above.
    assert!(
        wait_for_contents(
            &root.join("one").join("task.txt"),
            "first",
            Duration::from_secs(10)
        ),
        "pane one never finished writing task.txt"
    );
    assert!(
        wait_for_contents(
            &root.join("two").join("task.txt"),
            "second",
            Duration::from_secs(10)
        ),
        "pane two never finished writing task.txt"
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
    // Content, not mere existence: the shell creates the file when it applies
    // the redirection and writes a moment later, and "the work is still there"
    // is the claim this test makes — an empty file would not support it.
    assert!(
        wait_for_contents(
            &dirty_path.join("uncommitted.txt"),
            "wip",
            Duration::from_secs(10)
        ),
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
    assert_eq!(
        std::fs::read_to_string(dirty_path.join("uncommitted.txt"))
            .expect("the dirty worktree was kept")
            .trim(),
        "wip",
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

// ---------------------------------------------------------------------------
// T-0107: a recorded worktree path is data, not a permission
// ---------------------------------------------------------------------------

/// A guard around a real `arreo-server` **child process**, so the boot path (the
/// one that restores panes) runs exactly as it does for an operator — including
/// `--config`, which is how the configured root reaches it.
struct DaemonChild {
    child: std::process::Child,
    log: PathBuf,
}

impl DaemonChild {
    /// Start the daemon on `socket` with `config`, its stderr captured to
    /// `log` so the operator-visible refusal can be read after the fact.
    ///
    /// Waits, bounded, until the socket answers — which is *after* the boot
    /// restore, so a returned guard means the code under test has already run.
    /// A child that exits first is a failure with its own log attached.
    fn start(socket: &Path, config: &Path, log: &Path) -> Self {
        let out = std::fs::File::create(log).expect("log file");
        let err = out.try_clone().expect("log file");
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_arreo-server"))
            .arg("--socket")
            .arg(socket)
            .arg("--config")
            .arg(config)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(out))
            .stderr(std::process::Stdio::from(err))
            .spawn()
            .expect("arreo-server starts");
        match wait_serving(&mut child, socket) {
            Ok(()) => Self {
                child,
                log: log.to_path_buf(),
            },
            Err(why) => {
                // **Reaped before the panic**, both halves: a failing test that
                // leaves a daemon running on a fixed socket path makes the *next*
                // run fail for a reason that has nothing to do with the code —
                // which is the failure mode this whole repository keeps naming.
                let text = std::fs::read_to_string(log).unwrap_or_default();
                let _ = child.kill();
                let _ = child.wait();
                panic!("{why}: {text}");
            }
        }
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

/// Wait, bounded, until `socket` answers, or say why the daemon will not serve.
///
/// The socket answering is *after* the boot restore, so a returned `Ok` means the
/// code under test has already run. The child is borrowed, never dropped, so the
/// caller owns reaping it on both paths.
fn wait_serving(child: &mut std::process::Child, socket: &Path) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return Ok(());
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("the daemon exited before serving ({status})"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("the daemon never bound {}", socket.display()))
}

impl Drop for DaemonChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Write one pane record with the given worktree into a fresh store.
///
/// The store is exactly what the reviewer used as the attack surface: a file the
/// operator's own uid can edit. Writing the row directly is therefore the honest
/// way to test it — no daemon is needed to *produce* a hostile row, only to
/// refuse one.
fn store_with_row(db: &Path, id: &str, worktree: Option<&str>, body: &str) {
    let store = arreo_core::store::SessionStore::open(db).expect("store opens");
    store
        .save_topology(&[arreo_core::store::StoredPane {
            id: id.to_string(),
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), body.to_string()],
            cols: 80,
            rows: 24,
            scrollback: Vec::new(),
            harness: None,
            session_id: None,
            worktree: worktree.map(str::to_string),
        }])
        .expect("row written");
}

/// **A row naming a path outside the configured root creates nothing** (T-0107).
///
/// The reproduced defect: `restore_worktree_dir` took the root *from the record*,
/// and `ensure` then did `create_dir_all(root)` + `git worktree add` — so a store
/// row made the daemon create a directory anywhere it could write and register it
/// as a real worktree. The row here names a path that does not exist, which is
/// the case that *creates* rather than merely enters: the assertion is that boot
/// leaves the filesystem alone.
#[test]
fn a_record_outside_the_root_creates_nothing_and_skips_the_pane() {
    let dir = t0107_scratch("outside");
    let repo = repo_at(&dir);
    let root = dir.join("cfgroot");
    let config = t0107_config(&dir, &root, &repo);
    let socket = dir.join("arreo.sock");
    let db = arreo_server::persist::db_path_for(&socket);
    std::fs::create_dir_all(dir.join("state")).expect("state");

    // A row that asks for a directory three levels deep outside the root, and a
    // program that would leave a trace if it ever ran.
    let outside = dir.join("outside/nested/x");
    let ran = dir.join("ran-outside.txt");
    store_with_row(
        &db,
        "esc",
        Some(&outside.display().to_string()),
        &format!("echo ran > {}", ran.display()),
    );

    let log = dir.join("daemon.log");
    let daemon = DaemonChild::start(&socket, &config, &log);
    // A settle window: the restore runs during boot, before the socket answers,
    // so anything it was going to create already exists by now.
    std::thread::sleep(Duration::from_secs(1));

    assert!(
        !dir.join("outside").exists(),
        "the daemon created a directory outside the configured root: {:?}",
        std::fs::read_dir(&dir).map(|d| d
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect::<Vec<_>>())
    );
    assert!(!outside.exists(), "the recorded path itself must not exist");
    assert!(
        !ran.exists(),
        "the pane was restored and its program ran — it must be skipped"
    );
    // And git knows nothing about it: no worktree was registered.
    //
    // Asserted on the **branch**, not on a path substring: an Arreo worktree is
    // always on `arreo/<pane>`, while the repository's own path can contain any
    // word the test happens to use for its scratch directory (this one did).
    assert!(
        !git_worktree_list(&repo).contains("arreo/"),
        "a worktree was registered: {}",
        git_worktree_list(&repo)
    );

    // The operator is told, with both facts they need to decide.
    let log = daemon.log();
    assert!(
        log.contains("outside/nested/x"),
        "the log names the path: {log}"
    );
    assert!(
        log.contains(&root.display().to_string()),
        "the log names the configured root: {log}"
    );
    assert!(log.contains("esc"), "the log names the pane: {log}");
}

/// **A row naming the main checkout is refused, never used as a cwd** (T-0107).
///
/// The second reproduced case: a record naming the repository root restored the
/// pane *in the shared tree* — the isolation silently off, which is the collision
/// worktree-per-task exists to prevent. The program writes a **relative** path, so
/// the file's absence from the repository is direct evidence about the cwd.
#[test]
fn a_record_naming_the_main_checkout_is_refused() {
    let dir = t0107_scratch("maincheckout");
    let repo = repo_at(&dir);
    let root = dir.join("cfgroot");
    let config = t0107_config(&dir, &root, &repo);
    let socket = dir.join("arreo.sock");
    let db = arreo_server::persist::db_path_for(&socket);
    std::fs::create_dir_all(dir.join("state")).expect("state");

    store_with_row(
        &db,
        "shared",
        Some(&repo.display().to_string()),
        "echo ran > agent-was-here.txt",
    );

    let log = dir.join("daemon.log");
    let daemon = DaemonChild::start(&socket, &config, &log);
    std::thread::sleep(Duration::from_secs(1));

    assert!(
        !repo.join("agent-was-here.txt").exists(),
        "the agent ran in the main checkout — the isolation was off"
    );
    assert!(
        !root.join("shared/agent-was-here.txt").exists(),
        "the record was re-made under the configured root instead of being refused"
    );
    let log = daemon.log();
    assert!(log.contains("shared"), "the log names the pane: {log}");
    assert!(
        log.contains(&repo.display().to_string()),
        "the log names the recorded path: {log}"
    );
}

/// A scratch root for a T-0107 test, wiped first.
fn t0107_scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-scratch/T-0107")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    std::fs::canonicalize(&dir).expect("canonical")
}

/// The configuration file the daemon child is given: the only place the root the
/// check compares against is written.
fn t0107_config(dir: &Path, root: &Path, repo: &Path) -> PathBuf {
    let config = dir.join("arreo.toml");
    std::fs::write(
        &config,
        format!(
            "[worktree]\nroot = \"{}\"\nrepo = \"{}\"\n",
            root.display(),
            repo.display()
        ),
    )
    .expect("config written");
    config
}

fn git_worktree_list(repo: &Path) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .expect("git runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}
