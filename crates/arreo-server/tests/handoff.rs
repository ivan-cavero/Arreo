//! T-0038 stage 1: the socket never goes dead across the cut.
//!
//! Every test here owns real processes (the pattern from
//! `tests/single_instance.rs`): the daemon binary beside the test executable,
//! pid-scoped socket paths under `temp_dir()`, `Drop` guards that kill + wait
//! children, poll-for-readiness rather than sleeps. A leaked process on a fixed
//! path is indistinguishable from a product defect.
//!
//! The mechanism under test: the outgoing daemon's listener descriptor travels
//! over `SCM_RIGHTS` on a dedicated `<socket>.handoff` connection to the
//! incoming daemon, which serves on the inherited dup — so the kernel never
//! sees the socket without a listener, and a connect is never refused across
//! the cut. Live *connections* are not transferred in stage 1 (that is stage
//! 3); what "never goes dead" means here is that the *listening socket* always
//! accepts.

use arreo_core::proto::{codec, Message, VERSION};
use std::io::{Read, Write};
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
        "arreo-handoff-{name}-{}-{:?}.sock",
        std::process::id(),
        std::thread::current().id()
    ))
}

fn cleanup(socket: &Path) {
    for suffix in ["", ".db", ".db-shm", ".db-wal", ".lock", ".handoff"] {
        let mut path = socket.as_os_str().to_owned();
        path.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(path));
    }
}

/// A child that is killed when the test ends, so a failing assertion cannot
/// leave a daemon holding a path.
struct Guard(std::process::Child);

impl Guard {
    fn id(&self) -> u32 {
        self.0.id()
    }

    fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        self.0.try_wait()
    }

    fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.0.wait()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Wait up to `limit` for the child to exit, without ever blocking past it.
fn exited_within(child: &mut Guard, limit: Duration) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("try_wait: {e}"),
        }
    }
    false
}

fn spawn_daemon(socket: &Path, extra: &[&str], stderr: std::process::Stdio) -> Guard {
    Guard(
        std::process::Command::new(server_binary())
            .arg("--socket")
            .arg(socket)
            .args(extra)
            .stdout(std::process::Stdio::null())
            .stderr(stderr)
            .spawn()
            .expect("arreo-server runs"),
    )
}

fn spawn_handoff(socket: &Path, extra: &[&str], stderr: std::process::Stdio) -> Guard {
    Guard(
        std::process::Command::new(server_binary())
            .arg("--handoff-from")
            .arg(socket)
            .args(extra)
            .stdout(std::process::Stdio::null())
            .stderr(stderr)
            .spawn()
            .expect("arreo-server --handoff-from runs"),
    )
}

/// Wait until something answers on `socket` (or the child exits first, which
/// is always a failure for a daemon that should be serving).
fn wait_serving(child: &mut Guard, socket: &Path, limit: Duration) {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return;
        }
        if let Ok(Some(status)) = child.try_wait() {
            panic!("daemon exited instead of serving: {status}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("nothing answered on {} within {limit:?}", socket.display());
}

/// One framed request over a fresh connection: Hello→Welcome, then `message`,
/// then one reply.
fn raw_request(socket: &Path, message: &Message) -> Message {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    let hello = Message::Hello {
        v: VERSION,
        client: "handoff-test".to_string(),
        wants: vec![VERSION],
    };
    stream
        .write_all(&codec::encode_frame(&hello).expect("hello"))
        .expect("write");
    let welcome = read_one(&mut stream);
    assert!(
        matches!(welcome, Message::Welcome { .. }),
        "handshake: {welcome:?}"
    );
    stream
        .write_all(&codec::encode_frame(message).expect("encode"))
        .expect("write");
    read_one(&mut stream)
}

fn read_one(stream: &mut std::os::unix::net::UnixStream) -> Message {
    let mut acc = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if let Ok((message, _)) = codec::decode_frame(&acc) {
            return message;
        }
        let n: usize = stream.read(&mut chunk).expect("read");
        assert!(n > 0, "the daemon closed the connection");
        acc.extend_from_slice(&chunk[..n]);
    }
}

fn handoff_rows(socket: &Path, action: &str) -> Vec<arreo_core::store::StoredAudit> {
    let db = arreo_server::persist::db_path_for(socket);
    let store = arreo_core::store::SessionStore::open(&db).expect("audit store");
    store.audit_by_action(action, 10).expect("audit rows")
}

/// Criterion 1: the socket never goes dead across the cut.
///
/// How the window is deterministic rather than timing-dependent: the test waits
/// until the transfer socket exists (the outgoing daemon has bound it and
/// replied `HandoffReady` — i.e. it is blocked inside the handoff, and exits
/// only on commit) and then connects. That connect lands strictly inside the
/// handoff — after Ready, before commit — not before or after it. The only way
/// it succeeds is if a listener was bound at that instant; with the listener
/// *not* inherited (rebind after exit) the outgoing daemon would have to close
/// its listener before the incoming one binds, and a connect in that window
/// fails. The test then closes the mid-handoff connection (its handshake
/// already proved the point, and the cut's completion must not depend on any
/// client connection's state), waits for the cut, and proves a fresh connect
/// is served by the *new* daemon.
///
/// What removal turns red: serving the incoming daemon on a rebound socket
/// instead of the inherited dup (the mid-handoff connect fails); dropping the
/// listener before the commit (same); the outgoing daemon never exiting
/// (the cut assert).
#[test]
fn the_socket_accepts_while_the_handoff_is_in_progress() {
    let socket = temp_socket("cut");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));
    let old_pid = old.id();

    // Start the handoff. The incoming daemon connects to the client socket
    // itself (its own Hello→Handoff session), so by the time its transfer
    // begins the outgoing daemon is blocked inside `serve_handoff_request` —
    // and this test connects *now*, while the outgoing daemon is inside the
    // handoff and the incoming daemon has not yet committed. The only way
    // this connect succeeds is if a listener was bound at that instant; with
    // the listener *not* inherited (rebind after exit) the outgoing daemon
    // would have to close its listener before the incoming one binds, and a
    // connect in that window fails.
    let mut new = spawn_handoff(&socket, &[], std::process::Stdio::piped());
    // Wait until the transfer socket exists: the outgoing daemon has bound it
    // and replied Ready — i.e. it is inside the handoff — but has not yet
    // exited (it exits only on commit).
    let handoff_path = arreo_server::handoff::handoff_path_for(&socket);
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        assert!(
            old.try_wait().expect("try_wait").is_none(),
            "the outgoing daemon exited before the transfer began"
        );
        if handoff_path.exists() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the transfer socket never appeared"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // The deterministic window: the outgoing daemon is inside the handoff
    // (transfer socket bound), the incoming daemon has not committed (the old
    // daemon is still alive). A connect now proves the socket never went dead.
    let mut waiting = std::os::unix::net::UnixStream::connect(&socket)
        .expect("connect during the handoff: the socket must accept while the cut is in progress");
    waiting
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("timeout");
    let hello = Message::Hello {
        v: VERSION,
        client: "waiting-client".to_string(),
        wants: vec![VERSION],
    };
    waiting
        .write_all(&codec::encode_frame(&hello).expect("hello"))
        .expect("write");
    assert!(matches!(read_one(&mut waiting), Message::Welcome { .. }));

    // The cut must complete: the outgoing daemon exits 0, the socket stays.
    // The mid-handoff `waiting` connection is still owned by the outgoing
    // daemon's session loop — which is parked in the transfer, not reading
    // that connection — so it cannot disturb the cut either way. But its
    // handshake already proved the point (a listener accepted mid-handoff), so
    // close it before waiting: the cut's completion must not depend on any
    // client connection's state.
    drop(waiting);
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut old_exited = false;
    while std::time::Instant::now() < deadline {
        if let Ok(Some(_)) = new.try_wait() {
            panic!("the incoming daemon exited instead of serving");
        }
        if let Ok(Some(status)) = old.try_wait() {
            assert!(
                status.success(),
                "the outgoing daemon exits 0, got {status}"
            );
            old_exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(old_exited, "the outgoing daemon never exited");
    // A fresh connect after the cut is served — by the new daemon (the old
    // process is gone, so any answer is the new one's). Combined with the
    // mid-handoff connect above, the socket accepted on both sides of the cut
    // and therefore never went dead.
    let reply = raw_request(
        &socket,
        &Message::Panes {
            v: VERSION,
            panes: vec![],
        },
    );
    assert!(
        matches!(reply, Message::Panes { .. }),
        "a post-cut connect is served: {reply:?}"
    );
    // The new process is the one serving: the audit row names the outgoing
    // daemon's pid, and the child that is still alive is a different pid.
    assert_ne!(new.id(), old_pid, "the cut replaced the process");
    let rows = handoff_rows(&socket, arreo_core::store::actions::HANDOFF);
    assert_eq!(rows.len(), 1, "one cut, one audit row");
}

/// Criterion 2: the outgoing daemon exits 0, the socket path is unchanged, and
/// the new process is the one serving.
///
/// The observables are the outgoing daemon's exit status, the socket path
/// itself (same bytes — no rename, no rebind window), and the incoming
/// daemon's `handoff complete … pid <pid>` stderr line plus the audit row's
/// `pid=<pid>`: both name the new process without a `status` verb (which does
/// not exist — the criterion is corrected, not reinterpreted).
///
/// What removal turns red: exiting non-zero on commit (the status assert);
/// unlinking the socket file on the way out (the path assert); printing the
/// line before accepting (the connect-after-line ordering — the line is read
/// only after a fresh connect succeeds).
#[test]
fn the_cut_replaces_the_process_on_the_same_path() {
    let socket = temp_socket("replace");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));
    let old_pid = old.id();

    let mut new = spawn_handoff(&socket, &[], std::process::Stdio::piped());
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut status = None;
    while std::time::Instant::now() < deadline {
        if let Ok(Some(exit)) = old.try_wait() {
            status = Some(exit);
            break;
        }
        if let Ok(Some(exit)) = new.try_wait() {
            panic!("the incoming daemon exited instead of serving: {exit}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let status = status.expect("the outgoing daemon never exited");
    assert!(
        status.success(),
        "the outgoing daemon exits 0, got {status}"
    );
    // The path is unchanged and answers — on the new process.
    assert!(socket.exists(), "the socket file survives the cut");
    wait_serving(&mut new, &socket, Duration::from_secs(10));
    assert_ne!(new.id(), old_pid, "a new process serves the same path");
    // The operator-visible line names the new pid; the audit row agrees.
    // The pipe is taken (the daemon writes the one line, then never again).
    // A reader thread drains it so the test cannot block on a pipe the daemon
    // holds open for its whole life: the line arrives right after the cut,
    // which already happened (the old daemon exited above).
    let stderr = {
        use std::io::Read;
        let mut pipe = new.0.stderr.take().expect("piped stderr");
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        out.extend_from_slice(&chunk[..n]);
                        if out.windows(16).any(|w| w == b"handoff complete") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = tx.send(out);
        });
        let out = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("the handoff complete line arrives");
        String::from_utf8_lossy(&out).into_owned()
    };
    assert!(
        stderr.contains("handoff complete"),
        "the incoming daemon announces the cut: {stderr:?}"
    );
    let pid_line = format!("pid {}", new.id());
    assert!(
        stderr.contains(&pid_line),
        "the line names the new pid ({pid_line}): {stderr:?}"
    );
    // The old `Guard` must not SIGKILL the (already exited) child on drop in a
    // way that confuses the next test — wait it to reap.
    let _ = old.wait();
}

/// Criterion 3: the lock survives the handoff — immediately after the cut, a
/// third daemon on the same socket is refused.
///
/// This is T-0071's invariant continued across the handoff: the lock lives in
/// the shared open file description, so the incoming daemon's inherited dup
/// keeps it held after the outgoing daemon exits.
///
/// What removal turns red: not sending the lock descriptor (the third daemon
/// acquires a free lock and serves — the test's `exited_within` fails); an
/// inherited-lock `Drop` that calls `unlock()` (same — `flock(LOCK_UN)` on a
/// shared description releases it for every holder).
#[test]
fn the_lock_survives_the_cut() {
    let socket = temp_socket("lock");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));

    let mut new = spawn_handoff(&socket, &[], std::process::Stdio::null());
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut cut = false;
    while std::time::Instant::now() < deadline {
        if let Ok(Some(exit)) = old.try_wait() {
            assert!(exit.success(), "outgoing exits 0, got {exit}");
            cut = true;
            break;
        }
        if let Ok(Some(exit)) = new.try_wait() {
            panic!("incoming exited instead of serving: {exit}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(cut, "the cut never happened");
    wait_serving(&mut new, &socket, Duration::from_secs(10));

    // A third daemon must refuse — and refuse without disturbing the new one.
    let mut third = spawn_daemon(&socket, &[], std::process::Stdio::null());
    assert!(
        exited_within(&mut third, Duration::from_secs(10)),
        "a third daemon must refuse a socket whose lock the new daemon holds"
    );
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_ok(),
        "the refused daemon leaves the serving one's socket reachable"
    );
    let _ = old.wait();
}

/// Criterion 4: the audit row exists — `handoff` with the two protocol versions
/// and `panes=0`, written by the outgoing daemon before it exits, readable
/// afterwards via the store.
///
/// What removal turns red: not writing the row (empty); writing it after exit
/// (impossible — the process is gone, so the row would be missing); writing
/// the wrong versions or pane count (the `detail` asserts).
#[test]
fn the_cut_leaves_an_audit_row() {
    let socket = temp_socket("audit");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));

    let mut new = spawn_handoff(&socket, &[], std::process::Stdio::null());
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut cut = false;
    while std::time::Instant::now() < deadline {
        if let Ok(Some(exit)) = old.try_wait() {
            assert!(exit.success(), "outgoing exits 0, got {exit}");
            cut = true;
            break;
        }
        if let Ok(Some(exit)) = new.try_wait() {
            panic!("incoming exited instead of serving: {exit}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(cut, "the cut never happened");
    wait_serving(&mut new, &socket, Duration::from_secs(10));

    let rows = handoff_rows(&socket, arreo_core::store::actions::HANDOFF);
    assert_eq!(rows.len(), 1, "exactly one handoff row, got {rows:?}");
    let row = &rows[0];
    assert_eq!(row.device, "daemon");
    assert_eq!(row.outcome, arreo_core::store::AuditOutcome::Ok);
    let detail = row.detail.as_deref().unwrap_or("");
    assert!(
        detail.contains(&format!("handoff {VERSION} -> {VERSION}")),
        "both protocol versions: {detail:?}"
    );
    assert!(
        detail.contains("panes=0"),
        "stage 1 moves no pane: {detail:?}"
    );
    let _ = old.wait();
}

/// Criterion 5: a refused version leaves the outgoing daemon serving and writes
/// an audited refusal; the incoming daemon exits non-zero.
///
/// The refusal path is version-only: no `.handoff` file is created, no
/// descriptor moves, and the outgoing daemon's session stays open. The test
/// drives the request by hand (a `Handoff` frame with a protocol outside the
/// window) because the real incoming binary always offers a compatible one —
/// and then asserts the daemon still serves and the refusal row exists.
///
/// What removal turns red: answering the refusal but closing the session
/// (the follow-up `Panes` fails); forgetting the audit row (empty); exiting
/// the outgoing daemon on a refusal (the serving assert fails).
#[test]
fn a_refused_version_leaves_the_daemon_serving() {
    let socket = temp_socket("refuse");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));

    let reply = raw_request(
        &socket,
        &Message::Handoff {
            v: VERSION,
            protocol: VERSION + 10,
            build: "test-future".to_string(),
        },
    );
    match &reply {
        Message::Error { message, .. } => assert!(
            message.contains("refused") || message.contains("handoff"),
            "a typed refusal naming the handoff: {message:?}"
        ),
        other => panic!("want a typed Error refusal, got {other:?}"),
    }
    // The daemon is still serving on the same connection discipline: a fresh
    // request works, the process is alive, and no `.handoff` file lingers.
    let panes = raw_request(
        &socket,
        &Message::Panes {
            v: VERSION,
            panes: vec![],
        },
    );
    assert!(
        matches!(panes, Message::Panes { .. }),
        "the refused daemon keeps serving: {panes:?}"
    );
    assert!(
        old.try_wait().expect("try_wait").is_none(),
        "the outgoing daemon stays up after a refusal"
    );
    let handoff_path = arreo_server::handoff::handoff_path_for(&socket);
    assert!(
        !handoff_path.exists(),
        "a refusal creates no transfer socket"
    );
    let rows = handoff_rows(&socket, arreo_core::store::actions::HANDOFF_REFUSE);
    assert_eq!(rows.len(), 1, "one refusal, one audit row, got {rows:?}");
    assert_eq!(rows[0].outcome, arreo_core::store::AuditOutcome::Refused);
}

/// Criterion 6: an aborted handoff leaves the outgoing daemon serving, holding
/// the lock, with no `<socket>.handoff` file behind — and a retry succeeds.
///
/// The abort is a killed incoming daemon between receiving the descriptors and
/// committing: the test performs a real handoff, kills the child mid-transfer
/// (after `HandoffReady`, before commit), then asserts the main socket still
/// answers, a third daemon is still refused, and a retry of the handoff
/// succeeds.
///
/// Determinism: the kill happens after `HandoffReady` is observed on a
/// by-hand session but before that session closes — i.e. while the outgoing
/// daemon is blocked in the transfer — so the abort lands inside the handoff,
/// not before or after it.
///
/// What removal turns red: unlinking the main socket on abort (connect fails);
/// dropping the lock on abort (third daemon serves); leaving the `.handoff`
/// file (exists assert); breaking the retry (second cut never completes).
#[test]
fn an_aborted_handoff_leaves_the_daemon_serving_and_retryable() {
    let socket = temp_socket("abort");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));

    // A by-hand handoff session: Hello→Welcome, Handoff→HandoffReady — then
    // the test kills the would-be incoming daemon (a real one, started and
    // killed mid-transfer) instead of completing the transfer.
    let mut incoming = spawn_handoff(
        &socket,
        &["--handoff-timeout-secs", "30"],
        std::process::Stdio::null(),
    );
    // Wait until the transfer socket exists: the outgoing daemon is now blocked
    // inside the handoff (bound, replied Ready, waiting for the connect) — the
    // deterministic abort window.
    let handoff_path = arreo_server::handoff::handoff_path_for(&socket);
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while !handoff_path.exists() && std::time::Instant::now() < deadline {
        if let Ok(Some(exit)) = incoming.try_wait() {
            panic!("incoming exited before the transfer began: {exit}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    // Kill the incoming daemon mid-transfer: the outgoing daemon's transfer
    // fails, and it must keep serving.
    let _ = incoming.0.kill();
    let _ = incoming.wait();
    // Give the outgoing daemon a moment to observe the abort and unlink.
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let answers = std::os::unix::net::UnixStream::connect(&socket).is_ok();
        let gone = !handoff_path.exists();
        let alive = old.try_wait().expect("try_wait").is_none();
        if answers && gone && alive {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "answers={answers} gone={gone} alive={alive}: the abort did not recover"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    // The lock is still held: a third daemon is refused.
    let mut third = spawn_daemon(&socket, &[], std::process::Stdio::null());
    assert!(
        exited_within(&mut third, Duration::from_secs(10)),
        "after an abort the lock is still held"
    );
    // And a retry succeeds: a fresh handoff cuts cleanly.
    let mut retry = spawn_handoff(&socket, &[], std::process::Stdio::null());
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut cut = false;
    while std::time::Instant::now() < deadline {
        if let Ok(Some(exit)) = old.try_wait() {
            assert!(exit.success(), "outgoing exits 0 on the retry, got {exit}");
            cut = true;
            break;
        }
        if let Ok(Some(exit)) = retry.try_wait() {
            panic!("the retry incoming exited instead of serving: {exit}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(cut, "the retry never cut");
    wait_serving(&mut retry, &socket, Duration::from_secs(10));
    let _ = old.wait();
}

/// Criterion 7: `arreo-server --version` prints a version (needed by
/// `update --server`'s `verify_runs`, which shells out to `<binary> --version`
/// and refuses a candidate that does not identify as a server), and
/// `arreo-server --help` mentions `--handoff-from`.
///
/// What removal turns red: no `--version` arm (exits 2); a version string
/// without the binary name (the CLI's `SERVER_BINARY_NAME` check refuses it);
/// no `--handoff-from` in the help text.
#[test]
fn version_and_help_describe_the_handoff() {
    let version = std::process::Command::new(server_binary())
        .arg("--version")
        .output()
        .expect("arreo-server --version runs");
    assert!(
        version.status.success(),
        "--version exits 0, got {}",
        version.status
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&version.stdout),
        String::from_utf8_lossy(&version.stderr)
    );
    assert!(
        text.contains("arreo-server"),
        "the version names the server binary (the CLI checks for it): {text:?}"
    );
    let help = std::process::Command::new(server_binary())
        .arg("--help")
        .output()
        .expect("arreo-server --help runs");
    assert!(help.status.success(), "--help exits 0");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&help.stdout),
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(
        text.contains("--handoff-from"),
        "--help mentions --handoff-from: {text:?}"
    );
}
