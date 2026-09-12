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
use std::sync::Arc;

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
                println!("            exit codes: 3 another handoff holds the lock (retry),");
                println!("            1 any other failure, 2 usage, 0 = handed on");
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

/// `--handoff-from`'s exit codes (documented in `--help`; `arreo update
/// --server` reads them off the child it spawned).
const HANDOFF_EXIT_FAILED: i32 = 1;
/// The refusal that is not a failure: **another handoff holds the one-handoff
/// lock**. The project's "in progress" code — the same value the updater's own
/// lock produces — because the machine is not broken, it is being handed over by
/// someone else right now and retrying is the whole remedy.
const HANDOFF_EXIT_BUSY: i32 = 3;

/// Why the incoming daemon gave up, and what the process exits with.
///
/// Two shapes rather than a bool, because "refused because a handoff is already
/// in progress" is a different answer from "refused": the first is a *deferred*
/// update the launcher reports as in-progress, the second a failure the operator
/// has to read.
enum HandoffFailure {
    /// Anything else: exit [`HANDOFF_EXIT_FAILED`].
    Failed(String),
    /// The outgoing daemon refused because the one-handoff lock is held: exit
    /// [`HANDOFF_EXIT_BUSY`].
    Busy(String),
}

impl HandoffFailure {
    fn exit_code(&self) -> i32 {
        match self {
            Self::Failed(_) => HANDOFF_EXIT_FAILED,
            Self::Busy(_) => HANDOFF_EXIT_BUSY,
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::Failed(detail) | Self::Busy(detail) => detail,
        }
    }
}

/// The incoming side of a live handoff (T-0038 stage 1): take over the daemon
/// serving `socket` without the socket ever going dead. Never returns — on
/// success the process keeps serving (it never exits), on failure it prints
/// the reason on stderr and exits 1 — or 3 when the refusal is another handoff
/// holding the lock ([`HANDOFF_EXIT_BUSY`]). Usage errors already exited 2 in
/// `main`.
///
/// 1. Connect to the client socket, Hello/Welcome, send the handoff request,
///    read `HandoffReady` (with the nonce; a typed `Error` here is a refusal —
///    the outgoing daemon keeps serving, and this process exits 1 saying so,
///    or 3 when the refusal names the one-handoff lock as held).
/// 2. Connect to `<socket>.handoff`, present the nonce, receive the listener,
///    then the lock.
/// 3. **Validate what arrived** before building anything on it: the listener
///    must be a listening stream socket bound to this socket's path, and the
///    lock must be the lock at `<socket>.lock`. A descriptor that is not what
///    it claims is refused, and the refusal is audited on this side too.
/// 4. Build the daemon around the inherited listener and lock: no bind, no
///    acquire.
/// 5. Start accepting, and wait — **bounded** — for the accept loop to own the
///    listener. A refusal from there is a failure: no commit byte is sent, and
///    this process exits 1 with the reason, so the outgoing daemon keeps
///    serving.
/// 6. Commit: one marker byte, sent only now that the listener is genuinely
///    accepting here (EOF is not a commit — see `handoff.rs`). What the outgoing
///    daemon learns from it is *authorisation* — that this process, which asked
///    for the handoff and holds the nonce, is committing — not proof that a
///    server is answerable: see `handoff::wait_for_commit` for the limit and why
///    a probe was rejected. Then keep serving: this process is the daemon now,
///    and it exits 0 only when a *later* handoff hands the socket on again. A
///    child that exits non-zero is the failure signal the launcher polls for.
async fn run_handoff(socket: PathBuf, timeout: std::time::Duration) -> ! {
    let failure = run_handoff_inner(&socket, timeout).await;
    eprintln!("arreo-server: handoff failed: {}", failure.detail());
    eprintln!(
        "arreo-server: the old daemon is still serving {}",
        socket.display()
    );
    std::process::exit(failure.exit_code());
}

async fn run_handoff_inner(
    socket: &std::path::Path,
    timeout: std::time::Duration,
) -> HandoffFailure {
    use arreo_core::proto::{codec, Message, VERSION};
    use std::io::Write;
    use std::os::unix::io::AsFd;

    // The store the outgoing daemon audits into; this side records its own
    // refusals there, so a failed handoff is never silent on the side that
    // knows the reason (the outgoing daemon only ever sees the connection end).
    let db = arreo_server::persist::db_path_for(socket);
    // Failures before the transfer begins (no daemon to talk to, a refusal)
    // are not this side's to record — the outgoing daemon answers and audits
    // them itself.
    let fail = |detail: String| -> HandoffFailure { HandoffFailure::Failed(detail) };
    // Failures once the descriptors are in flight are this side's: the reason
    // (a missing socket path, a lock that is not the lock) is knowable here and
    // nowhere else.
    let refuse = |detail: String| -> HandoffFailure {
        arreo_server::handoff::record_incoming_abort(&db, &detail);
        HandoffFailure::Failed(detail)
    };
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
        wants: arreo_core::proto::client_versions(),
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
    let (agreed, old_protocol, panes, nonce, carries_panes) = match ready {
        Message::HandoffReady {
            protocol,
            server_protocol,
            panes,
            nonce,
            manifest,
            ..
        } => (protocol, server_protocol, panes, nonce, manifest),
        Message::Error { message, .. } => {
            // A refusal: the outgoing daemon audited it and keeps serving — a
            // deferred update, never a failure of the running machine.
            let detail = format!("the outgoing daemon refused the handoff: {message}");
            // **One refusal has its own code.** The one-handoff lock being held
            // is not "the update failed" but "someone else is cutting the socket
            // over right now": exit 3, which `arreo update --server` reports as
            // an update in progress (the same code its own lock produces). Every
            // other refusal — a version outside the window, a transfer socket
            // that cannot be bound — stays exit 1.
            //
            // The distinction is a string match, and the coupling runs one way:
            // `arreo_server::handoff::BUSY_REFUSAL` is the wording the outgoing
            // daemon builds this detail from, so the two sides change together
            // or the deferred update starts reading as a failure. There is no
            // cheaper carrier: the reason arrives as a typed `Error`'s prose, and
            // adding a protocol variant for one exit code would be a wire change
            // to serve a local decision.
            return if arreo_server::handoff::is_busy_refusal(&message) {
                HandoffFailure::Busy(detail)
            } else {
                HandoffFailure::Failed(detail)
            };
        }
        other => return fail(format!("handoff failed: unexpected {other:?}")),
    };
    // No nonce means an outgoing daemon that speaks the older grammar (no
    // nonce, EOF as the commit). Refusing is the safe direction: the outgoing
    // daemon aborts and goes on serving, where guessing the grammar could
    // commit a cut nobody authorized.
    if nonce.len() != arreo_server::handoff::NONCE_BYTES {
        return fail(format!(
            "the outgoing daemon sent a {}-byte handoff nonce (want {})",
            nonce.len(),
            arreo_server::handoff::NONCE_BYTES
        ));
    }
    // 2. The dedicated connection: the nonce first, then the descriptors — the
    // order is the contract (stage 2 adds panes after the lock).
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
    let _ = transfer.set_write_timeout(Some(timeout));
    if let Err(e) = arreo_server::handoff::send_nonce(&transfer, &nonce) {
        return fail(format!("cannot present the handoff nonce: {e}"));
    }
    let listener_fd = match arreo_server::handoff::recv_one(&transfer, timeout) {
        Ok(fd) => fd,
        Err(e) => return fail(format!("the listener descriptor did not arrive: {e}")),
    };
    let lock_fd = match arreo_server::handoff::recv_one(&transfer, timeout) {
        Ok(fd) => fd,
        Err(e) => return fail(format!("the lock descriptor did not arrive: {e}")),
    };
    // 3. Validate what arrived, before anything is built on it. A descriptor in
    // the listener slot that is not *this* listener, and a descriptor in the
    // lock slot that is not *the* lock, are refusals: serving on either would
    // hand the daemon to a socket nobody reaches, or serve while holding
    // nothing.
    if let Err(detail) = arreo_server::handoff::validate_listener(listener_fd.as_fd(), socket) {
        return refuse(format!("the listener descriptor was refused: {detail}"));
    }
    let lock_path = arreo_server::persist::lock_path_for(socket);
    let lock = match arreo_core::lock::ExclusiveLock::inherited_checked(lock_fd, &lock_path) {
        Ok(lock) => lock,
        Err(e) => return refuse(format!("the lock descriptor was refused: {e}")),
    };
    // 3b. **The panes** (T-0038 stage 2): the manifest, the count, and one
    // master descriptor per entry — in that order, which is the grammar.
    //
    // A sender that does not carry panes is accepted only when it says it has
    // none. A stage-1 outgoing daemon exits on the commit and its children are
    // orphaned, so committing over a cut that has panes and no manifest would
    // kill every agent on the machine; with no panes there is nothing to lose
    // and the cut is exactly what stage 1 promised. That distinction is the
    // N−1 gate, and it lives in `HandoffReady::manifest`.
    let manifest = if carries_panes {
        let encoded = match arreo_server::handoff::recv_manifest(&transfer, timeout) {
            Ok(encoded) => encoded,
            Err(detail) => return refuse(format!("the pane manifest did not arrive: {detail}")),
        };
        let manifest = match arreo_core::proto::message::decode_manifest(&encoded) {
            Ok(manifest) => manifest,
            Err(e) => return refuse(format!("the pane manifest did not decode: {e}")),
        };
        let count = match arreo_server::handoff::recv_pane_count(&transfer, timeout) {
            Ok(count) => count,
            Err(detail) => return refuse(format!("the pane count did not arrive: {detail}")),
        };
        // **The count is validated against the manifest, never trusted as a
        // loop bound.** Fewer descriptors than entries means the sender lost a
        // pane (and the ones after the loss would be matched to the wrong
        // entries); more means descriptors nobody named. Either way it is a
        // refusal rather than a guess — and a refusal is safe, because the
        // outgoing daemon keeps serving and a retry is the remedy.
        if count != manifest.len() {
            return refuse(format!(
                "the transfer claims {count} pane descriptor(s) but its manifest names {}",
                manifest.len()
            ));
        }
        manifest
    } else {
        if panes > 0 {
            return refuse(format!(
                "the outgoing daemon is not transferring its {panes} pane(s) \
                 (an older build: committing would orphan every agent)"
            ));
        }
        Vec::new()
    };
    // Every check that can still refuse runs **before** the first adoption, and
    // every adoption before the commit: nothing here has read a byte from an
    // inherited terminal yet, so an abort at any point below leaves the outgoing
    // daemon with exactly the panes it had.
    let mut adopted: Vec<(String, Arc<arreo_server::daemon::PaneEntry>)> =
        Vec::with_capacity(manifest.len());
    for entry in &manifest {
        // The guard is validated here but *adopted* after the commit (see
        // below): a `Guard`'s `Drop` removes the cgroup, so adopting one and
        // then aborting would strip a live agent's memory ceiling while
        // reporting that nothing happened. Checking the path now keeps the
        // refusal on the safe side of the cut.
        //
        // The check is the same rule `Guard::reopen` will apply after the
        // commit (F2, stage-2 review): the path must be a direct child of this
        // machine's cgroup scope and read like a cgroup. An arbitrary empty
        // directory used to be adopted, reported as a guard, and `rmdir`'d
        // when the pane died — a hostile outgoing daemon's path to deleting a
        // directory and serving an agent without its ceiling, silently.
        if let Some(path) = &entry.guard_path {
            if let Err(e) = arreo_core::enforce::Guard::validate(std::path::Path::new(path)) {
                return refuse(format!(
                    "pane {:?} arrived with a guard path that cannot be adopted: {e}",
                    entry.id
                ));
            }
        }
        let fd = match arreo_server::handoff::recv_one(&transfer, timeout) {
            Ok(fd) => fd,
            Err(e) => {
                return refuse(format!(
                    "the descriptor for pane {:?} did not arrive: {e}",
                    entry.id
                ))
            }
        };
        // **Paused**: the pump parks before its first read, so this process
        // consumes nothing until the cut is committed. Bytes the outgoing
        // daemon never read stay in the terminal's buffer for whoever ends up
        // owning the pane.
        let seed = arreo_core::pty::AdoptSeed {
            child_pid: entry.child_pid,
            size: arreo_core::pty::adopt::AdoptSize {
                cols: entry.cols,
                rows: entry.rows,
            },
            spec: arreo_core::pty::SpawnSpec {
                program: entry.program.clone(),
                args: entry.args.clone(),
            },
            scrollback: arreo_core::pty::Scrollback {
                lines: entry.lines.clone(),
                pending: entry.pending.clone(),
                raw: entry.raw.clone(),
                raw_truncated: entry.raw_truncated,
                dropped: entry.dropped,
                dropped_bytes: entry.dropped_bytes,
            },
        };
        let pane = match arreo_core::pty::Pane::adopt_seeded(fd, seed) {
            Ok(pane) => Arc::new(pane),
            Err(e) => return refuse(format!("pane {:?} was refused: {e}", entry.id)),
        };
        // The entry is rebuilt, not transferred: a fresh engine fed the
        // transferred journal (which is why the journal travels), a fresh
        // sampler, and the alert episode reconstructed through `AlertState`'s
        // own rule. The guard joins it after the commit.
        adopted.push((
            entry.id.clone(),
            Arc::new(arreo_server::daemon::PaneEntry::adopted(
                pane,
                entry.alert.as_deref(),
                entry.alert_line.as_deref(),
                entry.kill_on_breach,
            )),
        ));
    }
    // 4. Build the daemon around the inherited listener and lock: no bind, no
    // acquire. The descriptor becomes a non-blocking tokio listener first.
    let std_listener = arreo_server::handoff::std_listener_from_fd(listener_fd);
    if let Err(e) = std_listener.set_nonblocking(true) {
        return refuse(format!(
            "cannot make the inherited listener non-blocking: {e}"
        ));
    }
    let tokio_listener = match tokio::net::UnixListener::from_std(std_listener) {
        Ok(listener) => listener,
        Err(e) => return refuse(format!("cannot adopt the inherited listener: {e}")),
    };
    // 5. Start accepting on the inherited listener, in the background: the
    // commit must only happen once this process is genuinely accepting — not
    // merely after the descriptors arrived. `serve_inherited` reports either
    // readiness or the reason it refused through the channel, and **an `Err`
    // there is a failure**: committing on it would exit the outgoing daemon
    // with nobody serving, which is the half-dead state this whole mechanism
    // exists to prevent.
    let daemon = arreo_server::Daemon::new(socket);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<Result<(), String>>();
    // The entries are cloned into the serving task (an `Arc` clone each) and
    // kept here too: this thread is the one that starts the pumps, and it may
    // only do that once the cut is committed.
    let serving_panes = adopted.clone();
    let server_task = tokio::spawn(async move {
        // Readiness means "the accept loop owns the listener and is about to
        // accept" (the signal fires inside `serve_on`, after the lock check,
        // before the first `accept`). The kernel queues connects on the
        // inherited backlog from the moment the dup exists — so by the time
        // the outgoing daemon exits on our commit, this loop is the thing
        // draining them.
        let _ = daemon
            .serve_inherited(tokio_listener, lock, serving_panes, Some(ready_tx))
            .await;
    });
    // Bounded (F8): an incoming daemon that never starts accepting must not
    // hold the transfer open for ever, because the outgoing daemon is waiting
    // on the same connection.
    match tokio::time::timeout(timeout, ready_rx).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(reason))) => {
            return refuse(format!("the inherited listener was refused: {reason}"))
        }
        Ok(Err(_)) => {
            return refuse(
                "the serving task ended before it started accepting on the inherited listener"
                    .to_string(),
            )
        }
        Err(_) => {
            return refuse(format!(
                "the inherited listener did not start accepting within {timeout:?}"
            ))
        }
    }
    // 6. Commit: the marker byte, now that the accept loop owns the listener.
    // The outgoing daemon requires this byte; it does **not** read the close of
    // this connection as a commit, so a failure here leaves it serving.
    if let Err(e) = arreo_server::handoff::send_commit(&transfer) {
        return refuse(format!("cannot send the commit marker: {e}"));
    }
    drop(transfer);
    // 7. **Now the panes are ours, and only now.** Two things follow the commit
    // and nothing may do them earlier:
    //
    // - **The guards are re-opened.** `Guard`'s `Drop` removes the cgroup, so
    //   adopting one before the commit and then aborting would `rmdir` the group
    //   of a pane the outgoing daemon is still serving — silently removing a
    //   live agent's memory ceiling while reporting that the handoff did not
    //   happen. After the commit this process owns the pane, and removal-on-drop
    //   is exactly right. Nothing removes the group on the outgoing side: that
    //   process exits via `std::process::exit`, which runs no destructors, so the
    //   group outlives it and this daemon is its genuine owner.
    // - **The pumps start.** They have been parked since adoption, which is what
    //   kept this process from consuming a byte before the cut. From here the
    //   bytes that arrived while parked are read from the terminals' buffers, so
    //   the criterion-2 case (output written *during* the pause) arrives rather
    //   than being lost.
    for (id, entry) in &adopted {
        if let Some(path) = manifest
            .iter()
            .find(|pane| &pane.id == id)
            .and_then(|pane| pane.guard_path.as_ref())
        {
            match arreo_core::enforce::Guard::reopen(std::path::PathBuf::from(path)) {
                Ok(guard) => entry.adopt_guard(guard),
                // Committed, so there is no safe way back: refusing here would
                // leave the machine with no serving daemon at all, which is the
                // half-dead state ADR 0021 §2 exists to prevent. What is *not*
                // acceptable is silence — a pane served without the ceiling it
                // was running under is a security-relevant regression, and it is
                // said out loud and on the audit trail rather than hidden.
                Err(e) => {
                    eprintln!(
                        "arreo-server: pane {id} lost its enforcement guard across the handoff \
                         ({path}): {e} — serving it WITHOUT its budget"
                    );
                    arreo_server::handoff::record_incoming_abort(
                        &db,
                        &format!("pane {id} lost its guard across the handoff: {e}"),
                    );
                }
            }
        }
        // F6 (stage-2 review): the geometry repair, moved here from the
        // adoption — a 0×0 inherited terminal is the sender's numbers to set,
        // but only once this process owns the pane. Before the commit it would
        // mutate a terminal the outgoing daemon gets back on an abort; after
        // the commit it is exactly the repair the pane needs, and a failure is
        // reported, never a refusal (there is no way back). The manifest's
        // claim is what the sender reported for this very pane.
        if let Some(claimed) = manifest.iter().find(|pane| &pane.id == id) {
            if let Err(e) = entry.pane.repair_geometry(claimed.cols, claimed.rows) {
                eprintln!(
                    "arreo-server: pane {id} could not repair its geometry after the handoff: {e}"
                );
            }
        }
        entry.pane.resume();
    }
    let adopted_count = adopted.len();
    // **The local handles are dropped.** The registry owns these entries now,
    // and an `Arc` kept alive here would keep each `PaneEntry` — and with it each
    // `Guard` — from ever dropping: `Guard::drop` removes the cgroup, so a pane
    // killed after the cut would leave its group behind, which is exactly the
    // leak `a_guard_survives_the_cut` asserts against.
    drop(adopted);
    eprintln!(
        "arreo-server: handoff complete (protocol {old_protocol} -> {agreed}, panes {panes}, \
         adopted {adopted_count}, pid {})",
        std::process::id()
    );
    // Keep serving: success means this process never exits. A server task that
    // ends is a *later handoff* — this daemon handed the socket on, exactly as
    // the outgoing daemon does, and exits 0 the same way. A real failure (the
    // listener erroring) is an `Err` and is reported as one; `Ok(())` here is
    // never a failure, and calling it one printed "the inherited listener
    // failed" for a cut that succeeded.
    match server_task.await {
        Ok(()) => {
            eprintln!(
                "arreo-server: handed the socket on to a later handoff (pid {})",
                std::process::id()
            );
            std::process::exit(0);
        }
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
