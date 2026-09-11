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
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                socket = args.next().map(PathBuf::from);
            }
            "--config" => {
                config = args.next().map(PathBuf::from);
            }
            "--help" | "-h" => {
                println!("usage: arreo-server [--socket PATH] [--config PATH]");
                println!("  --config  a TOML file whose [relay] section enables the relay session");
                println!("            (or set ARREO_CONFIG); without one the relay stays off");
                return;
            }
            other => {
                eprintln!("arreo-server: unknown flag {other}");
                std::process::exit(2);
            }
        }
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
                    };
                    eprintln!(
                        "arreo-server: relay enabled for account {} via {}",
                        settings.account, settings.addr
                    );
                    tokio::spawn(arreo_server::relay_client::run(settings, context));
                }
                Err(e) => {
                    eprintln!("arreo-server: relay enabled but this machine has no identity: {e}");
                    eprintln!(
                        "arreo-server: pair this machine first (arreo pair), or set \
                         enabled = false"
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
            if let Err(e) = result {
                eprintln!("arreo-server: {e}");
                std::process::exit(1);
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
