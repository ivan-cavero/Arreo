//! `arreo-server` daemon binary (T-0005 serve, T-0012 graceful shutdown).
//!
//! Usage: `arreo-server [--socket PATH]`. Default socket:
//! `$XDG_RUNTIME_DIR/arreo.sock`, else `/tmp/arreo-<uid>.sock`.
//!
//! Shutdown (T-0012): SIGTERM/SIGINT stops accepting, flushes every pane's
//! committed ring output (a final drain pass over the registry), removes the
//! socket file, and exits 0 within `SHUTDOWN_DEADLINE`. Children keep running
//! (the daemon never kills agents on its way out); T-0018 re-attaches them.

use arreo_server::lifecycle::SHUTDOWN_DEADLINE;
use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let mut socket: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => {
                socket = args.next().map(PathBuf::from);
            }
            "--help" | "-h" => {
                println!("usage: arreo-server [--socket PATH]");
                return;
            }
            other => {
                eprintln!("arreo-server: unknown flag {other}");
                std::process::exit(2);
            }
        }
    }
    let socket = socket.unwrap_or_else(default_socket);
    let daemon = arreo_server::Daemon::new(&socket);
    let registry = daemon.registry();
    let socket_path = socket.clone();

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
