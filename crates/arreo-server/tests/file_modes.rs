//! T-0078: every file the daemon creates is owner-only (0600), and the leak
//! the policy closes is real — a pane's output is readable out of the store.
//!
//! ## Why the daemon runs under `umask 0002`
//!
//! A security property must not depend on the operator's umask. The pre-fix
//! daemon never chmodded anything, so its files carried the ambient umask —
//! 0644/0664/0775 under the common 002/022 umasks, which is exactly the leak
//! T-0078 exists to close. Running the daemon under `umask 0002` reproduces
//! the task's measured conditions (socket 775, db 644, wal 644, shm 644, lock
//! 664) so this test fails before the fix no matter what umask the test runner
//! itself inherited.
//!
//! ## What each assertion proves
//!
//! - The pane's token is genuinely in the store files — the leak is *content*
//!   (the operator's prompts, whatever a tool printed), not an abstract mode.
//! - The socket, the store and the lock are the daemon's own persistent files:
//!   created under the loose umask, they must be 0600 anyway.
//! - The `-wal`/`-shm` sidecars are recreated by SQLite (and deleted when the
//!   last connection closes), so the honest assertion is the re-application:
//!   loosen them to the pre-fix 0644, trigger the daemon's next store open,
//!   and require the open to tighten them back.

use arreo_core::proto::{codec, Message, VERSION};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The daemon binary, beside this test's own executable.
fn server_binary() -> PathBuf {
    std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("deps dir")
        .parent()
        .expect("debug dir")
        .join("arreo-server")
}

fn temp_socket(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "arreo-modes-{name}-{}-{:?}.sock",
        std::process::id(),
        std::thread::current().id()
    ))
}

/// A child that is killed when the test ends, so a failing assertion cannot
/// leave a daemon running.
struct Guard(std::process::Child);

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start the daemon under `umask 0002` — the leak is reproduced, not assumed.
fn spawn_server_daemon(socket: &Path) -> std::process::Child {
    std::process::Command::new("sh")
        .arg("-c")
        .arg("umask 0002; exec \"$0\" \"$@\"")
        .arg(server_binary())
        .arg("--socket")
        .arg(socket)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("arreo-server runs")
}

/// Wait for the daemon to accept connections.
fn wait_for_serve(socket: &Path, child: &mut std::process::Child) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(socket).is_err() {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("daemon exited instead of serving: {status}");
        }
        assert!(std::time::Instant::now() < deadline, "daemon never bound");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Hello→Welcome, then one request, then one reply frame.
fn raw_request(socket: &Path, message: &Message) -> Message {
    let mut stream = UnixStream::connect(socket).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let hello = Message::Hello {
        v: VERSION,
        client: "file-modes-test".to_string(),
        wants: vec![VERSION],
    };
    stream
        .write_all(&codec::encode_frame(&hello).expect("hello"))
        .expect("write");
    stream.flush().expect("flush");
    let mut acc = Vec::new();
    let mut chunk = [0u8; 8192];
    let welcome = loop {
        let n: usize = stream.read(&mut chunk).expect("read");
        assert!(n > 0, "handshake closed");
        acc.extend_from_slice(&chunk[..n]);
        if let Ok((message, consumed)) = codec::decode_frame(&acc) {
            acc.drain(..consumed);
            break message;
        }
    };
    assert!(
        matches!(welcome, Message::Welcome { .. }),
        "handshake: {welcome:?}"
    );
    stream
        .write_all(&codec::encode_frame(message).expect("encode"))
        .expect("write");
    stream.flush().expect("flush");
    loop {
        let n: usize = stream.read(&mut chunk).expect("read");
        assert!(n > 0, "reply closed");
        acc.extend_from_slice(&chunk[..n]);
        if let Ok((message, _)) = codec::decode_frame(&acc) {
            return message;
        }
    }
}

fn spawn_pane(socket: &Path, id: &str, command: &str) {
    let reply = raw_request(
        socket,
        &Message::Spawn {
            v: VERSION,
            id: id.to_string(),
            program: "/bin/sh".to_string(),
            args: vec!["-c".to_string(), command.to_string()],
            cols: 80,
            rows: 24,
            memory_max: None,
            pids_max: None,
            kill_on_breach: false,
        },
    );
    assert!(
        matches!(reply, Message::Ok { .. }),
        "spawn {id:?}: {reply:?}"
    );
}

/// Whether `path`'s raw bytes contain `needle` — pane output is stored by
/// SQLite as verbatim UTF-8, so a substring match *is* reading the pane's
/// output out of the file (the task's `sqlite3 … 'select * from panes'`
/// equivalent).
fn file_contains(path: &Path, needle: &[u8]) -> bool {
    if !path.exists() {
        return false;
    }
    let bytes = std::fs::read(path).expect("read store file");
    bytes.windows(needle.len()).any(|window| window == needle)
}

/// Wait until `check` holds, or panic after `limit`.
fn wait_until(what: &str, limit: Duration, check: impl Fn() -> bool) {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        if check() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    let what = format!("stat {}", path.display());
    std::fs::metadata(path).expect(&what).permissions().mode() & 0o777
}

/// Assert one file is exactly 0600, naming the leak when it is not.
fn assert_owner_only(path: &Path, what: &str, token: &str) {
    let mode = mode_of(path);
    assert_eq!(
        mode,
        0o600,
        "{what} {path:?} is {mode:o}, must be 0600 — {}",
        if mode & 0o077 != 0 {
            format!("it holds the pane's output {token:?}, so every local user could read it")
        } else {
            "the policy requires owner-only".to_string()
        }
    );
}

#[test]
fn every_file_the_daemon_creates_is_owner_only() {
    let socket = temp_socket("modes");
    let db = arreo_server::persist::db_path_for(&socket);
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    let shm = PathBuf::from(format!("{}-shm", db.display()));
    let lock = arreo_server::persist::lock_path_for(&socket);
    let token = format!("ARREO-MODE-TOKEN-{}", std::process::id());

    let mut server = Guard(spawn_server_daemon(&socket));
    wait_for_serve(&socket, &mut server.0);

    // A pane prints the token; the second spawn snapshots the registry, which
    // drains the first pane's scrollback into the store — so the token is
    // persisted as real pane content.
    spawn_pane(&socket, "first", &format!("echo {token}; sleep 5"));
    std::thread::sleep(Duration::from_millis(1200));
    spawn_pane(&socket, "second", "sleep 5");

    // The leak, proven: the pane's output is genuinely readable out of the
    // store files. (Before T-0078 they sat at 0644 under this umask, so any
    // local user could read the operator's prompts.) The snapshot is a
    // background task, so poll for the bytes.
    wait_until(
        &format!("pane output in the store ({token:?})"),
        Duration::from_secs(10),
        || file_contains(&db, token.as_bytes()) || file_contains(&wal, token.as_bytes()),
    );
    assert!(
        file_contains(&db, token.as_bytes()) || file_contains(&wal, token.as_bytes()),
        "the pane's output must be readable out of the store files — otherwise \
         the mode assertions prove nothing about the leak"
    );

    // The daemon's own persistent files, created under the loose umask: the
    // socket (the gate in front of every verb and the handoff request), the
    // store, and the lock.
    assert_owner_only(&socket, "socket", &token);
    assert_owner_only(&db, "store", &token);
    assert_owner_only(&lock, "lock", &token);

    // The `-wal`/`-shm` sidecars: SQLite recreates them whenever a connection
    // opens the store and deletes them when the last one closes, so the
    // honest assertion is the re-application. Hold a store open (as a daemon
    // background op does), loosen the sidecars to the pre-fix 0644, and
    // trigger the daemon's next store open (any Kill snapshots the registry,
    // which opens the store): the open must tighten them back to owner-only.
    let held = arreo_core::store::SessionStore::open(&db).expect("hold store open");
    {
        use std::os::unix::fs::PermissionsExt;
        for file in [&wal, &shm] {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644))
                .expect("loosen sidecar");
        }
    }
    raw_request(
        &socket,
        &Message::Kill {
            v: VERSION,
            id: "ghost".to_string(),
        },
    );
    wait_until(
        "the daemon's next open to re-apply owner-only to the sidecars",
        Duration::from_secs(10),
        || mode_of(&wal) == 0o600 && mode_of(&shm) == 0o600,
    );
    assert_owner_only(&wal, "-wal", &token);
    assert_owner_only(&shm, "-shm", &token);
    drop(held);

    // Cleanup: kill the panes (the daemon never kills agents on its way out),
    // then the daemon itself via the guard.
    let _ = raw_request(
        &socket,
        &Message::Kill {
            v: VERSION,
            id: "first".to_string(),
        },
    );
    let _ = raw_request(
        &socket,
        &Message::Kill {
            v: VERSION,
            id: "second".to_string(),
        },
    );
    std::thread::sleep(Duration::from_millis(300));
}
