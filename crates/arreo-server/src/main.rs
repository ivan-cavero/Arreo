//! `arreo-server` daemon binary (T-0005 serve, T-0012 graceful shutdown).
//!
//! Usage: `arreo-server [--socket PATH] [--config PATH]`. Default socket:
//! `$XDG_RUNTIME_DIR/arreo.sock`, else `/tmp/arreo-<uid>.sock`.
//!
//! Shutdown (T-0012): SIGTERM/SIGINT stops accepting, flushes every pane's
//! committed ring output (a final drain pass over the registry), removes the
//! socket file, and exits 0 within `SHUTDOWN_DEADLINE`. Children keep running
//! (the daemon never kills agents on its way out); T-0018 re-attaches them.
//!
//! Device authority (T-0025): boot loads (or bootstraps) the server root key
//! and the pinned device certificates. A root key that exists but is unusable
//! is a **loud exit**, not a regeneration — minting a new root would silently
//! invalidate every paired device. The same authority is what the remote
//! transport (T-0023) asks before accepting a peer.
//!
//! Relay (T-0051): `--config PATH` (or `$ARREO_CONFIG`) points at a TOML file
//! whose `[relay]` section, when `enabled = true`, starts an outbound session to
//! a relay. **Disabled is the default and costs nothing**: a self-hosted runtime
//! must work with no relay at all, and the local socket API is unchanged whether
//! or not the relay is on.

use arreo_server::lifecycle::SHUTDOWN_DEADLINE;
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let mut socket: Option<PathBuf> = None;
    let mut config: Option<PathBuf> = None;
    let mut handoff_from: Option<PathBuf> = None;
    let mut handoff_timeout = arreo_server::handoff::DEFAULT_HANDOFF_TIMEOUT;
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                socket = args.next().map(PathBuf::from);
            }
            "--config" => {
                config = args.next().map(PathBuf::from);
            }
            "--handoff-from" => {
                handoff_from = args.next().map(PathBuf::from);
            }
            "--handoff-timeout-secs" => {
                let secs = args.next().unwrap_or_default();
                match secs.parse::<u64>() {
                    Ok(secs) => {
                        handoff_timeout = std::time::Duration::from_secs(secs.max(1));
                    }
                    Err(_) => {
                        eprintln!(
                            "arreo-server: --handoff-timeout-secs wants a number, got {secs:?}"
                        );
                        std::process::exit(2);
                    }
                }
            }
            "--version" => {
                // `update --server`'s `verify_runs` shells out to `<binary>
                // --version`, and the handoff's build reporting names it too —
                // so this must print a version and exit 0, never fall through
                // to serving. The string names the binary (`verify_runs`
                // refuses a candidate that is not a server by checking for it).
                println!("arreo-server {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--help" | "-h" => {
                println!("usage: arreo-server [--socket PATH] [--config PATH] [--handoff-from SOCKET] [--handoff-timeout-secs N] [--version]");
                println!("  --config  a TOML file whose [relay] section enables the relay session");
                println!("            (or set ARREO_CONFIG); without one the relay stays off");
                println!("  --handoff-from SOCKET");
                println!(
                    "            take over the daemon serving SOCKET without the socket going dead"
                );
                println!(
                    "            (T-0038 stage 1: the new process inherits the listener + lock)"
                );
                println!("  --handoff-timeout-secs N");
                println!("            bound each handoff wait (default 10)");
                println!("  --version print the server version and exit");
                return;
            }
            other => {
                eprintln!("arreo-server: unknown flag {other}");
                std::process::exit(2);
            }
        }
    }
    if handoff_from.is_some() && socket.is_some() {
        eprintln!("arreo-server: --socket and --handoff-from are exclusive (the handoff takes the socket it is given)");
        std::process::exit(2);
    }
    if let Some(from) = handoff_from {
        run_handoff(from, handoff_timeout).await;
    }
    let socket = socket.unwrap_or_else(default_socket);
    // Bootstrap the device authority before serving: a device-gated session
    // must never be possible against an authority that failed to load.
    let authority = match arreo_server::devices::load_for_socket(&socket) {
        Ok(authority) => authority,
        Err(e) => {
            eprintln!("arreo-server: device identity unavailable: {e}");
            eprintln!(
                "arreo-server: refusing to serve without a device authority \
                 (fix or remove the identity directory, then start again)"
            );
            std::process::exit(1);
        }
    };
    eprintln!(
        "arreo-server: device authority ready (root {}…, {} device(s))",
        &authority.root_fingerprint()[..16],
        authority.devices().len()
    );
    let daemon = arreo_server::Daemon::new(&socket);
    let registry = daemon.registry();
    let sessions = daemon.sessions();
    let socket_path = socket.clone();

    // This machine's trust ledger (T-0046), opened before anything is served: a
    // session must never be gated by a ledger that failed to load.
    let ledger = match arreo_server::TrustLedger::open(
        &arreo_server::db_path_for(&socket_path),
        &arreo_server::transport::root_key_path(),
        arreo_core::mesh::default_machine_name(),
    ) {
        Ok(ledger) => ledger,
        Err(e) => {
            eprintln!("arreo-server: cannot open this machine's trust ledger: {e}");
            eprintln!(
                "arreo-server: refusing to serve without it — every remote verb is gated \
                 by a grant, and a ledger that failed to load would refuse them all"
            );
            std::process::exit(1);
        }
    };
    // **The backfill runs before the first session is served.** Every device
    // pinned before this machine ran this code has a certificate and no grant, so
    // the strict rule would lock them all out — and the symptom would read as
    // "the machine stopped trusting me" rather than as a migration. It runs once,
    // ever: a marker that could re-run would silently undo an operator's
    // deliberate "revoke everything".
    {
        let existing: Vec<arreo_core::identity::DeviceId> = authority
            .devices()
            .into_iter()
            // Only devices that may still connect. A revoked device must not be
            // granted by a migration, or revocation would have a back door.
            .filter(|record| arreo_core::identity::revocation::may_connect(record).is_ok())
            .map(|record| record.id)
            .collect();
        match ledger.backfill_once(&existing) {
            Ok(granted) if granted.is_empty() => {}
            Ok(granted) => eprintln!(
                "arreo-server: trust ledger initialized — granted {} previously pinned \
                 device(s) access to this machine, matching how they were already treated; \
                 change any of them with `arreo machines trust`",
                granted.len()
            ),
            Err(e) => {
                eprintln!("arreo-server: cannot initialize the trust ledger: {e}");
                std::process::exit(1);
            }
        }
    }
    let ledger = arreo_core::mesh::SharedLedger::new(ledger);
    let authority = std::sync::Arc::new(std::sync::Mutex::new(authority));

    // Relay (T-0051). A configuration that enables the relay but is incomplete
    // is a loud exit rather than a silent no-op: an operator who asked for the
    // remote path and quietly did not get it has a bug they cannot see.
    let config_path = config.or_else(|| std::env::var_os("ARREO_CONFIG").map(PathBuf::from));
    if let Some(path) = config_path {
        match arreo_server::load_config(&path) {
            Ok(Some(settings)) => match arreo_server::own_identity() {
                Ok((device, cert)) => {
                    let context = arreo_server::RelayContext {
                        authority: std::sync::Arc::clone(&authority),
                        registry: std::sync::Arc::clone(&registry),
                        sessions: std::sync::Arc::clone(&sessions),
                        db: arreo_server::db_path_for(&socket_path),
                        device: std::sync::Arc::new(device),
                        cert: std::sync::Arc::new(cert),
                        // The name this machine asserts in the account's
                        // directory (T-0056). Cloned out of the settings before
                        // they are moved into the relay task, so the log line
                        // and the join request cannot disagree.
                        machine_name: settings.name.clone(),
                        ledger: ledger.clone(),
                    };
                    eprintln!(
                        "arreo-server: relay enabled for account {} via {}",
                        settings.account, settings.addr
                    );
                    tokio::spawn(arreo_server::relay_client::run(settings, context));
                }
                Err(e) => {
                    // **The one moment a stranger is stuck, so the message names the
                    // actual next step** (T-0066). It used to say "pair this machine
                    // first (arreo pair)", which reads as "run the admitting
                    // command" — and `arreo pair` on its own prints a code and then
                    // waits for a joiner, so following that advice costs the full
                    // TTL (300 s) and ends in "the pairing window closed". The
                    // machine that holds the account root can admit *itself*; the
                    // message has to say so, and say that the code must be used.
                    eprintln!("arreo-server: relay enabled but this machine has no identity: {e}");
                    eprintln!(
                        "arreo-server: a machine joins an account by pairing, which needs a code \
                         from a machine that already belongs. If this machine holds the account \
                         root (the key the relay registered the account with), it admits itself — \
                         in two steps, because the first one waits:"
                    );
                    eprintln!("arreo-server:   arreo pair                       # prints four words, then waits");
                    eprintln!(
                        "arreo-server:   arreo pair --join \"<the four words>\" --uri \"<the invite>\""
                    );
                    eprintln!(
                        "arreo-server: otherwise run `arreo pair` on a machine that already belongs \
                         and `arreo machines add <the four words> --uri <the invite>` here. \
                         (`enabled = false` runs this machine without a relay.)"
                    );
                    std::process::exit(1);
                }
            },
            Ok(None) => {}
            Err(e) => {
                eprintln!("arreo-server: relay configuration is unusable: {e}");
                std::process::exit(1);
            }
        }
    }

    // Remote transport (T-0023). Shipped posture is zero inbound ports, so this
    // listener exists only for the loopback test seam; the production remote
    // path is the daemon dialling out to a relay (T-0029).
    if let Some(addr) = arreo_server::transport::test_listen_addr() {
        let authority = std::sync::Arc::clone(&authority);
        let registry = std::sync::Arc::clone(&registry);
        let db = arreo_server::db_path_for(&socket_path);
        // The identity is loaded here, at the composition root, so a failure to
        // read the root key is reported once and the daemon keeps serving the
        // local socket either way.
        match arreo_server::transport::server_identity() {
            Ok(local) => {
                match arreo_server::transport::listen_on(
                    addr,
                    local,
                    authority,
                    ledger.clone(),
                    registry,
                    std::sync::Arc::clone(&sessions),
                    db,
                )
                .await
                {
                    Ok(bound) => eprintln!(
                        "arreo-server: remote transport listening on {bound} (test seam {})",
                        arreo_server::transport::TEST_LISTEN_ENV
                    ),
                    Err(e) => eprintln!("arreo-server: remote transport unavailable: {e}"),
                }
            }
            Err(e) => eprintln!("arreo-server: remote transport unavailable: {e}"),
        }
    }

    eprintln!("arreo-server: serving on {}", socket.display());
    tokio::select! {
        result = daemon.serve() => {
            match result {
                Ok(()) => {
                    // The handoff cut: the accept loop ended after commit, the
                    // incoming daemon is serving on the inherited listener, and
                    // the socket file stays. No drain, no unlink — this is a
                    // handover, not a shutdown. No destructor may run.
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("arreo-server: {e}");
                    std::process::exit(1);
                }
            }
        }
        signal = shutdown_signal() => {
            eprintln!("arreo-server: {signal} received, draining...");
            let drained = drain(&registry).await;
            // Persist the final topology (post-T-0018: restart restores it).
            let panes: Vec<(String, std::sync::Arc<arreo_core::pty::Pane>)> = registry
                .read()
                .await
                .iter()
                .map(|(id, entry)| (id.clone(), std::sync::Arc::clone(&entry.pane)))
                .collect();
            let db = arreo_server::db_path_for(&socket_path);
            if let Err(e) = arreo_server::snapshot(&panes, &db) {
                eprintln!("arreo-server: final snapshot failed: {e}");
            }
            eprintln!(
                "arreo-server: drained {drained} pane(s), exiting cleanly \
                 (children keep running; restart restores from {})",
                db.display()
            );
            let _ = std::fs::remove_file(&socket_path);
            std::process::exit(0);
        }
    }
}

/// Wait for SIGTERM (service stop) or SIGINT (Ctrl-C). Returns the name.
async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler installs");
        let mut int = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .expect("SIGINT handler installs");
        tokio::select! {
            _ = term.recv() => "SIGTERM",
            _ = int.recv() => "SIGINT",
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await.ok();
        "CTRL"
    }
}

/// Final drain pass: touch every pane once (a `drain()` flushes committed
/// output through the ring buffer) with an overall deadline. Returns panes
/// flushed. Never hangs past `SHUTDOWN_DEADLINE` — the process exits either
/// way (this function is not where the deadline is enforced; the `select!`
/// arm completes and `exit(0)` runs unconditionally after).
async fn drain(registry: &arreo_server::daemon::Registry) -> usize {
    let panes: Vec<_> = registry.read().await.values().cloned().collect();
    let deadline = std::time::Instant::now() + SHUTDOWN_DEADLINE;
    let mut flushed = 0;
    for pane in panes {
        if std::time::Instant::now() >= deadline {
            break;
        }
        // A drain flushes committed output; the lines themselves live in the
        // pane (and die with the daemon pre-T-0018 — documented, not hidden).
        let _ = pane.pane.drain();
        flushed += 1;
    }
    flushed
}

fn default_socket() -> PathBuf {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime).join("arreo.sock");
    }
    let uid = libc_uid();
    std::env::temp_dir().join(format!("arreo-{uid}.sock"))
}

/// The incoming side of a live handoff (T-0038 stage 1): take over the daemon
/// serving `socket` without the socket ever going dead. Never returns — on
/// success the process keeps serving (it never exits), on failure it prints
/// the reason on stderr and exits 1 (usage errors already exited 2 in `main`).
///
/// 1. Connect to the client socket, Hello/Welcome, send the handoff request,
///    read `HandoffReady` (a typed `Error` here is a refusal — the outgoing
///    daemon keeps serving, and this process exits 1 saying so).
/// 2. Connect to `<socket>.handoff`, receive the listener, then the lock.
/// 3. Build the daemon around the inherited listener and lock: no bind, no
///    `try_lock`. The descriptor becomes a non-blocking tokio listener first.
/// 4. Verify the inheritance (the lock path reads as held — this replaces the
///    acquire and proves the descriptor really carried the lock), then serve.
/// 5. Once genuinely accepting on the inherited listener, commit (close the
///    handoff connection → EOF) and print `handoff complete` with the new pid.
/// 6. Keep serving. The process never exits on success — a child that exits
///    *is* the failure signal, and the launcher polls for a new pid on the
///    socket rather than parsing this line.
async fn run_handoff(socket: PathBuf, timeout: std::time::Duration) -> ! {
    let detail = run_handoff_inner(&socket, timeout).await;
    eprintln!("arreo-server: handoff failed: {detail}");
    eprintln!(
        "arreo-server: the old daemon is still serving {}",
        socket.display()
    );
    std::process::exit(1);
}

async fn run_handoff_inner(socket: &std::path::Path, timeout: std::time::Duration) -> String {
    use arreo_core::proto::{codec, Message, VERSION};
    use std::io::Write;

    let fail = |detail: String| -> String { detail };
    // 1. The client socket: Hello→Welcome, then the request.
    let mut stream = match std::os::unix::net::UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(e) => {
            return fail(format!(
                "cannot reach the daemon at {}: {e}",
                socket.display()
            ))
        }
    };
    if let Err(e) = stream.set_read_timeout(Some(timeout)) {
        return fail(format!("cannot set a read timeout: {e}"));
    }
    if let Err(e) = stream.set_write_timeout(Some(timeout)) {
        return fail(format!("cannot set a write timeout: {e}"));
    }
    let hello = Message::Hello {
        v: VERSION,
        client: "arreo-server-handoff".to_string(),
        wants: vec![VERSION],
    };
    let frame = match codec::encode_frame(&hello) {
        Ok(frame) => frame,
        Err(e) => return fail(format!("cannot encode Hello: {e}")),
    };
    if let Err(e) = stream.write_all(&frame) {
        return fail(format!("cannot send Hello: {e}"));
    }
    let welcome = match read_one(&mut stream) {
        Ok(welcome) => welcome,
        Err(e) => return fail(format!("no Welcome: {e}")),
    };
    let _agreed = match welcome {
        Message::Welcome { v, .. } => v,
        Message::Error { message, .. } => return fail(format!("handshake refused: {message}")),
        other => return fail(format!("handshake failed: unexpected {other:?}")),
    };
    let request = Message::Handoff {
        v: VERSION,
        protocol: VERSION,
        build: env!("CARGO_PKG_VERSION").to_string(),
    };
    let frame = match codec::encode_frame(&request) {
        Ok(frame) => frame,
        Err(e) => return fail(format!("cannot encode Handoff: {e}")),
    };
    if let Err(e) = stream.write_all(&frame) {
        return fail(format!("cannot send Handoff: {e}"));
    }
    let ready = match read_one(&mut stream) {
        Ok(ready) => ready,
        Err(e) => return fail(format!("no HandoffReady: {e}")),
    };
    let (agreed, old_protocol, panes) = match ready {
        Message::HandoffReady {
            protocol,
            server_protocol,
            panes,
            ..
        } => (protocol, server_protocol, panes),
        Message::Error { message, .. } => {
            // A refusal: the outgoing daemon audited it and keeps serving — a
            // deferred update, never a failure of the running machine.
            return fail(format!(
                "the outgoing daemon refused the handoff: {message}"
            ));
        }
        other => return fail(format!("handoff failed: unexpected {other:?}")),
    };
    // 2. The dedicated connection: listener first, then the lock — the order
    // is the contract (stage 2 adds panes after the lock).
    let handoff_path = arreo_server::handoff::handoff_path_for(socket);
    let deadline = std::time::Instant::now() + timeout;
    let transfer = loop {
        match std::os::unix::net::UnixStream::connect(&handoff_path) {
            Ok(stream) => break stream,
            Err(_) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => {
                return fail(format!(
                    "cannot reach the transfer socket {}: {e}",
                    handoff_path.display()
                ));
            }
        }
    };
    let _ = transfer.set_read_timeout(Some(timeout));
    let listener_fd = match arreo_server::handoff::recv_one(&transfer, timeout) {
        Ok(fd) => fd,
        Err(e) => return fail(format!("the listener descriptor did not arrive: {e}")),
    };
    let lock_fd = match arreo_server::handoff::recv_one(&transfer, timeout) {
        Ok(fd) => fd,
        Err(e) => return fail(format!("the lock descriptor did not arrive: {e}")),
    };
    // 3. Build the daemon around the inherited listener and lock: no bind, no
    // `try_lock`. The descriptor becomes a non-blocking tokio listener first.
    let std_listener = arreo_server::handoff::std_listener_from_fd(listener_fd);
    if let Err(e) = std_listener.set_nonblocking(true) {
        return fail(format!(
            "cannot make the inherited listener non-blocking: {e}"
        ));
    }
    let tokio_listener = match tokio::net::UnixListener::from_std(std_listener) {
        Ok(listener) => listener,
        Err(e) => return fail(format!("cannot adopt the inherited listener: {e}")),
    };
    let lock_path = arreo_server::persist::lock_path_for(socket);
    let lock = arreo_core::lock::ExclusiveLock::inherited(lock_fd, lock_path.clone());
    //
    // 4. Verify the inheritance before serving: the lock path must read as
    // held. This replaces the acquire and proves the descriptor really carried
    // the lock — serving on a free lock would fork the world into two daemons.
    if !arreo_core::lock::ExclusiveLock::is_held(&lock_path) {
        return fail(format!(
            "the inherited lock for {} reads as free — refusing to serve",
            socket.display()
        ));
    }
    // 5. Start accepting on the inherited listener, in the background: the
    // commit must only happen once this process is genuinely accepting — not
    // merely after the descriptors arrived — because the launcher treats the
    // cut as done when a new pid answers on the socket. `serve_inherited`
    // re-checks the lock itself; the check above is the early, loud refusal
    // before any task is spawned.
    let daemon = arreo_server::Daemon::new(socket);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
    let server_task = tokio::spawn(async move {
        // Readiness means "the accept loop owns the listener and is about to
        // accept" (the signal fires inside `serve_on`, after the lock check,
        // before the first `accept`). The kernel queues connects on the
        // inherited backlog from the moment the dup exists — so by the time
        // the outgoing daemon exits on our commit, this loop is the thing
        // draining them. Committing before accepting would drop connects into
        // a socket nobody drains: the half-dead state this mechanism exists
        // to prevent.
        let _ = daemon
            .serve_inherited(tokio_listener, lock, Some(ready_tx))
            .await;
    });
    // Wait until the accept loop is running before committing: the outgoing
    // daemon exits on our commit, so this ordering is the cut itself.
    let _ = ready_rx.await;
    // 6. Commit: close our side of the transfer connection (EOF for the
    // outgoing daemon's `wait_for_commit`), then announce the cut with the
    // new pid — the only place an operator can see it, given no `status`
    // verb exists. Printed only now, after the accept loop is running on
    // the inherited listener — never merely when the descriptors arrived.
    drop(transfer);
    eprintln!(
        "arreo-server: handoff complete (protocol {old_protocol} -> {agreed}, panes {panes}, pid {})",
        std::process::id()
    );
    // Keep serving: success means this process never exits. A server task that
    // ends (its listener errored fatally) is a failure — report it and exit 1
    // rather than idling as a pid that answers nothing.
    match server_task.await {
        Ok(()) => fail("the inherited listener failed".to_string()),
        Err(e) => fail(format!("the serving task failed: {e}")),
    }
}

/// Read exactly one framed message on a blocking stream with a read timeout.
fn read_one(
    stream: &mut std::os::unix::net::UnixStream,
) -> Result<arreo_core::proto::Message, String> {
    use arreo_core::proto::codec;
    use std::io::Read;
    let mut acc = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match codec::decode_frame(&acc) {
            Ok((message, _)) => return Ok(message),
            Err(codec::CodecError::Truncated { .. }) => {}
            Err(e) => return Err(format!("cannot decode the reply: {e}")),
        }
        let n: usize = stream.read(&mut chunk).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            return Err("the daemon closed the connection".to_string());
        }
        acc.extend_from_slice(&chunk[..n]);
    }
}

#[cfg(unix)]
fn libc_uid() -> u32 {
    unsafe {
        extern "C" {
            fn getuid() -> u32;
        }
        getuid()
    }
}

#[cfg(not(unix))]
fn libc_uid() -> u32 {
    0
}
