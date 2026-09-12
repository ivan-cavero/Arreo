//! Server live handoff, stage 1 (T-0038): one daemon process takes over the
//! socket from a running one **without the socket ever going dead**.
//!
//! ## Topology
//!
//! The request is negotiated on the existing client socket (so it can be
//! refused with a typed error and audited), but **all file descriptors travel
//! on a dedicated connection** to `<socket>.handoff`, which the outgoing daemon
//! binds only for the duration of a handoff and unlinks when it ends. Why:
//! ancillary data on a stream socket is a *barrier* — a descriptor rides on
//! whatever byte the receiver reads next, so if a buffered async reader ever
//! consumes that byte the kernel **discards the descriptor**. On the dedicated
//! connection only `send_fd`/`recv_fd` traffic exists, so the hazard cannot
//! arise; on the client socket it could, and a silently lost listener
//! descriptor means the daemon exits and the socket goes dead — the exact
//! "half-dead" state ADR 0021 §2 exists to prevent.
//!
//! ## The descriptor grammar
//!
//! The dedicated connection carries **no framing other than the marker bytes
//! the descriptor helpers already define** (`FD_PASS_BYTE = 0x01`, one per
//! descriptor). The outgoing daemon sends exactly two descriptors in a fixed
//! order — the **listener** first, then the **lock** (stage 2 adds panes after
//! the lock) — and the incoming daemon receives exactly two. `commit` needs no
//! byte of its own: the incoming daemon's `commit` is observed by the outgoing
//! daemon as **EOF on the handoff connection** — the incoming daemon closes its
//! side once it is genuinely accepting on the inherited listener, and the
//! outgoing daemon's third `recv_fd` (which expects no descriptor) returns
//! `FdTransferError::PeerClosed` for exactly that. An incoming daemon that dies
//! before committing reads as `PeerClosed`/`TimedOut` at whatever step the
//! outgoing daemon reached — an abort, and the outgoing daemon keeps serving.
//!
//! ## Ordering (outgoing)
//!
//! 1. Receive the request on the client socket; validate the version against
//!    the *incoming* daemon's window; audit a refusal if refused.
//! 2. Bind `<socket>.handoff`; reply `HandoffReady { v, protocol, panes }`.
//! 3. Accept one connection there with a deadline ([`DEFAULT_HANDOFF_TIMEOUT`]);
//!    unlink the path as soon as it is accepted.
//! 4. Send the listener descriptor, then the lock descriptor.
//! 5. Wait for the incoming daemon's commit (EOF).
//! 6. Write the audit row (`handoff <from> -> <to> panes=0`).
//! 7. Stop accepting — the accept loop ends, which drops *this* process's
//!    listener fd (the incoming daemon holds a dup, and the **socket file must
//!    NOT be unlinked**).
//! 8. `std::process::exit(0)`. **No destructor may run.**
//!
//! ## Ordering (incoming)
//!
//! 1. Connect to the client socket, Hello/Welcome, send the request, read
//!    `HandoffReady`.
//! 2. Connect to `<socket>.handoff`, receive the listener, then the lock.
//! 3. Build the daemon around the **inherited** listener and lock: no bind, no
//!    `try_lock`. The descriptor is made non-blocking before it becomes a
//!    `tokio::net::UnixListener`.
//! 4. **Verify the inheritance**: open the lock path afresh and assert the lock
//!    is held (this replaces the acquire and proves the descriptor really
//!    carried the lock).
//! 5. Start accepting on the inherited listener.
//! 6. Send `commit` (close the handoff connection → EOF), then keep serving.
//!
//! ## What is NOT transferred
//!
//! The store is opened per operation, never held, so there is no checkpoint to
//! do and no handle to transfer — a second process opening the DB is an
//! existing, supported situation. The incoming daemon loads the authority and
//! trust ledger from disk exactly as a fresh start does, and starts **its own**
//! relay session (T-0060's displacement rule makes that safe). No pane process
//! is spawned, waited for, or signalled here: stage 1 moves descriptors and
//! stops accepting; it touches no `Pane` (stage 2 does).

use std::os::unix::io::{BorrowedFd, FromRawFd, IntoRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long the outgoing daemon waits for the incoming daemon to connect to
/// `<socket>.handoff`, for the commit after the descriptors, and for each
/// descriptor on the incoming side. Generous because the alternative to waiting
/// is a failed update, and a slow machine under load must not turn a working
/// handoff into a refused one.
pub const DEFAULT_HANDOFF_TIMEOUT: Duration = Duration::from_secs(10);

/// `<socket>.handoff` — the dedicated descriptor-transfer socket, bound only
/// for the duration of a handoff and unlinked when it ends.
#[must_use]
pub fn handoff_path_for(socket: &Path) -> PathBuf {
    arreo_core::identity::authority::sidecar(socket, ".handoff")
}

/// What the outgoing daemon concluded. Returned to the session loop so the
/// single place that owns the accept loop can act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffOutcome {
    /// The cut happened: descriptors sent, commit observed, audit row written.
    /// The caller must stop accepting and `std::process::exit(0)`.
    Committed,
    /// The incoming daemon never finished (died, timed out, or the version was
    /// refused before any descriptor moved). The caller keeps serving; the
    /// `.handoff` file is already gone.
    Aborted,
}

/// The outgoing side: validate `incoming_protocol`, bind the handoff socket,
/// move the listener + lock descriptors, wait for commit, audit, and report
/// whether the caller should exit.
///
/// `listener_fd` and `lock_fd` are borrowed: sending over `SCM_RIGHTS` dups
/// them into the peer, so this side keeps (and the caller drops) its own.
/// `panes` is the live pane count for the `HandoffReady` reply and the audit
/// row (stage 1 always reports the real count; it moves no pane).
///
/// A refusal (incoming protocol outside this daemon's N−1 window) writes the
/// `handoff.refuse` audit row and returns `Ok(Aborted)` with **no reply sent
/// and no `.handoff` file created** — the session loop answers the typed
/// `Error` itself, and the daemon keeps serving.
pub fn check_incoming_protocol(
    incoming_protocol: u32,
    db: &Path,
    incoming_build: &str,
) -> Result<u32, String> {
    match arreo_core::proto::codec::negotiate(
        arreo_core::proto::VERSION,
        std::slice::from_ref(&incoming_protocol),
    ) {
        Ok(agreed) => Ok(agreed),
        Err(e) => {
            let detail = format!(
                "handoff refused: incoming protocol {incoming_protocol} (build {incoming_build}) \
                 outside this daemon's window (speaks {}): {e}",
                arreo_core::proto::VERSION,
            );
            record_refusal(db, &detail);
            Err(detail)
        }
    }
}

/// Write the `handoff.refuse` audit row. Best-effort like every background
/// daemon write: a store failure must not take down a daemon that is — by
/// definition of this path — still serving.
fn record_refusal(db: &Path, detail: &str) {
    if let Ok(store) = arreo_core::store::SessionStore::open(db) {
        let _ = store.record(&arreo_core::store::AuditEvent {
            device: "daemon".to_string(),
            agent: String::new(),
            prompt: String::new(),
            detail: Some(detail.to_string()),
            ..arreo_core::store::AuditEvent::new(
                arreo_core::store::actions::HANDOFF_REFUSE,
                arreo_core::store::AuditKind::Unknown,
                arreo_core::store::AuditOutcome::Refused,
                now_ms(),
            )
        });
    }
}

/// Write the `handoff` audit row: `handoff <from> -> <to> panes=<n>`.
/// Written by the outgoing daemon after commit, before it exits.
pub fn record_handoff(db: &Path, from: u32, to: u32, panes: u64, pid: u32) {
    if let Ok(store) = arreo_core::store::SessionStore::open(db) {
        let _ = store.record(&arreo_core::store::AuditEvent {
            device: "daemon".to_string(),
            agent: pid.to_string(),
            prompt: String::new(),
            detail: Some(format!("handoff {from} -> {to} panes={panes} pid={pid}")),
            ..arreo_core::store::AuditEvent::new(
                arreo_core::store::actions::HANDOFF,
                arreo_core::store::AuditKind::Unknown,
                arreo_core::store::AuditOutcome::Ok,
                now_ms(),
            )
        });
    }
}

/// Send one descriptor over an already-connected handoff stream.
pub fn send_one(
    socket: &std::os::unix::net::UnixStream,
    fd: BorrowedFd<'_>,
) -> std::io::Result<()> {
    arreo_core::pty::adopt::send_fd(socket, fd).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            format!("handoff send descriptor: {e}"),
        )
    })
}

/// Receive one descriptor, waiting at most `timeout` in total.
pub fn recv_one(
    socket: &std::os::unix::net::UnixStream,
    timeout: Duration,
) -> Result<OwnedFd, arreo_core::pty::adopt::FdTransferError> {
    arreo_core::pty::adopt::recv_fd(socket, timeout)
}

/// Wait for the incoming daemon's commit: EOF on the handoff connection with
/// no further descriptor. `recv_fd` reports that as `PeerClosed` on a stream
/// socket, which is the only success here — a descriptor at this step would
/// mean a peer speaking a different grammar, and a timeout means it died.
pub fn wait_for_commit(
    socket: &std::os::unix::net::UnixStream,
    timeout: Duration,
) -> Result<(), String> {
    match arreo_core::pty::adopt::recv_fd(socket, timeout) {
        Err(arreo_core::pty::adopt::FdTransferError::PeerClosed) => Ok(()),
        Err(e) => Err(format!("handoff commit not observed: {e}")),
        Ok(_) => Err("handoff commit not observed: peer sent an unexpected descriptor".to_string()),
    }
}

/// Turn an inherited listener descriptor into a blocking `std` listener. The
/// caller makes it non-blocking before `tokio::net::UnixListener::from_std`.
/// Takes ownership: the returned listener closes the descriptor on drop, which
/// is the correct lifetime (the daemon holds the socket until it exits).
///
/// # Soundness
///
/// `fd` is owned — it arrived over `SCM_RIGHTS`, so this process holds the only
/// reference to this descriptor number. `into_raw_fd` transfers that ownership
/// to the listener without duplicating or closing anything, and the listener
/// takes over closing it exactly once. The descriptor is a bound Unix listener
/// (the outgoing daemon sent its own), so interpreting it as one performs no
/// I/O and cannot misinterpret the handle.
pub fn std_listener_from_fd(fd: OwnedFd) -> std::os::unix::net::UnixListener {
    let raw = fd.into_raw_fd();
    // SAFETY: `raw` came from an owned descriptor this line just consumed, so
    // it is valid, open, and uniquely owned — the three conditions
    // `from_raw_fd` requires.
    unsafe { std::os::unix::net::UnixListener::from_raw_fd(raw) }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
