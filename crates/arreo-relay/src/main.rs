//! `arreo-relay` binary: serve the pairing mailbox (T-0024).
//!
//! Usage:
//!   `arreo-relay --pairing-socket /run/arreo/relay.sock`      (local, default)
//!   `arreo-relay --pairing-tcp 0.0.0.0:8770`                  (network)
//!
//! Both listeners can run at once and share one mailbox. The pairing mailbox
//! carries opaque SPAKE2 flights: it never sees the code (which lives only on
//! the two devices) and it cannot forge a flight (the confirmation MAC is keyed
//! by the shared secret the code produces). What it *does* enforce — one open
//! per session id, one flight per slot, a fixed lifetime — is what keeps a
//! wrong guess from being cheap.
//!
//! Routing, the durable inbox and presence are T-0029+; this binary is the
//! pairing half that those will grow from.

use std::path::PathBuf;
use std::sync::Arc;

fn main() {
    let mut socket: Option<PathBuf> = None;
    let mut tcp: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pairing-socket" => socket = args.next().map(PathBuf::from),
            "--pairing-tcp" => tcp = args.next(),
            "--help" | "-h" => {
                println!("usage: arreo-relay [--pairing-socket PATH] [--pairing-tcp HOST:PORT]");
                println!("  at least one listener is required; both may be given");
                return;
            }
            other => {
                eprintln!("arreo-relay: unknown flag {other}");
                std::process::exit(2);
            }
        }
    }
    if socket.is_none() && tcp.is_none() {
        eprintln!("arreo-relay: nothing to serve — pass --pairing-socket and/or --pairing-tcp");
        std::process::exit(2);
    }

    let mailbox = Arc::new(arreo_relay::Mailbox::new());

    // One thread per listener: each is a simple blocking accept loop, and they
    // share the mailbox behind its own lock.
    let mut handles = Vec::new();
    if let Some(path) = socket {
        let mailbox = Arc::clone(&mailbox);
        eprintln!("arreo-relay: pairing mailbox on unix://{}", path.display());
        handles.push(std::thread::spawn(move || {
            if let Err(e) = arreo_relay::pairing::serve_unix(&path, mailbox) {
                eprintln!("arreo-relay: unix listener died: {e}");
                std::process::exit(1);
            }
        }));
    }
    if let Some(addr) = tcp {
        let mailbox = Arc::clone(&mailbox);
        eprintln!("arreo-relay: pairing mailbox on tcp://{addr}");
        handles.push(std::thread::spawn(move || {
            if let Err(e) = arreo_relay::pairing::serve_tcp(&addr, mailbox) {
                eprintln!("arreo-relay: tcp listener died: {e}");
                std::process::exit(1);
            }
        }));
    }
    for handle in handles {
        let _ = handle.join();
    }
}
