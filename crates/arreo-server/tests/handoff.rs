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
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsFd;
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
    for suffix in [
        "",
        ".db",
        ".db-shm",
        ".db-wal",
        ".lock",
        ".handoff",
        ".handoff.lock",
    ] {
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
    read_one_soft(stream).unwrap_or_else(|e| panic!("read: {e}"))
}

/// `read_one`, but a broken connection is a value rather than a panic: a client
/// whose connection the outgoing daemon accepted at the instant of the cut is
/// dropped on purpose (the incoming daemon is serving), so the exclusivity test
/// retries instead of failing the run.
fn read_one_soft(stream: &mut std::os::unix::net::UnixStream) -> Result<Message, String> {
    let mut acc = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if let Ok((message, _)) = codec::decode_frame(&acc) {
            return Ok(message);
        }
        let n: usize = stream.read(&mut chunk).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("the daemon closed the connection".to_string());
        }
        acc.extend_from_slice(&chunk[..n]);
    }
}

/// One Hello→Welcome→request→reply round trip that reports failure rather than
/// panicking, for a client that must keep talking across a cut.
fn try_round_trip(socket: &Path, message: &Message) -> Result<Message, String> {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    let hello = Message::Hello {
        v: VERSION,
        client: "handoff-test".to_string(),
        wants: vec![VERSION],
    };
    stream
        .write_all(&codec::encode_frame(&hello).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    match read_one_soft(&mut stream)? {
        Message::Welcome { .. } => {}
        other => return Err(format!("no Welcome: {other:?}")),
    }
    stream
        .write_all(&codec::encode_frame(message).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    read_one_soft(&mut stream)
}

/// Poll `check` every 20 ms until it is true or `limit` elapses.
fn until(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + limit;
    loop {
        if check() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Everything the child wrote to stderr, read after it exited (so the pipe is
/// at end-of-file and this cannot block).
fn stderr_of(child: &mut Guard) -> String {
    let mut pipe = child.0.stderr.take().expect("piped stderr");
    let mut out = Vec::new();
    let _ = pipe.read_to_end(&mut out);
    String::from_utf8_lossy(&out).into_owned()
}

/// Accept one connection, waiting at most `limit` (a `UnixListener` has no
/// accept timeout of its own).
fn accept_within(
    listener: &std::os::unix::net::UnixListener,
    limit: Duration,
) -> std::os::unix::net::UnixStream {
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let deadline = std::time::Instant::now() + limit;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).expect("blocking stream");
                return stream;
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "no connection within {limit:?}"
                );
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("accept: {e}"),
        }
    }
}

/// A by-hand handoff request on the main socket: Hello→Welcome, `Handoff`, and
/// whatever the daemon answers. The returned stream must stay alive while the
/// daemon is inside the handoff — dropping it ends the session that is serving
/// the transfer.
fn handoff_session(
    socket: &Path,
    protocol: u32,
    build: &str,
) -> (std::os::unix::net::UnixStream, Message) {
    let mut stream = std::os::unix::net::UnixStream::connect(socket).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(30)))
        .expect("write timeout");
    let hello = Message::Hello {
        v: VERSION,
        client: "handoff-test".to_string(),
        wants: vec![VERSION],
    };
    stream
        .write_all(&codec::encode_frame(&hello).expect("hello"))
        .expect("write");
    assert!(
        matches!(read_one(&mut stream), Message::Welcome { .. }),
        "the handshake is answered"
    );
    let request = Message::Handoff {
        v: VERSION,
        protocol,
        build: build.to_string(),
    };
    stream
        .write_all(&codec::encode_frame(&request).expect("encode"))
        .expect("write");
    let reply = read_one(&mut stream);
    (stream, reply)
}

/// The nonce a [`FakeOutgoing`] mints. Fixed so the test can present it.
const FAKE_NONCE: [u8; 32] = [0x5a; 32];

/// The outgoing daemon, played by hand.
///
/// The real outgoing daemon can only ever send its own listener and its own
/// lock, so the only way to test what the *incoming* daemon does with
/// descriptors that are not what they claim — and with a socket path that
/// disappeared mid-transfer — is to be the outgoing side. The handshake, the
/// nonce and the transfer connection are all real; only the descriptors are
/// chosen by the test.
struct FakeOutgoing {
    socket: PathBuf,
    /// The control socket the incoming daemon connected to, and (by default)
    /// the listener whose descriptor is "the listener".
    control: std::os::unix::net::UnixListener,
    /// A genuinely held lock at `<socket>.lock`, so a valid lock descriptor can
    /// be a dup of a real lock rather than a stand-in.
    lock: arreo_core::lock::ExclusiveLock,
    /// The accepted transfer connection, once [`FakeOutgoing::serve_handshake`]
    /// has run.
    transfer: Option<std::os::unix::net::UnixStream>,
}

impl FakeOutgoing {
    /// Bind the control socket and take the lock. Before the incoming daemon is
    /// spawned, so its first connect lands on this listener.
    fn bind(socket: &Path) -> Self {
        let control = std::os::unix::net::UnixListener::bind(socket).expect("bind control");
        let lock =
            arreo_core::lock::ExclusiveLock::acquire(&arreo_server::persist::lock_path_for(socket))
                .expect("hold the lock");
        Self {
            socket: socket.to_path_buf(),
            control,
            lock,
            transfer: None,
        }
    }

    /// Answer one incoming daemon's Hello→Handoff, bind the transfer socket,
    /// accept the transfer connection, and check the nonce it presents.
    fn serve_handshake(&mut self) {
        let mut conn = accept_within(&self.control, Duration::from_secs(15));
        conn.set_read_timeout(Some(Duration::from_secs(15)))
            .expect("timeout");
        conn.set_write_timeout(Some(Duration::from_secs(15)))
            .expect("timeout");
        // A1: the real `--handoff-from` binary opens with Hello, and what it
        // offers must be every version it can speak — not merely its own.
        // Asserted here because this is the one place a test can read the
        // *dialling* client's own Hello off the wire, and the handoff is the cut
        // that MUST work in the forward direction (a newer daemon taking over
        // from an older one). At `VERSION = 0` the two spellings coincide, so
        // this pins the rule for the version bump that separates them: revert the
        // call site to `vec![VERSION]` after a bump and this asserts.
        match read_one(&mut conn) {
            Message::Hello { wants, .. } => assert_eq!(
                wants,
                arreo_core::proto::client_versions(),
                "the incoming daemon offers every version it speaks, not just its own"
            ),
            other => panic!("the incoming daemon opens with Hello, got {other:?}"),
        }
        conn.write_all(
            &codec::encode_frame(&Message::Welcome {
                v: VERSION,
                server: "fake-outgoing".to_string(),
            })
            .expect("welcome"),
        )
        .expect("write");
        assert!(
            matches!(read_one(&mut conn), Message::Handoff { .. }),
            "then asks for the handoff"
        );
        conn.write_all(
            &codec::encode_frame(&Message::HandoffReady {
                v: VERSION,
                protocol: VERSION,
                server_protocol: VERSION,
                panes: 0,
                nonce: FAKE_NONCE.to_vec(),
            })
            .expect("ready"),
        )
        .expect("write");
        let transfer_listener = std::os::unix::net::UnixListener::bind(
            arreo_server::handoff::handoff_path_for(&self.socket),
        )
        .expect("bind the transfer socket");
        let mut transfer = accept_within(&transfer_listener, Duration::from_secs(15));
        transfer
            .set_read_timeout(Some(Duration::from_secs(15)))
            .expect("timeout");
        // The incoming daemon presents the nonce before anything else.
        let mut presented = [0u8; 32];
        transfer
            .read_exact(&mut presented)
            .expect("the incoming daemon presents the nonce");
        assert_eq!(presented, FAKE_NONCE, "the nonce it was given");
        self.transfer = Some(transfer);
    }

    /// Send the two descriptors in the contract's order.
    fn send_descriptors(
        &self,
        listener: std::os::unix::io::BorrowedFd<'_>,
        lock: std::os::unix::io::BorrowedFd<'_>,
    ) {
        let transfer = self.transfer.as_ref().expect("the handshake was served");
        arreo_core::pty::adopt::send_fd(transfer, listener).expect("send the listener descriptor");
        arreo_core::pty::adopt::send_fd(transfer, lock).expect("send the lock descriptor");
    }

    /// The audit store the incoming daemon records into.
    fn db(&self) -> PathBuf {
        arreo_server::persist::db_path_for(&self.socket)
    }

    /// The transfer connection must end **without** the commit marker: the
    /// incoming daemon refused instead of committing the cut.
    fn assert_no_commit(&mut self) {
        let transfer = self.transfer.as_mut().expect("the handshake was served");
        let mut byte = [0u8; 1];
        match transfer.read(&mut byte) {
            Ok(0) => {}
            Ok(_) => panic!(
                "the incoming daemon committed ({:#04x}) after refusing",
                byte[0]
            ),
            Err(e) => panic!("reading the transfer connection: {e}"),
        }
    }
}

/// Spawn a real `--handoff-from` process against a bound [`FakeOutgoing`] and
/// serve its handshake.
fn start_incoming(fake: &mut FakeOutgoing) -> Guard {
    let incoming = spawn_handoff(
        &fake.socket,
        &["--handoff-timeout-secs", "10"],
        std::process::Stdio::piped(),
    );
    fake.serve_handshake();
    incoming
}

fn handoff_rows(socket: &Path, action: &str) -> Vec<arreo_core::store::StoredAudit> {
    handoff_rows_n(socket, action, 10)
}

/// `handoff_rows` with an explicit limit, for the row-count assertions.
fn handoff_rows_n(
    socket: &Path,
    action: &str,
    limit: usize,
) -> Vec<arreo_core::store::StoredAudit> {
    let db = arreo_server::persist::db_path_for(socket);
    let store = arreo_core::store::SessionStore::open(&db).expect("audit store");
    store.audit_by_action(action, limit).expect("audit rows")
}

/// Criterion 1: the socket never goes dead across the cut.
///
/// Two halves, both deterministic:
///
/// 1. A **real cut**, sampled throughout: a client thread connects to the
///    socket in a tight loop from before the handoff starts until after it
///    completes, and every connect must succeed. A design that leaves the path
///    without a listener for any moment (close-then-rebind instead of handing
///    the listener over) shows up as a refused connect.
/// 2. The **mid-handoff window**, held open: the test asks for a handoff by
///    hand and, while the daemon is inside it — transfer socket bound,
///    `HandoffReady` sent, waiting for the incoming daemon to connect —
///    connects to the client socket and completes a Hello→Welcome. That lands
///    strictly inside a handoff every run, which the earlier version of this
///    test could not guarantee: it raced the real incoming daemon's whole
///    handoff (~1 ms) against a 20 ms poll and failed under load whenever the
///    cut finished first. The request is then abandoned, and the daemon must
///    still be serving.
///
/// What removal turns red: closing the listener before the cut completes (the
/// sampler sees a refused connect); serving on a rebound socket (the held-open
/// mid-handoff connect fails); dropping the listener on commit (the post-cut
/// request fails); the outgoing daemon never exiting (the cut assert).
#[test]
fn the_socket_accepts_while_the_handoff_is_in_progress() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    let socket = temp_socket("cut");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));
    let old_pid = old.id();

    // The sampler: every connect to the path must find a listener, before,
    // during and after the cut.
    let stop = Arc::new(AtomicBool::new(false));
    let looked = Arc::new(AtomicUsize::new(0));
    let refused = Arc::new(AtomicUsize::new(0));
    let sampler = {
        let stop = Arc::clone(&stop);
        let looked = Arc::clone(&looked);
        let refused = Arc::clone(&refused);
        let path = socket.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match std::os::unix::net::UnixStream::connect(&path) {
                    Ok(stream) => {
                        looked.fetch_add(1, Ordering::Relaxed);
                        // Close at once: this is a liveness probe, not a session.
                        drop(stream);
                    }
                    Err(_) => {
                        refused.fetch_add(1, Ordering::Relaxed);
                    }
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        })
    };

    let mut new = spawn_handoff(&socket, &[], std::process::Stdio::piped());
    assert!(
        until(Duration::from_secs(30), || matches!(
            old.try_wait(),
            Ok(Some(_))
        )),
        "the outgoing daemon never exited"
    );
    let status = old.try_wait().expect("try_wait").expect("exited");
    assert!(
        status.success(),
        "the outgoing daemon exits 0, got {status}"
    );
    wait_serving(&mut new, &socket, Duration::from_secs(10));
    stop.store(true, Ordering::Relaxed);
    sampler.join().expect("the sampler does not panic");
    assert!(
        looked.load(Ordering::Relaxed) > 0,
        "the sampler made no observations"
    );
    assert_eq!(
        refused.load(Ordering::Relaxed),
        0,
        "the socket accepted at every one of {} samples across the cut",
        looked.load(Ordering::Relaxed)
    );
    // A fresh connect after the cut is served by the new daemon.
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
    assert_ne!(new.id(), old_pid, "the cut replaced the process");
    assert_eq!(
        handoff_rows(&socket, arreo_core::store::actions::HANDOFF).len(),
        1,
        "one cut, one audit row"
    );

    // The mid-handoff window, held open here rather than raced: the daemon has
    // bound the transfer socket, answered `HandoffReady`, and is waiting for
    // the incoming daemon to connect. A client connect + Hello→Welcome while it
    // is in that state lands strictly inside a handoff every run — which the
    // earlier version of this test could not guarantee, since it raced the real
    // incoming daemon's whole handoff (~1 ms) against a 20 ms poll and failed
    // under load whenever the cut finished first.
    //
    // (The window *after* the descriptors, with the commit outstanding, is not
    // used here on purpose: a peer that holds that state open makes the daemon
    // wait the full `DEFAULT_HANDOFF_TIMEOUT`, and this test would spend ten
    // seconds proving the same point.)
    let (session, reply) = accepted_handoff(&socket, "held-open");
    assert!(
        matches!(reply, Message::HandoffReady { .. }),
        "the daemon accepted the request: {reply:?}"
    );
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
    drop(waiting);
    // Abandon the handoff: dropping the session without ever connecting to
    // `<socket>.handoff` means no transfer begins, so the cut does not happen.
    // (The daemon will time that request out after `DEFAULT_HANDOFF_TIMEOUT` and
    // record an abort; nothing waits on the socket meanwhile, and the daemon
    // must still be serving right now.)
    drop(session);
    assert!(
        new.try_wait().expect("try_wait").is_none(),
        "the daemon is still serving after the abandoned handoff"
    );
    let reply = raw_request(
        &socket,
        &Message::Panes {
            v: VERSION,
            panes: vec![],
        },
    );
    assert!(
        matches!(reply, Message::Panes { .. }),
        "and a client is still served: {reply:?}"
    );
    assert_eq!(
        handoff_rows(&socket, arreo_core::store::actions::HANDOFF).len(),
        1,
        "the abandoned handoff did not commit"
    );
    cleanup(&socket);
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
/// The version here is `VERSION + 2` — a gap of two, refused under either
/// direction of the check. The *direction* is pinned by
/// `the_version_window_is_the_incoming_daemons` (unit) and
/// `a_forward_bump_takes_the_socket_over` (end to end), because `VERSION + 10`
/// could not tell the two directions apart.
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
            protocol: VERSION + 2,
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
/// The abort is an incoming daemon that dies between receiving the descriptors
/// and committing, played deterministically: the test is the incoming daemon
/// through the descriptor transfer (nonce presented, listener and lock
/// received) and then drops the connection instead of sending the commit
/// marker. A killed process closes its descriptors the same way — dropping the
/// connection *is* the abort — without racing the real binary's startup against
/// the ~1 ms handoff (the earlier version waited for `<socket>.handoff` to
/// appear and then killed the child, which killed a *committed* incoming daemon
/// when the cut won the race, leaving nobody serving).
///
/// What removal turns red: unlinking the main socket on abort (connect fails);
/// dropping the lock on abort (third daemon serves); leaving the `.handoff`
/// file (exists assert); not releasing the one-handoff lock (the retry never
/// cuts); reading EOF as a commit (the transfer commits — the daemon exits and
/// the abort row never appears).
#[test]
fn an_aborted_handoff_leaves_the_daemon_serving_and_retryable() {
    let socket = temp_socket("abort");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));

    // Play the incoming daemon up to the moment it dies.
    let (session, reply) = accepted_handoff(&socket, "aborted");
    let nonce = match reply {
        Message::HandoffReady { nonce, .. } => nonce,
        other => panic!("the daemon accepted the request: {other:?}"),
    };
    let handoff_path = arreo_server::handoff::handoff_path_for(&socket);
    let mut transfer = std::os::unix::net::UnixStream::connect(&handoff_path)
        .expect("connect to the transfer socket");
    transfer
        .set_read_timeout(Some(Duration::from_secs(15)))
        .expect("timeout");
    transfer.write_all(&nonce).expect("present the nonce");
    let listener_fd = arreo_server::handoff::recv_one(&transfer, Duration::from_secs(15))
        .expect("the listener descriptor");
    let lock_fd = arreo_server::handoff::recv_one(&transfer, Duration::from_secs(15))
        .expect("the lock descriptor");
    // The incoming daemon dies here: the connection closes with no commit byte.
    drop(transfer);

    // The outgoing daemon must recover: unlink the transfer socket, keep
    // serving, and record the abort rather than a cut.
    assert!(
        until(Duration::from_secs(15), || {
            !handoff_path.exists()
                && !handoff_rows(&socket, arreo_core::store::actions::HANDOFF_ABORT).is_empty()
        }),
        "the outgoing daemon observes the abort and cleans up"
    );
    assert!(
        old.try_wait().expect("try_wait").is_none(),
        "the outgoing daemon keeps serving after the abort"
    );
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_ok(),
        "the socket still answers after the abort"
    );
    let reply = raw_request(
        &socket,
        &Message::Panes {
            v: VERSION,
            panes: vec![],
        },
    );
    assert!(
        matches!(reply, Message::Panes { .. }),
        "the daemon is still serving: {reply:?}"
    );
    assert!(
        handoff_rows(&socket, arreo_core::store::actions::HANDOFF).is_empty(),
        "no cut is recorded: none happened"
    );
    // The one-handoff lock is released when the aborted handoff's session ends
    // — probed by taking it (and giving it straight back) rather than waiting.
    let handoff_lock = arreo_server::handoff::handoff_lock_path_for(&socket);
    assert!(
        until(Duration::from_secs(15), || {
            arreo_core::lock::ExclusiveLock::acquire(&handoff_lock).is_ok()
        }),
        "the aborted handoff releases the one-handoff lock"
    );
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
    drop((session, listener_fd, lock_fd));
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

/// C (T-0038 re-review): **a held one-handoff lock is a deferred update, not a
/// failure** — `--handoff-from` exits 3 for that refusal and 1 for everything
/// else.
///
/// The re-review measured that the real `--handoff-from` binary exits 1 with
/// "another handoff holds already locked (…)" when a same-user process holds the
/// lock, which `arreo update --server` reported as an update *failure* while ADR
/// 0021 §5 promises a deferred one. Exit 3 is the project's "in progress" code —
/// the same value the updater's own lock produces — so a script sees the same
/// answer from either lock, and the CLI (which is not this crate's to change)
/// maps it to its in-progress status.
///
/// The busy half is the *real* daemon refusing a *real* `--handoff-from` process
/// while this test holds the lock: no fake outgoing side, and the refusal
/// travels the whole way — the daemon's typed `Error`, the incoming daemon's
/// string match, the process exit status. The control is the same binary against
/// a socket nobody serves, which must stay 1, so 3 is specific to the lock
/// rather than "any refusal".
///
/// What removal turns red: dropping the exit-3 selection (the busy case observes
/// 1 — the deferred update reads as a failure again); matching too widely, e.g.
/// on any `Error` (the control observes 3); removing `BUSY_REFUSAL` from the
/// daemon's refusal detail (the busy case observes 1, because the incoming
/// daemon can no longer recognise the refusal it must map).
#[test]
fn a_held_handoff_lock_exits_3_and_other_failures_exit_1() {
    let socket = temp_socket("busy-exit");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));

    // The lock the outgoing daemon takes for a handoff, held by this process:
    // the reviewer's "another handoff is already in progress on this machine".
    let held = arreo_core::lock::ExclusiveLock::acquire(
        &arreo_server::handoff::handoff_lock_path_for(&socket),
    )
    .expect("the handoff lock is free while no handoff runs");

    let mut busy = spawn_handoff(&socket, &[], std::process::Stdio::piped());
    assert!(
        until(Duration::from_secs(30), || matches!(
            busy.try_wait(),
            Ok(Some(_))
        )),
        "the incoming daemon exits rather than waiting out the held lock"
    );
    let status = busy.try_wait().expect("try_wait").expect("exited");
    assert_eq!(
        status.code(),
        Some(3),
        "a held handoff lock is 'in progress' (3), not a failure: {status}"
    );
    let detail = stderr_of(&mut busy);
    assert!(
        detail.contains("another handoff"),
        "and the reason reaches stderr: {detail:?}"
    );
    assert!(
        old.try_wait().expect("try_wait").is_none(),
        "the outgoing daemon keeps serving through a refusal"
    );
    drop(held);

    // The control: every other failure stays 1. There is no daemon at this
    // path at all, so the incoming daemon never gets as far as a handoff.
    let nowhere = temp_socket("busy-exit-nowhere");
    cleanup(&nowhere);
    let mut failed = spawn_handoff(&nowhere, &[], std::process::Stdio::piped());
    assert!(
        until(Duration::from_secs(30), || matches!(
            failed.try_wait(),
            Ok(Some(_))
        )),
        "the incoming daemon exits when nothing serves the socket"
    );
    let status = failed.try_wait().expect("try_wait").expect("exited");
    assert_eq!(
        status.code(),
        Some(1),
        "a failure that is not the lock stays 1: {status}"
    );
    drop(failed);
    cleanup(&nowhere);
    cleanup(&socket);
}

/// Ask for a handoff, retrying while the previous one still holds the
/// one-handoff lock (it is released when its session ends — a moment after the
/// abort row of that handoff appears).
fn accepted_handoff(socket: &Path, build: &str) -> (std::os::unix::net::UnixStream, Message) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let (stream, reply) = handoff_session(socket, VERSION, build);
        match &reply {
            Message::Error { message, .. } if message.contains("another handoff") => {
                drop(stream);
                assert!(
                    std::time::Instant::now() < deadline,
                    "the previous handoff never released the lock: {message}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => return (stream, reply),
        }
    }
}

/// F4 (CRITICAL): **a half-close is not a commit**.
///
/// The reported sequence, exactly: connect to the client socket, take the
/// `HandoffReady`, connect to `<socket>.handoff`, `shutdown(SHUT_WR)`, and do
/// nothing else — never read a descriptor, never accept on a listener. The
/// outgoing daemon used to read that end-of-stream as the commit: it exited 0,
/// wrote `handoff ok`, nothing held a listener, and a client's connect hung on
/// a stale backlog.
///
/// The root cause is conceptual — EOF says a peer stopped *writing*, never that
/// a daemon is *serving* — so the fix is a marker byte the peer must *choose* to
/// send, backed by the nonce, and the test drives all three inputs:
///
/// 1. the reported half-close, with nothing else on the transfer connection;
/// 2. the same half-close after presenting the nonce and taking both
///    descriptors, which is what pins the **commit marker** (the first case is
///    already refused by the nonce, so on its own it would not notice EOF being
///    read as a commit again);
/// 3. a peer that never asked for the handoff, sending 32 bytes it was never
///    given plus the commit marker — it cannot commit, which is what pins the
///    **nonce** (a peer that never read `HandoffReady` does not know it).
///
/// The marker is *authorisation*, not proof that a server is behind it — a peer
/// can send it and serve nothing, which
/// `a_commit_by_the_requester_is_authorisation_not_proof_of_serving` states as
/// the accepted limit. What this test pins is narrower and still the thing that
/// matters: no cut commits *without* the byte.
///
/// All three are aborts: the outgoing daemon keeps serving, the audit records
/// aborts, and no `handoff ok` row exists.
///
/// What removal turns red (each verified by reverting it): reading EOF as the
/// commit in `wait_for_commit` (case 2 commits — the outgoing daemon exits 0);
/// accepting any nonce instead of comparing (`recv_nonce` keeps the read, so
/// case 3's marker reaches `wait_for_commit` and commits); dropping the nonce
/// requirement as well (case 1 commits).
#[test]
fn a_half_close_is_not_a_commit() {
    let socket = temp_socket("half-close");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));

    // (1) The reported sequence, verbatim.
    let (session, reply) = handoff_session(&socket, VERSION, "half-close");
    assert!(
        matches!(reply, Message::HandoffReady { .. }),
        "the request is accepted: {reply:?}"
    );
    let handoff_path = arreo_server::handoff::handoff_path_for(&socket);
    // F1: the transfer socket is created 0600 — not the mode the ambient umask
    // would give it (0775 or 0755), which admits every same-group user, since
    // `connect()` needs only write permission on the inode.
    let mode = std::fs::metadata(&handoff_path)
        .expect("the transfer socket exists")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "the transfer socket is bound mode 0600");
    let transfer = std::os::unix::net::UnixStream::connect(&handoff_path)
        .expect("connect to the transfer socket");
    transfer
        .shutdown(std::net::Shutdown::Write)
        .expect("shutdown(SHUT_WR)");

    // The abort is audited, and the outgoing daemon is still there serving.
    assert!(
        until(Duration::from_secs(15), || !handoff_rows(
            &socket,
            arreo_core::store::actions::HANDOFF_ABORT
        )
        .is_empty()),
        "the half-close is recorded as an abort"
    );
    assert!(
        old.try_wait().expect("try_wait").is_none(),
        "the outgoing daemon keeps serving instead of exiting on a half-close"
    );
    // A client is served — the socket is not half-dead.
    let reply = raw_request(
        &socket,
        &Message::Panes {
            v: VERSION,
            panes: vec![],
        },
    );
    assert!(
        matches!(reply, Message::Panes { .. }),
        "a client is still served: {reply:?}"
    );
    assert!(
        handoff_rows(&socket, arreo_core::store::actions::HANDOFF).is_empty(),
        "no cut is recorded: none happened"
    );
    assert!(
        until(Duration::from_secs(5), || !handoff_path.exists()),
        "the aborted transfer socket is unlinked"
    );

    // (2) The same input with the nonce presented: the half-close now happens
    // *after* the descriptors, so what refuses it is the missing commit marker.
    let (session2, reply2) = accepted_handoff(&socket, "half-close");
    let nonce = match reply2 {
        Message::HandoffReady { nonce, .. } => nonce,
        other => panic!("the second request is accepted too: {other:?}"),
    };
    let mut transfer2 = std::os::unix::net::UnixStream::connect(&handoff_path)
        .expect("connect to the second transfer socket");
    transfer2
        .set_read_timeout(Some(Duration::from_secs(15)))
        .expect("timeout");
    transfer2.write_all(&nonce).expect("present the nonce");
    let listener_fd = arreo_server::handoff::recv_one(&transfer2, Duration::from_secs(15))
        .expect("the listener descriptor");
    let lock_fd = arreo_server::handoff::recv_one(&transfer2, Duration::from_secs(15))
        .expect("the lock descriptor");
    transfer2
        .shutdown(std::net::Shutdown::Write)
        .expect("shutdown(SHUT_WR) after the descriptors");

    assert!(
        until(Duration::from_secs(15), || handoff_rows_n(
            &socket,
            arreo_core::store::actions::HANDOFF_ABORT,
            10
        )
        .len()
            >= 2),
        "the second half-close is recorded as an abort too"
    );
    assert!(
        old.try_wait().expect("try_wait").is_none(),
        "the outgoing daemon is still serving after both half-closes"
    );
    assert!(
        handoff_rows(&socket, arreo_core::store::actions::HANDOFF).is_empty(),
        "no cut is recorded: neither half-close was a commit"
    );
    let reply = raw_request(
        &socket,
        &Message::Panes {
            v: VERSION,
            panes: vec![],
        },
    );
    assert!(
        matches!(reply, Message::Panes { .. }),
        "and a client is still served: {reply:?}"
    );

    // (3) A peer that never asked for the handoff does not know the nonce, so
    // it cannot commit one: 32 bytes it was never given, then the commit
    // marker, is refused before any descriptor moves.
    let (session3, reply3) = accepted_handoff(&socket, "not-the-requester");
    assert!(
        matches!(reply3, Message::HandoffReady { .. }),
        "the daemon accepted the request before the impostor arrived: {reply3:?}"
    );
    let mut transfer3 = std::os::unix::net::UnixStream::connect(&handoff_path)
        .expect("connect to the third transfer socket");
    transfer3
        .write_all(&[0xaa; arreo_server::handoff::NONCE_BYTES])
        .expect("a nonce it was never given");
    transfer3
        .write_all(&[arreo_core::pty::adopt::HANDOFF_COMMIT_BYTE])
        .expect("and the commit marker");
    assert!(
        until(Duration::from_secs(15), || handoff_rows_n(
            &socket,
            arreo_core::store::actions::HANDOFF_ABORT,
            10
        )
        .len()
            >= 3),
        "the impostor's attempt is recorded as an abort"
    );
    assert!(
        old.try_wait().expect("try_wait").is_none(),
        "the outgoing daemon is still serving after the impostor"
    );
    assert!(
        handoff_rows(&socket, arreo_core::store::actions::HANDOFF).is_empty(),
        "no cut is recorded: nobody committed"
    );
    drop((
        session,
        transfer,
        session2,
        transfer2,
        listener_fd,
        lock_fd,
        session3,
        transfer3,
    ));
    cleanup(&socket);
}

/// F9: the version window is the **incoming** daemon's, so a forward bump
/// commits and a downgrade does not.
///
/// This is the end-to-end half of the direction check: the incoming daemon
/// offers `VERSION + 1` — the release that will exist tomorrow — and the cut
/// must complete, with the audit naming both sides. The old call
/// (`negotiate(VERSION, [incoming])`) refused exactly this, which is why a real
/// release could never hand over; the existing test sent `VERSION + 10`, which
/// is refused under either direction and so could not tell them apart.
///
/// The by-hand transfer also exercises the whole hardened grammar (nonce first,
/// two descriptors, commit marker byte) against the real outgoing daemon.
///
/// What removal turns red: calling `negotiate(VERSION, [incoming])` (the
/// request is refused and the outgoing daemon never exits); dropping the nonce
/// requirement (this test's nonce presentation stops being required, and the
/// half-close test's abort stops being an abort).
#[test]
fn a_forward_bump_takes_the_socket_over() {
    let socket = temp_socket("forward-bump");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));

    let (session, reply) = handoff_session(&socket, VERSION + 1, "newer");
    let nonce = match reply {
        Message::HandoffReady { nonce, .. } => nonce,
        other => panic!("a one-version bump is accepted, got {other:?}"),
    };
    assert_eq!(
        nonce.len(),
        arreo_server::handoff::NONCE_BYTES,
        "the outgoing daemon mints the handoff nonce"
    );
    let handoff_path = arreo_server::handoff::handoff_path_for(&socket);
    let mut transfer = std::os::unix::net::UnixStream::connect(&handoff_path)
        .expect("connect to the transfer socket");
    transfer
        .set_read_timeout(Some(Duration::from_secs(15)))
        .expect("timeout");
    transfer.write_all(&nonce).expect("present the nonce");
    let listener_fd = arreo_server::handoff::recv_one(&transfer, Duration::from_secs(15))
        .expect("the listener descriptor");
    let lock_fd = arreo_server::handoff::recv_one(&transfer, Duration::from_secs(15))
        .expect("the lock descriptor");
    // Commit: the marker byte a serving incoming daemon sends.
    transfer
        .write_all(&[arreo_core::pty::adopt::HANDOFF_COMMIT_BYTE])
        .expect("commit");

    assert!(
        until(Duration::from_secs(30), || matches!(
            old.try_wait(),
            Ok(Some(_))
        )),
        "the cut commits and the outgoing daemon exits"
    );
    let status = old.try_wait().expect("try_wait").expect("exited");
    assert!(
        status.success(),
        "the outgoing daemon exits 0, got {status}"
    );
    let rows = handoff_rows(&socket, arreo_core::store::actions::HANDOFF);
    assert_eq!(rows.len(), 1, "one cut, one row: {rows:?}");
    let detail = rows[0].detail.clone().unwrap_or_default();
    assert!(
        detail.contains(&format!("handoff {VERSION} -> {}", VERSION + 1)),
        "the row names both sides of the cut: {detail:?}"
    );
    drop((listener_fd, lock_fd, transfer, session));
    cleanup(&socket);
}

/// Does **anything on `socket` answer a Hello with a Welcome**? Every step is
/// bounded, so a socket with nobody behind it answers `false` in milliseconds
/// rather than hanging: a connect that fails, a write that breaks, or a read
/// that ends without a frame all mean the same thing.
fn something_serves(socket: &Path) -> bool {
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(socket) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
    let hello = Message::Hello {
        v: VERSION,
        client: "no-server-probe".to_string(),
        wants: arreo_core::proto::client_versions(),
    };
    if stream
        .write_all(&codec::encode_frame(&hello).expect("hello"))
        .is_err()
    {
        return false;
    }
    matches!(read_one_soft(&mut stream), Ok(Message::Welcome { .. }))
}

/// B (T-0038 re-review): **the commit byte is authorisation, not proof that a
/// peer is serving.**
///
/// The stage-1 docs (and this file's module docs) called it "positive evidence
/// that a peer is serving". It is not, and cannot be: the outgoing daemon cannot
/// see another process's accept loop. This is the reviewer's reproduction, kept
/// as a test — a peer asks for the handoff, reads `HandoffReady`, connects to the
/// transfer socket, presents the nonce, takes both descriptors, sends the commit
/// byte, and then does nothing at all. The outgoing daemon exits 0, records the
/// cut, and **nobody is serving**: a Hello on that socket is never answered.
/// The byte proves only that its sender is the process that requested the
/// handoff (it could only learn the nonce by asking on the main socket).
///
/// The assertion is "nothing answers a Hello", not "connect refuses". The
/// outgoing daemon wakes its parked accept loop by connecting to its own socket
/// (`daemon.rs`, step 8), and an unaccepted connection from that wakeup keeps the
/// socket object alive for a few milliseconds after the listener closes — a
/// connect in that window is queued and then reset. With a real incoming daemon
/// the socket never dies (it holds the dup the cut exists to pass), so this
/// window belongs to *this test's* input; the durable fact is the one asserted.
///
/// Why the gap is accepted rather than closed: the obvious fix — after the byte,
/// connect to `<socket>` and wait for a `Welcome` — makes the cut **two**
/// decisions. A probe that times out on a slow-but-healthy daemon would leave the
/// outgoing daemon serving *and* the incoming daemon committed: two daemons on
/// one socket, the F5 failure, reintroduced by the check meant to prevent a
/// different one. One byte is one atomic decision; a probe is two. The reasoning
/// lives next to the code in `handoff::wait_for_commit` and on
/// `HANDOFF_COMMIT_BYTE`, and this test is what says it out loud.
///
/// What turns it red: **adding the probe** — the outgoing daemon would then wait
/// for a `Welcome` nobody sends, so it never exits and the exit assert fails
/// (`a_forward_bump_takes_the_socket_over` goes red for the same reason on the
/// same input, which is why this test names the decision rather than duplicating
/// that one); **dropping the commit byte** (F4's half-close cases commit again,
/// and "committed" here stops meaning what this test asserts).
#[test]
fn a_commit_by_the_requester_is_authorisation_not_proof_of_serving() {
    let socket = temp_socket("commit-no-server");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));

    let (session, reply) = handoff_session(&socket, VERSION, "commit-only");
    let nonce = match reply {
        Message::HandoffReady { nonce, .. } => nonce,
        other => panic!("the request is accepted: {other:?}"),
    };
    let handoff_path = arreo_server::handoff::handoff_path_for(&socket);
    let mut transfer = std::os::unix::net::UnixStream::connect(&handoff_path)
        .expect("connect to the transfer socket");
    transfer
        .set_read_timeout(Some(Duration::from_secs(15)))
        .expect("timeout");
    transfer.write_all(&nonce).expect("present the nonce");
    let listener_fd = arreo_server::handoff::recv_one(&transfer, Duration::from_secs(15))
        .expect("the listener descriptor");
    let lock_fd = arreo_server::handoff::recv_one(&transfer, Duration::from_secs(15))
        .expect("the lock descriptor");
    // The whole of what this peer does: commit, and then nothing. It never
    // accepts, never serves, never even reads the descriptor it was given.
    transfer
        .write_all(&[arreo_core::pty::adopt::HANDOFF_COMMIT_BYTE])
        .expect("commit");

    assert!(
        until(Duration::from_secs(30), || matches!(
            old.try_wait(),
            Ok(Some(_))
        )),
        "the byte alone commits the cut and the outgoing daemon exits"
    );
    let status = old.try_wait().expect("try_wait").expect("exited");
    assert!(status.success(), "and it exits 0, got {status}");
    assert_eq!(
        handoff_rows(&socket, arreo_core::store::actions::HANDOFF).len(),
        1,
        "the cut is audited as one that happened"
    );
    // Now the limit the claim fix admits to: with the outgoing daemon gone and
    // this side's dups closed, **nothing on the path answers**. Asked as "does a
    // Hello ever get a Welcome", not as "does connect refuse": the outgoing
    // daemon's self-connect wakeup (`daemon.rs`, step 8) can leave a connection
    // queued on the socket object, which keeps it alive for a few milliseconds
    // after the listener is closed — a `connect()` in that window is queued and
    // then reset. That is a property of *this* test's input (there is no
    // incoming daemon holding the dup the real handoff always has), and the
    // durable fact is the one asserted here: no daemon is behind the socket.
    drop((listener_fd, lock_fd, transfer, session));
    assert!(
        !something_serves(&socket),
        "something answered a Hello after the cut: the byte left a daemon serving"
    );
    cleanup(&socket);
}

/// F5 (HIGH): **one handoff at a time**.
///
/// Part 1 is the review's race made deterministic: a handoff is held open by
/// hand (the daemon has bound the transfer socket and answered `HandoffReady`),
/// and a second request on a fresh connection must be refused with a typed
/// error while the first proceeds. Without the one-handoff lock the second
/// request is *accepted* — that is how two concurrent handoffs both committed
/// and left two daemons serving one socket, T-0071's invariant defeated.
///
/// Part 2 is the review's scenario end to end: two `--handoff-from` processes
/// started together. When it settles, exactly one daemon serves the socket: the
/// other either overlapped the first and was refused, or arrived after the
/// first cut and performed a second, sequential cut — both correct, and neither
/// leaves two daemons on one socket. A client keeps talking across the whole
/// window and is served throughout.
///
/// What removal turns red: not taking the handoff lock (part 1's refusal
/// disappears — the second request is answered `HandoffReady`); the lock not
/// being released when a handoff ends (part 2 never settles to one daemon).
#[test]
fn one_handoff_at_a_time() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    let socket = temp_socket("exclusive");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));

    // A client that keeps talking across the whole window: the socket must
    // answer throughout (a connection the outgoing daemon accepts at the instant
    // of the cut is dropped on purpose, so the client retries).
    let stop = Arc::new(AtomicBool::new(false));
    let client_socket = socket.clone();
    let client_stop = Arc::clone(&stop);
    let client = std::thread::spawn(move || {
        let mut served = 0u32;
        while !client_stop.load(Ordering::Relaxed) {
            if try_round_trip(
                &client_socket,
                &Message::Panes {
                    v: VERSION,
                    panes: vec![],
                },
            )
            .is_ok()
            {
                served += 1;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        served
    });

    // (1) While a handoff is in progress, a second request is refused.
    let handoff_path = arreo_server::handoff::handoff_path_for(&socket);
    let handoff_lock = arreo_server::handoff::handoff_lock_path_for(&socket);
    let (held, reply) = accepted_handoff(&socket, "held-open");
    assert!(
        matches!(reply, Message::HandoffReady { .. }),
        "the daemon accepted the first request: {reply:?}"
    );
    let (second, reply2) = handoff_session(&socket, VERSION, "second");
    match &reply2 {
        Message::Error { message, .. } => assert!(
            message.contains("another handoff"),
            "the second request is refused, naming the reason: {message:?}"
        ),
        other => panic!("a second handoff must be refused while one is in progress, got {other:?}"),
    }
    assert!(
        old.try_wait().expect("try_wait").is_none(),
        "a refusal leaves the daemon serving"
    );
    assert!(
        try_round_trip(
            &socket,
            &Message::Panes {
                v: VERSION,
                panes: vec![],
            }
        )
        .is_ok(),
        "a client is served while the handoff is in progress"
    );
    assert!(
        handoff_rows(&socket, arreo_core::store::actions::HANDOFF).is_empty(),
        "no cut is recorded: no handoff completed yet"
    );
    // End the held handoff (connect and close: an abort, so the daemon cleans up
    // at once instead of waiting out its accept deadline) and let it release the
    // lock for part 2.
    let _ = std::os::unix::net::UnixStream::connect(&handoff_path).map(drop);
    drop((held, second));
    assert!(
        until(Duration::from_secs(15), || !handoff_path.exists()),
        "the abandoned handoff unlinks its transfer socket"
    );
    assert!(
        until(Duration::from_secs(15), || {
            arreo_core::lock::ExclusiveLock::acquire(&handoff_lock).is_ok()
        }),
        "the abandoned handoff releases the one-handoff lock"
    );

    // (2) The review's scenario: two `--handoff-from` processes at once.
    let mut first = spawn_handoff(&socket, &[], std::process::Stdio::null());
    let mut contender = spawn_handoff(&socket, &[], std::process::Stdio::null());
    assert!(
        until(Duration::from_secs(30), || {
            let first_done = first.try_wait().expect("try_wait").is_some();
            let second_done = contender.try_wait().expect("try_wait").is_some();
            first_done != second_done && matches!(old.try_wait(), Ok(Some(_)))
        }),
        "exactly one handoff settles into serving, and the old daemon exits"
    );
    let status = old.try_wait().expect("try_wait").expect("exited");
    assert!(
        status.success(),
        "the outgoing daemon exits 0, got {status}"
    );

    stop.store(true, Ordering::Relaxed);
    let served = client.join().expect("the client thread does not panic");
    assert!(served > 0, "a client was served throughout the cut");

    let (mut alive, mut gone) = if first.try_wait().expect("try_wait").is_none() {
        (first, contender)
    } else {
        (contender, first)
    };
    assert!(
        alive.try_wait().expect("try_wait").is_none(),
        "exactly one daemon serves the socket"
    );
    assert!(
        matches!(gone.try_wait(), Ok(Some(_))),
        "the other handoff process is gone"
    );
    wait_serving(&mut alive, &socket, Duration::from_secs(10));
    let reply = raw_request(
        &socket,
        &Message::Panes {
            v: VERSION,
            panes: vec![],
        },
    );
    assert!(
        matches!(reply, Message::Panes { .. }),
        "the surviving daemon serves the socket: {reply:?}"
    );
    // One cut, or two *sequential* ones if the second request arrived after the
    // first completed — never two `ok` rows from two concurrent handoffs.
    let rows = handoff_rows_n(&socket, arreo_core::store::actions::HANDOFF, 10);
    assert!(
        !rows.is_empty() && rows.len() <= 2,
        "one or two sequential cuts: {rows:?}"
    );
    let _ = old.wait();
    cleanup(&socket);
}

/// A daemon that took the socket over by handoff can hand it on again, and
/// **exits 0** when it does: at that moment it is an outgoing daemon like any
/// other, and a successful handover must not be reported as a failure. (It was:
/// the incoming daemon's serving task ends with `Ok(())` when a later handoff
/// commits, and that arm claimed "the inherited listener failed" and exited 1.)
///
/// Two sequential cuts, both real: B takes over from A, then C takes over from
/// B. The second cut is what makes B an outgoing daemon.
///
/// What removal turns red: treating `Ok(())` from the serving task as a failure
/// (B exits 1 and this test's `success()` assert fails).
#[test]
fn a_daemon_that_took_over_can_hand_the_socket_on() {
    let socket = temp_socket("hand-on");
    cleanup(&socket);
    let mut a = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut a, &socket, Duration::from_secs(10));

    // B takes over from A.
    let mut b = spawn_handoff(&socket, &[], std::process::Stdio::piped());
    assert!(
        until(Duration::from_secs(30), || matches!(
            a.try_wait(),
            Ok(Some(_))
        )),
        "the first cut completes"
    );
    assert!(
        a.try_wait().expect("try_wait").expect("exited").success(),
        "the first outgoing daemon exits 0"
    );
    wait_serving(&mut b, &socket, Duration::from_secs(10));

    // C takes over from B: B is the outgoing daemon now, and exits 0 with it.
    let mut c = spawn_handoff(&socket, &[], std::process::Stdio::piped());
    assert!(
        until(Duration::from_secs(30), || matches!(
            b.try_wait(),
            Ok(Some(_))
        )),
        "the second cut completes"
    );
    let status = b.try_wait().expect("try_wait").expect("exited");
    assert!(
        status.success(),
        "a daemon that took over by handoff exits 0 when it hands the socket on, got {status}"
    );
    let text = stderr_of(&mut b);
    assert!(
        !text.contains("failed"),
        "a successful handover is not reported as a failure: {text:?}"
    );
    wait_serving(&mut c, &socket, Duration::from_secs(10));
    let reply = raw_request(
        &socket,
        &Message::Panes {
            v: VERSION,
            panes: vec![],
        },
    );
    assert!(
        matches!(reply, Message::Panes { .. }),
        "the third daemon serves the socket: {reply:?}"
    );
    assert_eq!(
        handoff_rows(&socket, arreo_core::store::actions::HANDOFF).len(),
        2,
        "two sequential cuts"
    );
    let _ = a.wait();
    cleanup(&socket);
}

/// F6 (HIGH): a lock that is not where the daemon looks stops the cut.
///
/// `<socket>.lock` is removed while the daemon holds the lock (unlinking does
/// not release an open description). The incoming daemon's descriptor check
/// compares the received descriptor with the path and refuses, and — this is
/// the fix — a refusal is a **failure**: it sends no commit marker, so the
/// outgoing daemon aborts and keeps serving. It used to commit anyway, leaving
/// the socket with nobody serving.
///
/// What removal turns red: ignoring the refusal (the outgoing daemon exits 0 —
/// the `still serving` assert fails — and a `handoff` row exists); not checking
/// the received descriptor against the path (the handoff commits and the lock
/// is gone).
#[test]
fn a_lock_that_is_not_where_the_daemon_looks_stops_the_handoff() {
    let socket = temp_socket("missing-lock");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));

    std::fs::remove_file(arreo_server::persist::lock_path_for(&socket))
        .expect("unlink the lock the daemon holds");

    let mut incoming = spawn_handoff(
        &socket,
        &["--handoff-timeout-secs", "10"],
        std::process::Stdio::piped(),
    );
    assert!(
        exited_within(&mut incoming, Duration::from_secs(20)),
        "the incoming daemon refuses and exits"
    );
    let text = stderr_of(&mut incoming);
    assert!(
        text.contains("the lock descriptor was refused"),
        "the reason is on stderr: {text:?}"
    );
    // The outgoing daemon is still serving: it never exited, and a fresh client
    // is answered.
    assert!(
        old.try_wait().expect("try_wait").is_none(),
        "the outgoing daemon keeps serving after the refusal"
    );
    let reply = raw_request(
        &socket,
        &Message::Panes {
            v: VERSION,
            panes: vec![],
        },
    );
    assert!(
        matches!(reply, Message::Panes { .. }),
        "a client is still served: {reply:?}"
    );
    assert!(
        handoff_rows(&socket, arreo_core::store::actions::HANDOFF).is_empty(),
        "no cut is recorded"
    );
    assert!(
        until(Duration::from_secs(15), || handoff_rows(
            &socket,
            arreo_core::store::actions::HANDOFF_ABORT
        )
        .iter()
        .any(|row| row
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("the lock descriptor was refused"))),
        "the refusal is on the record, with its reason"
    );
    cleanup(&socket);
}

/// F6 (HIGH), the missing-socket trigger: a refusal after the descriptors
/// arrived must not commit.
///
/// The socket path is unlinked while the transfer is in flight (the incoming
/// daemon already connected — that is how it got to the transfer), so its
/// `serve_inherited` finds no socket and refuses. It must exit 1 without
/// sending the commit marker, and it must record its own refusal: the outgoing
/// side only ever sees the connection end.
///
/// What removal turns red: `let _ = ready_rx.await` (the refusal is ignored,
/// the incoming daemon reports "the inherited listener failed" instead of the
/// reason, and writes no abort row with that reason).
#[test]
fn a_missing_socket_path_stops_the_commit() {
    let socket = temp_socket("missing-socket");
    cleanup(&socket);
    let mut fake = FakeOutgoing::bind(&socket);
    let mut incoming = start_incoming(&mut fake);
    // The path goes away mid-transfer, after the incoming daemon's connect.
    std::fs::remove_file(&socket).expect("unlink the socket mid-handoff");
    fake.send_descriptors(fake.control.as_fd(), fake.lock.fd());
    fake.assert_no_commit();
    assert!(
        exited_within(&mut incoming, Duration::from_secs(20)),
        "the incoming daemon refuses and exits"
    );
    let text = stderr_of(&mut incoming);
    assert!(
        text.contains("the inherited listener was refused"),
        "the refusal is reported, not swallowed: {text:?}"
    );
    assert!(
        text.contains("is missing"),
        "and names what was missing: {text:?}"
    );
    let store = arreo_core::store::SessionStore::open(&fake.db()).expect("audit store");
    assert!(
        store
            .audit_by_action(arreo_core::store::actions::HANDOFF, 10)
            .expect("rows")
            .is_empty(),
        "no cut is recorded"
    );
    let aborts = store
        .audit_by_action(arreo_core::store::actions::HANDOFF_ABORT, 10)
        .expect("rows");
    assert_eq!(
        aborts.len(),
        1,
        "one abort row from the incoming side: {aborts:?}"
    );
    let detail = aborts[0].detail.clone().unwrap_or_default();
    assert!(detail.starts_with("incoming:"), "{detail:?}");
    assert!(detail.contains("is missing"), "{detail:?}");
    cleanup(&socket);
}

/// F2 (HIGH): the listener descriptor is validated for **identity**, not just
/// for being a descriptor.
///
/// The review's shape: a *different* listening socket in the listener slot. The
/// incoming daemon must refuse it (serving on it would accept connections the
/// operator's clients never reach), name what arrived, and record the refusal.
///
/// What removal turns red: dropping `validate_listener` (the incoming daemon
/// serves on the decoy socket and never exits).
#[test]
fn a_different_listener_in_the_listener_slot_is_refused() {
    let socket = temp_socket("wrong-listener");
    cleanup(&socket);
    let decoy = temp_socket("decoy");
    cleanup(&decoy);
    let decoy_listener = std::os::unix::net::UnixListener::bind(&decoy).expect("decoy listener");

    let mut fake = FakeOutgoing::bind(&socket);
    let mut incoming = start_incoming(&mut fake);
    fake.send_descriptors(decoy_listener.as_fd(), fake.lock.fd());
    fake.assert_no_commit();
    assert!(
        exited_within(&mut incoming, Duration::from_secs(20)),
        "the incoming daemon refuses and exits"
    );
    let text = stderr_of(&mut incoming);
    assert!(
        text.contains("the listener descriptor was refused"),
        "{text:?}"
    );
    assert!(
        text.contains(&decoy.display().to_string()),
        "naming what arrived: {text:?}"
    );
    let store = arreo_core::store::SessionStore::open(&fake.db()).expect("audit store");
    assert!(
        store
            .audit_by_action(arreo_core::store::actions::HANDOFF, 10)
            .expect("rows")
            .is_empty(),
        "no cut is recorded"
    );
    let aborts = store
        .audit_by_action(arreo_core::store::actions::HANDOFF_ABORT, 10)
        .expect("rows");
    assert_eq!(aborts.len(), 1, "{aborts:?}");
    assert!(
        aborts[0]
            .detail
            .as_deref()
            .unwrap_or("")
            .contains("the listener descriptor was refused"),
        "{:?}",
        aborts[0].detail
    );
    cleanup(&decoy);
    cleanup(&socket);
}

/// F2 (HIGH), the second review shape: a **non-listening `socketpair` end** in
/// the listener slot. `SO_TYPE` is a stream, but `SO_ACCEPTCONN` is 0 — it can
/// never accept, so serving on it would leave the socket dead.
///
/// What removal turns red: dropping the `SO_ACCEPTCONN` check (the incoming
/// daemon adopts the pair end and never exits).
#[test]
fn a_socketpair_end_in_the_listener_slot_is_refused() {
    let socket = temp_socket("not-a-listener");
    cleanup(&socket);
    let (pair, _peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");

    let mut fake = FakeOutgoing::bind(&socket);
    let mut incoming = start_incoming(&mut fake);
    fake.send_descriptors(pair.as_fd(), fake.lock.fd());
    fake.assert_no_commit();
    assert!(
        exited_within(&mut incoming, Duration::from_secs(20)),
        "the incoming daemon refuses and exits"
    );
    let text = stderr_of(&mut incoming);
    assert!(text.contains("not a listening stream socket"), "{text:?}");
    cleanup(&socket);
}

/// F3 (HIGH): a **regular file** in the lock slot is not the lock.
///
/// `is_held(<socket>.lock)` was satisfied by the real holder while the
/// descriptor handed over was `/etc/hostname` in the review's run; the daemon
/// then served holding nothing. The received descriptor is now judged: same
/// device and inode as the lock path, held, and *the held description*.
///
/// What removal turns red: checking only the path (`is_held`) — the incoming
/// daemon adopts the regular file and never exits.
#[test]
fn a_regular_file_in_the_lock_slot_is_refused() {
    let socket = temp_socket("wrong-lock");
    cleanup(&socket);
    let regular_path = temp_socket("regular-file");
    let regular = std::fs::File::create(&regular_path).expect("a regular file");

    let mut fake = FakeOutgoing::bind(&socket);
    let mut incoming = start_incoming(&mut fake);
    // A *valid* listener (the control listener is bound to the socket path),
    // and a regular file where the lock belongs.
    fake.send_descriptors(fake.control.as_fd(), regular.as_fd());
    fake.assert_no_commit();
    assert!(
        exited_within(&mut incoming, Duration::from_secs(20)),
        "the incoming daemon refuses and exits"
    );
    let text = stderr_of(&mut incoming);
    assert!(text.contains("the lock descriptor was refused"), "{text:?}");
    assert!(text.contains("is device"), "naming what arrived: {text:?}");
    cleanup(&regular_path);
    cleanup(&socket);
}

/// F7 (MEDIUM): the audit trail does not invert, and a peer cannot fill it.
///
/// (a) A peer-supplied `build` string is stripped of control characters and
/// bounded before it reaches `detail`: the review wrote a **900 KB** audit row
/// from one frame. Here the same frame is sent and the row is small, readable,
/// and free of the escape sequence.
///
/// What removal turns red: dropping `sanitize_build` (the row is ~900 KB and
/// contains the raw escape).
#[test]
fn a_huge_build_string_does_not_become_a_huge_row() {
    let socket = temp_socket("huge-build");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));

    let hostile = format!("evil\x1b[2J\x07{}", "x".repeat(900 * 1024));
    let (_session, reply) = handoff_session(&socket, VERSION + 2, &hostile);
    assert!(matches!(reply, Message::Error { .. }), "{reply:?}");

    let rows = handoff_rows(&socket, arreo_core::store::actions::HANDOFF_REFUSE);
    assert_eq!(rows.len(), 1, "one refusal, one row: {rows:?}");
    let detail = rows[0].detail.clone().unwrap_or_default();
    assert!(
        detail.len() < 4096,
        "the row is bounded, got {} bytes",
        detail.len()
    );
    assert!(
        !detail.chars().any(char::is_control),
        "control characters are stripped: {detail:?}"
    );
    assert!(
        detail.contains("evil[2J"),
        "the readable part is kept: {detail:?}"
    );
}

/// F7 (MEDIUM): **one refusal row per session**, not one per frame.
///
/// A client that sends `Handoff` in a loop used to make the daemon write a row
/// per frame (50 frames, 50 rows) — the cheapest denial of service against an
/// audit trail, from the machine itself. T-0059 solved the same problem for
/// trust refusals the same way.
///
/// What removal turns red: dropping `HandoffFailureLog` (50 rows).
#[test]
fn handoff_refusals_are_recorded_once_per_session() {
    let socket = temp_socket("refuse-once");
    cleanup(&socket);
    let mut old = spawn_daemon(&socket, &[], std::process::Stdio::null());
    wait_serving(&mut old, &socket, Duration::from_secs(10));

    let (mut session, first) = handoff_session(&socket, VERSION + 2, "loop");
    assert!(matches!(first, Message::Error { .. }), "{first:?}");
    for _ in 1..50 {
        let request = Message::Handoff {
            v: VERSION,
            protocol: VERSION + 2,
            build: "loop".to_string(),
        };
        session
            .write_all(&codec::encode_frame(&request).expect("encode"))
            .expect("write");
        assert!(matches!(read_one(&mut session), Message::Error { .. }));
    }
    let rows = handoff_rows_n(&socket, arreo_core::store::actions::HANDOFF_REFUSE, 100);
    assert_eq!(
        rows.len(),
        1,
        "one refusal row per session, not one per frame: {rows:?}"
    );
    drop(session);
    cleanup(&socket);
}
