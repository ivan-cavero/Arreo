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
//! ## The transfer grammar
//!
//! One connection, six steps, each bound by the marker bytes the descriptor
//! helpers already define ([`arreo_core::pty::adopt::FD_PASS_BYTE`] per
//! descriptor, [`arreo_core::pty::adopt::HANDOFF_COMMIT_BYTE`] for the commit):
//!
//! 1. **Incoming → outgoing: the nonce** — the 32 bytes the outgoing daemon
//!    returned in `HandoffReady`, as the first bytes on the connection. A
//!    mismatch or an early close refuses *before* any descriptor moves.
//! 2. **Outgoing → incoming: the listener, then the lock** (the order is the
//!    contract).
//! 3. **Outgoing → incoming: the pane count**, as four little-endian bytes —
//!    the number of descriptors step 4 will carry. It is on this connection
//!    rather than in the framed manifest for one reason: **the receiver must
//!    never read descriptors until end-of-stream**, because an attacker who
//!    controls the outgoing side could then hold the connection open forever.
//!    A count is a length, and a length is what makes the read bounded.
//! 4. **Outgoing → incoming: one master descriptor per pane**, in the manifest's
//!    order, each accompanied by [`FD_PASS_BYTE`].
//! 5. **Incoming → outgoing: the commit marker byte** — sent by the incoming
//!    daemon once its accept loop owns the inherited listener *and* the pane
//!    pumps have been started. What the outgoing daemon learns from it is that
//!    *the process it authorised is committing* — not that a server is
//!    reachable behind it (see [`wait_for_commit`]).
//! 6. Both sides close.
//!
//! **The panes' contents travel on the framed socket, not here.** The manifest
//! ([`arreo_core::proto::Message::HandoffPanes`]) is sent on the client socket
//! right after `HandoffReady`, because it is protocol data — it can be large,
//! and it is framed and bounded by the codec rather than by a byte grammar
//! invented here. What this connection carries is only what a descriptor
//! *requires*: marker bytes, descriptors, and the one length that bounds them.
//! The two are joined by position (manifest entry *n* ↔ descriptor *n*), which
//! is why the count is validated against the manifest before the first pane
//! descriptor is read.
//!
//! **The descriptor count is validated, never inferred.** A count that disagrees
//! with the manifest is a refusal, not a loop that reads until EOF: a peer that
//! sends three descriptors and claims eight would otherwise hold the handoff
//! open until the timeout, and one that sends nine would have the extra
//! descriptor accepted silently by a receiver that only ever reads "the number
//! it was told".
//!
//! **EOF is not a commit.** The first version of this file read end-of-stream
//! on the transfer connection as the incoming daemon's success, and a process
//! that connected, half-closed, and never served anything committed the cut:
//! the outgoing daemon exited 0, nothing held a listener, and the audit log
//! recorded a handoff that never happened. EOF says a peer stopped *writing* —
//! which is also what a peer that died mid-transfer says — so the outgoing
//! daemon requires the marker byte, and EOF before it is an abort.
//!
//! **And the marker byte is authorisation, not evidence of serving.** It is a
//! real improvement over EOF — it can only come from the process the outgoing
//! daemon answered, because only that process was given the nonce — but it does
//! not, and cannot, show that the other end accepted anything: a peer may
//! present the nonce, take the descriptors, send the byte and do nothing else.
//! What closes that gap is not a stronger signal but a smaller window, and the
//! obvious candidate is worse than the gap: a probe (connect to `<socket>`, wait
//! for a `Welcome`) makes the cut **two** decisions — a probe that times out on
//! a slow-but-healthy daemon leaves the outgoing daemon serving *and* the
//! incoming daemon committed, which is two daemons on one socket, the F5
//! failure, reintroduced by the check meant to prevent a different one. The byte
//! is one atomic decision, so it is what ships. [`wait_for_commit`] carries the
//! reasoning where it is implemented, and
//! `a_commit_by_the_requester_is_authorisation_not_proof_of_serving` keeps it
//! from being "fixed" silently.
//!
//! ## Ordering (outgoing)
//!
//! 1. Receive the request on the client socket; validate the version against
//!    the *incoming* daemon's window; audit a refusal if refused.
//! 2. Take the one-handoff lock ([`handoff_lock_path_for`]) for the duration:
//!    a second request while it is held is refused, never raced.
//! 3. Bind `<socket>.handoff` (mode 0600), reply
//!    `HandoffReady { v, protocol, panes, nonce }`.
//! 4. Accept one connection there with a deadline; check the peer's uid; read
//!    the nonce.
//! 5. Send the listener descriptor, then the lock descriptor.
//! 6. **Pause every pane's pump and wait, bounded, for each acknowledgement**
//!    ([`arreo_core::pty::Pane::pause`]). Only then is no byte in flight, and
//!    only then is a snapshot complete. A pane that does not acknowledge in
//!    time is a **failed handoff**: resume every pane and keep serving (§3 of
//!    the ADR — the machine keeps a working daemon rather than a fast cut).
//! 7. Snapshot every pane ([`arreo_core::pty::Pane::scrollback`]) and send the
//!    `HandoffPanes` manifest on the **client socket**, then the pane count and
//!    the master descriptors on the transfer connection, in manifest order.
//! 8. Wait for the commit marker byte.
//! 9. Write the audit row (`handoff <from> -> <to> panes=<n>`).
//! 10. Stop accepting — the accept loop ends, which drops *this* process's
//!     listener fd (the incoming daemon holds a dup, and the **socket file must
//!     NOT be unlinked**).
//! 11. `std::process::exit(0)`. **No destructor may run.**
//!
//! **Every exit before step 8 that is not a commit resumes every paused pump
//! before it returns to serving.** A paused pane whose child keeps writing fills
//! the terminal's buffer and then blocks the child: aborting a handoff without
//! resuming would strand an agent — a worse outcome than the update not
//! happening, and the reason the resume is not optional.
//!
//! ## Ordering (incoming)
//!
//! 1. Connect to the client socket, Hello/Welcome, send the request, read
//!    `HandoffReady` (with the nonce) and then the `HandoffPanes` manifest.
//! 2. Connect to `<socket>.handoff`, present the nonce, receive the listener,
//!    then the lock, then the pane count and the pane descriptors.
//! 3. **Validate what arrived**: the listener must be a listening stream socket
//!    bound to this socket's path, the lock must be *the* lock at
//!    `<socket>.lock` (see [`validate_listener`] and
//!    [`arreo_core::lock::ExclusiveLock::inherited_checked`]), and the pane count
//!    must equal the manifest's length. Anything else is refused, naming what
//!    arrived.
//! 4. Adopt every pane **with its pump paused** and its ring seeded from the
//!    manifest ([`arreo_core::pty::Pane::adopt_seeded`]). Nothing reads from an
//!    inherited terminal yet: bytes the outgoing daemon never read are still in
//!    the terminal's buffer, and consuming them before the cut commits would
//!    leave the sender — which resumes and keeps serving if this side aborts —
//!    with a hole it cannot recover.
//! 5. Build the daemon around the inherited listener and lock: no bind, no
//!    acquire.
//! 6. Start accepting on the inherited listener, install the adopted panes, and
//!    **then start the pumps** ([`arreo_core::pty::Pane::resume`]).
//! 7. Send the commit marker byte, then keep serving.
//!
//! ## Abort safety on the incoming side
//!
//! Every failure before step 7 drops the adopted panes: closing a `dup` of a
//! master does not disturb the outgoing daemon's own descriptor, and because no
//! pump ever read, nothing was consumed — so the sender resumes with its ring
//! exactly as it was and its terminal buffer untouched. That is why the pumps
//! start only at step 6, and why an abort is a retry rather than a loss.
//!
//! ## Why the transfer is authenticated (F1 of the stage-1 security review)
//!
//! Three layers, because they defend different things:
//!
//! - **Mode 0600 on `<socket>.handoff`** ([`restrict_transfer_socket`]). The
//!   ambient umask gives 0775, and `connect()` needs only write permission on
//!   the inode, so the default mode admits any same-group user. The socket's
//!   *location* is not a control either: with `XDG_RUNTIME_DIR` unset the
//!   daemon falls back to `/tmp`, whose mode is 1777 — every user on the box.
//! - **`SO_PEERCRED` on the accepted connection** ([`check_peer_uid`]): the
//!   peer uid must be ours.
//! - **A per-handoff nonce** ([`send_nonce`]/[`recv_nonce`]): it binds *this*
//!   transfer to the process that requested the handoff on the main socket,
//!   not to any process that noticed the path. It closes the window between
//!   `bind` and `chmod` too, where a same-group peer could still connect.
//!
//! **What the nonce does not change**: a same-user process can request its own
//! handoff on the main socket, exactly as it can send any other verb there.
//! The local socket is ungated by design (`auth: None` in `handle`), and a
//! same-user process can already `kill` the daemon. **And the gate for
//! `Handoff` is the main socket's mode, not the uid**: `Handoff` needs only
//! `connect()`, which on a Unix socket requires write permission on the inode,
//! and the main socket is bound with the ambient umask and never chmod'd — so at
//! the default mode (0775 measured here; 0755 under `umask 022`) a peer in the
//! same **group** can request a handoff, read the nonce and drive the cut, not
//! merely a same-user one. Narrowing the main socket's mode is T-0078's
//! decision; this feature neither narrows nor widens it. What the nonce adds is
//! that the *descriptor transfer* can only be joined by the process the daemon
//! answered.
//!
//! ## One handoff at a time (F5)
//!
//! Two concurrent handoffs both used to commit and leave **two daemons serving
//! one socket**, which is the state T-0071's lock exists to make impossible.
//! The duration of a handoff is now guarded by an exclusive lock at
//! [`handoff_lock_path_for`]; a second request while it is held is answered
//! with a typed refusal and the first proceeds undisturbed.
//!
//! ## Every wait and every write is bounded (F1, stage-2 review)
//!
//! The transfer's reads carry timeouts ([`bound_read`]) and each descriptor
//! receive is bounded in total by [`recv_fd`]. The outgoing *writes* are
//! bounded the same way, by one total deadline shared across them
//! ([`bound_write`], armed by the caller before every send): a peer that takes
//! the descriptors and then stops reading must not be able to freeze this
//! daemon's pumps in a full send buffer — the reproduction that made this
//! section necessary. Giving up on a write is an abort like any other: pumps
//! resume, `.handoff` is unlinked, an `handoff.abort` row is written, and a
//! retry is the remedy. The manifest read on the incoming side is total as
//! well ([`recv_manifest`]), so a trickling peer cannot hold that daemon in the
//! 64 MiB read past the handoff timeout.
//!
//! ## What is NOT transferred
//!
//! The store is opened per operation, never held, so there is no checkpoint to
//! do and no handle to transfer — a second process opening the DB is an
//! existing, supported situation. The incoming daemon loads the authority and
//! trust ledger from disk exactly as a fresh start does, and starts **its own**
//! relay session (T-0060's displacement rule makes that safe). No pane process
//! is spawned, waited for, or signalled: stage 2 moves descriptors, scrollback
//! and the guard's *path*, and the child behind the pane is never touched.
//!
//! Three things are deliberately left behind as **derived**, and each is worth
//! naming because "just send it too" is the tempting move:
//!
//! - **The state engine's state and its `fed` counter.** The engine's input is
//!   the raw journal ([`crate::daemon::PaneEntry::pump`]), so the incoming
//!   daemon re-derives the state by feeding the transferred journal through a
//!   fresh engine. That is why the journal travels at all: without it a pane
//!   that was `Question` arrives `Unknown` and the sidebar loses it until more
//!   output happens to arrive.
//! - **The metrics sampler.** It samples `/proc` for a pid; there is nothing in
//!   it that is not recomputed on the next tick, and its first sample on the far
//!   side is as good as its last on this one.
//! - **Nothing else.** `dropped`/`dropped_bytes` **do** travel: they count what
//!   the bounded ring already evicted and truncated, which is not derivable from
//!   the surviving lines, and a fresh zero would have the new daemon report "0
//!   dropped" about a pane whose history was evicted — a lie in the direction of
//!   looking healthy.
//!
//! ## What stage 2 costs on the wire
//!
//! The raw journal is capped at `arreo_core::pty::MAX_RAW_JOURNAL` (1 MiB) per
//! pane **by the ring's own design**, so a full machine is ≤ 8 MiB of journal
//! for eight panes and normally far less (the journal is a journal, not a
//! buffer: it only reaches 1 MiB if the pane produced 1 MiB). No second cap is
//! invented here; the frame body limit (`codec::MAX_FRAME_BYTES`, also 1 MiB) is
//! what the *manifest* is checked against before it is sent, and a manifest that
//! would exceed it is a refusal — a bounded transfer or none, never a truncated
//! one. Stage 3 (live connections) does not change this: the journal's size is
//! the pane's history, and the history is what has to arrive.

use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::{BorrowedFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long the outgoing daemon waits for the incoming daemon to connect to
/// `<socket>.handoff`, for the nonce, for the commit after the descriptors, and
/// for each descriptor on the incoming side. Generous because the alternative
/// to waiting is a failed update, and a slow machine under load must not turn a
/// working handoff into a refused one.
pub const DEFAULT_HANDOFF_TIMEOUT: Duration = Duration::from_secs(10);

/// The nonce's length. Fixed by the grammar: the incoming daemon's first 32
/// bytes on the transfer connection are the nonce.
pub const NONCE_BYTES: usize = 32;

/// The mode `<socket>.handoff` is bound with. `connect()` needs only write
/// permission on the inode, so anything looser admits every user in the group.
pub const TRANSFER_MODE: u32 = 0o600;

/// How much of a peer-supplied build string may reach an audit row. A frame can
/// carry megabytes; the audit row cannot.
pub const MAX_BUILD_CHARS: usize = 64;

/// `<socket>.handoff` — the dedicated descriptor-transfer socket, bound only
/// for the duration of a handoff and unlinked when it ends.
#[must_use]
pub fn handoff_path_for(socket: &Path) -> PathBuf {
    arreo_core::identity::authority::sidecar(socket, ".handoff")
}

/// `<socket>.handoff.lock` — the one-handoff-at-a-time lock.
///
/// A path of its own rather than the transfer socket itself: `ExclusiveLock`
/// opens a regular file, and `<socket>.handoff` is a socket (opening it
/// read/write fails outright). The lock file stays on disk after release, like
/// `<socket>.lock` — the *open description* carries the lock, so a leftover
/// zero-byte file is not a stale lock.
#[must_use]
pub fn handoff_lock_path_for(socket: &Path) -> PathBuf {
    arreo_core::identity::authority::sidecar(socket, ".handoff.lock")
}

/// Check the incoming daemon's protocol against **its** window.
///
/// The incoming daemon is the server the socket is being handed to, so what
/// matters is that *we* are a version it can speak: our `VERSION` must be
/// inside `{incoming - 1, incoming}`. That is `negotiate(incoming, [VERSION])`
/// — accepted when `incoming == VERSION` or `incoming == VERSION + 1`, refused
/// for a gap of two or more **and for a downgrade**.
///
/// `negotiate(VERSION, [incoming])` — what this used to call — is the same
/// question asked backwards: its window is `{VERSION - 1, VERSION}`, so it
/// refuses every forward bump and accepts a downgrade. A real release could
/// never hand over.
pub fn check_incoming_protocol(incoming_protocol: u32) -> Result<u32, String> {
    incoming_window(arreo_core::proto::VERSION, incoming_protocol)
}

/// The window rule with both versions named, so the direction is testable at a
/// build whose `VERSION` has no representable downgrade (it is 0 today).
fn incoming_window(ours: u32, incoming: u32) -> Result<u32, String> {
    arreo_core::proto::codec::negotiate(incoming, &[ours])
        .map_err(|e| format!("protocol {incoming} cannot take over from {ours}: {e}"))
}

/// A peer-supplied build string, made safe for an audit row: control characters
/// dropped (they can rewrite a terminal that renders the log) and the length
/// bounded to [`MAX_BUILD_CHARS`] bytes on a char boundary.
#[must_use]
pub fn sanitize_build(build: &str) -> String {
    let mut out = String::new();
    for ch in build.chars() {
        if ch.is_control() {
            continue;
        }
        if out.len() + ch.len_utf8() > MAX_BUILD_CHARS {
            break;
        }
        out.push(ch);
    }
    out
}

/// Write the `handoff.refuse` audit row. Best-effort like every background
/// daemon write: a store failure must not take down a daemon that is — by
/// definition of this path — still serving.
fn record_refusal(db: &Path, detail: &str) {
    record_row(db, arreo_core::store::actions::HANDOFF_REFUSE, detail);
}

/// Write the `handoff.abort` audit row: a cut that started and did not happen.
fn record_abort_row(db: &Path, detail: &str) {
    record_row(db, arreo_core::store::actions::HANDOFF_ABORT, detail);
}

/// The incoming daemon records its own refusal, with the reason the outgoing
/// side cannot know (a missing socket path, a lock that reads as free). A
/// failed handoff must not be silent on either side.
pub fn record_incoming_abort(db: &Path, detail: &str) {
    record_abort_row(db, &format!("incoming: {detail}"));
}

fn record_row(db: &Path, action: &str, detail: &str) {
    if let Ok(store) = arreo_core::store::SessionStore::open(db) {
        let _ = store.record(&arreo_core::store::AuditEvent {
            device: "daemon".to_string(),
            agent: String::new(),
            prompt: String::new(),
            detail: Some(detail.to_string()),
            ..arreo_core::store::AuditEvent::new(
                action,
                arreo_core::store::AuditKind::Unknown,
                arreo_core::store::AuditOutcome::Refused,
                now_ms(),
            )
        });
    }
}

/// The opening words of the outgoing daemon's **busy** refusal — the one
/// refusal that is not a failure of the machine but a *deferred* update.
///
/// The two halves of this string are in two processes that may even be two
/// builds apart (the incoming daemon is the new binary), so it travels as prose
/// in the typed `Error`'s `message`: `Message` has no refusal-reason field, and
/// adding a protocol variant to carry one local exit code would be a wire change
/// serving a decision that is not on the wire's behalf. **The coupling is
/// one-way and it is a string match**: the outgoing daemon builds the detail
/// *from* this constant, and the incoming daemon asks
/// [`is_busy_refusal`] before choosing its exit code. If the wording below
/// changes, both sides change with it — and because it is the *only* thing that
/// distinguishes a locked handoff from a broken one, changing it silently turns
/// the deferred update back into a reported failure.
pub const BUSY_REFUSAL: &str = "another handoff holds";

/// Whether a refusal detail names the one-handoff lock as the reason.
///
/// The consumer side of [`BUSY_REFUSAL`]: the incoming daemon exits 3 (the
/// project's "in progress" code, the same one the updater's own lock uses) for
/// this refusal and 1 for every other.
#[must_use]
pub fn is_busy_refusal(detail: &str) -> bool {
    detail.contains(BUSY_REFUSAL)
}

/// The failure rows one session may write: **one refusal and one abort**, never
/// one per frame.
///
/// A client can send `Handoff` in a loop; a row per attempt would let it fill
/// the operator's log from the machine itself, which is the cheapest denial of
/// service against an audit trail. The first refusal and the first abort are
/// the interesting ones — who tried, when, and why they were turned away. The
/// ten thousandth says nothing new (T-0059 solved the same problem for trust
/// refusals the same way).
#[derive(Debug, Default)]
pub struct HandoffFailureLog {
    refused: bool,
    aborted: bool,
}

impl HandoffFailureLog {
    /// Record a refusal (a handoff that ended before any descriptor moved).
    pub fn refuse(&mut self, db: &Path, detail: &str) {
        if self.refused {
            return;
        }
        self.refused = true;
        record_refusal(db, detail);
    }

    /// Record an abort (a handoff that started and did not commit).
    pub fn abort(&mut self, db: &Path, detail: &str) {
        if self.aborted {
            return;
        }
        self.aborted = true;
        record_abort_row(db, &format!("outgoing: {detail}"));
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
pub fn send_one(socket: &UnixStream, fd: BorrowedFd<'_>) -> std::io::Result<()> {
    arreo_core::pty::adopt::send_fd(socket, fd).map_err(|e| {
        std::io::Error::new(
            std::io::ErrorKind::ConnectionAborted,
            format!("handoff send descriptor: {e}"),
        )
    })
}

/// Receive one descriptor, waiting at most `timeout` in total.
pub fn recv_one(
    socket: &UnixStream,
    timeout: Duration,
) -> Result<OwnedFd, arreo_core::pty::adopt::FdTransferError> {
    arreo_core::pty::adopt::recv_fd(socket, timeout)
}

/// How many bytes the pane count takes on the transfer connection: four,
/// little-endian, matching the framing convention the rest of the workspace
/// uses (`codec::encode_frame`'s `u32 LE` length prefix).
pub const PANE_COUNT_BYTES: usize = 4;

/// Send the number of pane descriptors that follow on the transfer connection.
///
/// **Why the count is on the wire at all**: the receiver must never read
/// descriptors until end-of-stream. An attacker controlling the outgoing side
/// would then hold the handoff open for the whole timeout, and a peer that sent
/// *more* descriptors than the manifest named would have the surplus accepted
/// by a receiver that only ever read "the number it was told". A count is a
/// length, and a length is what makes the read bounded and checkable.
pub fn send_pane_count(socket: &UnixStream, count: usize) -> std::io::Result<()> {
    let mut handle = socket;
    handle.write_all(&(count as u32).to_le_bytes())
}

/// Read the pane count, waiting at most `timeout`.
///
/// The value is *not* trusted as a loop bound on its own: the caller compares it
/// with the manifest it received and refuses on a mismatch, which is what keeps
/// a hostile count from making the incoming daemon allocate or wait for a
/// number the sender invented.
pub fn recv_pane_count(socket: &UnixStream, timeout: Duration) -> Result<usize, String> {
    bound_read(socket, timeout)?;
    let mut bytes = [0u8; PANE_COUNT_BYTES];
    let mut handle = socket;
    handle
        .read_exact(&mut bytes)
        .map_err(|e| format!("the pane count did not arrive: {e}"))?;
    Ok(u32::from_le_bytes(bytes) as usize)
}

/// Send the pane manifest: a `u32 LE` length, then that many bytes of
/// MessagePack (`arreo_core::proto::message::encode_manifest`).
///
/// The length prefix is the same shape the framed protocol uses, and it is what
/// lets the receiver bound the read before it allocates: a manifest's size is
/// peer-supplied, so it is checked against [`arreo_core::proto::message::MAX_MANIFEST_BYTES`]
/// on the receiving side rather than believed.
///
/// **The write is bounded by `deadline` in total** (F1 of the stage-2 review):
/// a single `SO_SNDTIMEO` is not enough, because Linux applies it per blocked
/// syscall and a stream write that fills the buffer partway returns the partial
/// count with a *fresh* timeout for the next syscall — one `write_all` of a
/// megabyte can consume twice its armed timeout, which is exactly how a peer
/// that stops reading used to hold the panes paused for 2× the handoff bound.
/// [`write_all_bounded`] re-arms the timeout with what is left of the shared
/// deadline before every syscall, so the total of them is the deadline.
pub fn send_manifest(
    socket: &UnixStream,
    bytes: &[u8],
    deadline: std::time::Instant,
) -> std::io::Result<()> {
    write_all_bounded(
        socket,
        deadline,
        &(bytes.len() as u32).to_le_bytes(),
        "the pane manifest length",
    )?;
    write_all_bounded(socket, deadline, bytes, "the pane manifest")
}

/// `write_all` with the socket's write timeout re-armed to what is left of
/// `deadline` before **every** write — the total-write-deadline half of
/// [`send_manifest`] (F1).
///
/// `write_all` alone is not enough: Linux applies `SO_SNDTIMEO` per blocking
/// syscall, and when a stream buffer fills mid-call the syscall times out and
/// returns the partial count, so the next syscall gets a fresh timeout — a
/// single `write_all` of a manifest can consume **twice** the armed timeout
/// (measured: a 1 MiB `write_all` armed with 2 s blocked for 4.02 s). A peer
/// that stops reading would then hold the panes paused for 2× the handoff
/// bound. Re-arming before every syscall makes the total of them exactly the
/// deadline: the manifest send gives up within the handoff timeout, and giving
/// up is an abort.
pub(crate) fn write_all_bounded(
    mut socket: &UnixStream,
    deadline: std::time::Instant,
    mut buf: &[u8],
    what: &str,
) -> std::io::Result<()> {
    while !buf.is_empty() {
        bound_write(socket, deadline)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::TimedOut, e))?;
        match socket.write(buf) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    format!("{what} made no progress: the peer stopped reading"),
                ))
            }
            Ok(n) => buf = &buf[n..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // A blocking socket whose write budget this function just armed can
            // only report WouldBlock when the remaining time ran out: that IS
            // the total deadline firing, and the error says so.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("{what} did not reach the peer: the handoff write deadline elapsed"),
                ))
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Receive the pane manifest, waiting at most `timeout` **in total**.
///
/// A length over [`arreo_core::proto::message::MAX_MANIFEST_BYTES`] is refused **before**
/// the body is read, so a peer cannot make this allocate a gigabyte by naming
/// one. The body is then read exactly, and a peer that closes early is an error
/// rather than a short manifest: a truncated transfer must never be applied as a
/// smaller one.
///
/// **The deadline is total, not per-read** (F3 of the stage-2 review): the
/// reads re-arm the socket's timeout with what is left of the one `deadline`,
/// on the `recv_fd` model, so a peer that trickles a byte at a time cannot hold
/// this daemon in the 64 MiB read for ever. The body is read through a manual
/// loop rather than `read_exact` for exactly that reason: `read_exact` would
/// re-arm the timeout only once, and a trickler that lands inside the per-read
/// bound on every read would stretch the whole read past the deadline
/// indefinitely.
pub fn recv_manifest(socket: &UnixStream, timeout: Duration) -> Result<Vec<u8>, String> {
    let deadline = std::time::Instant::now() + timeout;
    let handle = socket;
    let mut length = [0u8; PANE_COUNT_BYTES];
    read_exact_bounded(handle, deadline, &mut length, "the manifest length")?;
    let length = u32::from_le_bytes(length) as usize;
    if length > arreo_core::proto::message::MAX_MANIFEST_BYTES {
        return Err(format!(
            "the manifest claims {length} bytes (limit {})",
            arreo_core::proto::message::MAX_MANIFEST_BYTES
        ));
    }
    let mut body = vec![0u8; length];
    read_exact_bounded(handle, deadline, &mut body, "the manifest body")?;
    Ok(body)
}

/// `read_exact` with the socket's read timeout re-armed to what is left of
/// `deadline` before **every** read — the total-deadline half of
/// [`recv_manifest`] (F3). `read_exact` alone would run the same loop with a
/// fixed per-read timeout, which a peer that trickles bytes can stretch past
/// the deadline one read at a time.
fn read_exact_bounded(
    mut socket: &UnixStream,
    deadline: std::time::Instant,
    mut buf: &mut [u8],
    what: &str,
) -> Result<(), String> {
    while !buf.is_empty() {
        let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
            return Err(format!(
                "{what} did not arrive: the handoff read deadline elapsed"
            ));
        };
        socket
            .set_read_timeout(Some(remaining))
            .map_err(|e| format!("cannot bound the handoff read by {remaining:?}: {e}"))?;
        match socket.read(buf) {
            Ok(0) => {
                return Err(format!(
                    "{what} ended early: the peer closed the connection"
                ))
            }
            Ok(n) => buf = &mut buf[n..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // A blocking socket whose read budget this function just armed can
            // only report WouldBlock when the remaining time ran out: that IS
            // the total deadline firing, and the refusal must say so (F3).
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Err(format!(
                    "{what} did not arrive: the handoff read deadline elapsed"
                ))
            }
            Err(e) => return Err(format!("cannot read {what}: {e}")),
        }
    }
    Ok(())
}

/// Every pane whose pump this handoff has stopped, and the promise to start it
/// again.
///
/// **Why this is a type and not a `resume_all()` call at the end.** The outgoing
/// daemon has a dozen abort paths between "the panes are paused" and "the cut
/// committed", and one of them forgetting to resume would leave the agent behind
/// that pane **blocked for ever**: the child keeps writing, the terminal's buffer
/// fills, and nothing reads it. That is a stuck agent — strictly worse than an
/// update that did not happen — and a `Drop` cannot be forgotten the way a
/// `return` can.
///
/// **The number that makes this urgent**: a pty with no reader absorbs only
/// about **8–12 KiB** before the writer blocks (measured on this kernel; a pty
/// buffer is *not* a pipe's 64 KiB — evidence in
/// `.loop/evidence/T-0038/stage2-pty-buffer.txt`). That is a couple of screens of
/// build output, not an unlikely burst, so the window between pausing and
/// resuming is the whole of the hazard.
pub struct PausedPanes {
    panes: Vec<std::sync::Arc<arreo_core::pty::Pane>>,
    /// Set once the cut is committed and the pumps must stay parked (the
    /// incoming daemon owns them now; this process is about to `exit(0)`).
    committed: bool,
}

impl PausedPanes {
    /// Pause every pane's pump and wait, bounded, for each acknowledgement.
    ///
    /// `Ok` means no byte is in flight on any of them, so a snapshot taken now
    /// is complete: everything the children have written is in their rings or
    /// still in their terminals' buffers. A pane that does not acknowledge
    /// within `timeout` fails the whole call — and **every pane paused before
    /// the failure is already resumed** by the `Drop` of the partially built
    /// value, so the caller has nothing to unwind.
    pub fn pause_all(
        panes: Vec<std::sync::Arc<arreo_core::pty::Pane>>,
        timeout: Duration,
    ) -> Result<Self, String> {
        let mut paused = Self {
            panes: Vec::with_capacity(panes.len()),
            committed: false,
        };
        for pane in panes {
            paused.panes.push(std::sync::Arc::clone(&pane));
            if let Err(e) = pane.pause(timeout) {
                // `paused` drops here: every pane already stopped is resumed
                // before this returns an error.
                return Err(format!("a pane's reader pump did not pause: {e}"));
            }
        }
        Ok(paused)
    }

    /// The pumps stay parked for ever: the incoming daemon owns them now, and
    /// this process is about to exit without running destructors.
    pub fn commit(mut self) {
        self.committed = true;
    }

    /// Resume every pump now (the ordinary way out of a failed handoff).
    pub fn resume_all(mut self) {
        self.resume_now();
    }

    fn resume_now(&mut self) {
        for pane in &self.panes {
            pane.resume();
        }
        self.panes.clear();
    }
}

impl Drop for PausedPanes {
    fn drop(&mut self) {
        if !self.committed {
            self.resume_now();
        }
    }
}

/// Bind `<socket>.handoff` with mode [`TRANSFER_MODE`], immediately after the
/// bind.
///
/// `bind` creates the socket with the process umask, which is 0775 or 0770 in
/// the common configurations — and `connect()` on a Unix socket needs only
/// write permission on the inode, so a same-group user could connect, present
/// nothing, and (before this) abort or join the transfer. The tiny
/// `bind`-then-`chmod` window is closed by the peer-uid and nonce checks on the
/// accepted connection, so this is belt to their braces rather than the only
/// strap.
///
/// Note that the socket's *location* is not a control: with `XDG_RUNTIME_DIR`
/// unset, `arreo-server` falls back to `/tmp`, whose mode is 1777.
pub fn restrict_transfer_socket(path: &Path) -> std::io::Result<()> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(TRANSFER_MODE))
}

/// Send the per-handoff nonce as the first bytes of the transfer connection.
pub fn send_nonce(socket: &UnixStream, nonce: &[u8]) -> std::io::Result<()> {
    let mut handle = socket;
    handle.write_all(nonce)
}

/// Read the nonce the incoming daemon presented, waiting at most `timeout`.
///
/// A peer that sends fewer than [`NONCE_BYTES`] and closes reads as an error
/// here, which is the point: nobody gets past this step without the bytes the
/// outgoing daemon minted for *this* handoff.
pub fn recv_nonce(socket: &UnixStream, timeout: Duration) -> Result<[u8; NONCE_BYTES], String> {
    bound_read(socket, timeout)?;
    let mut nonce = [0u8; NONCE_BYTES];
    let mut handle = socket;
    handle
        .read_exact(&mut nonce)
        .map_err(|e| format!("the incoming daemon did not present the handoff nonce: {e}"))?;
    Ok(nonce)
}

/// The incoming daemon's commit: exactly one marker byte, sent once this
/// process's accept loop owns the inherited listener and is about to accept.
///
/// That the marker is only sent from a genuinely-accepting process is *this*
/// side's honest statement; the outgoing daemon reads it as authorisation
/// rather than as proof that a server is reachable — see [`wait_for_commit`].
pub fn send_commit(socket: &UnixStream) -> std::io::Result<()> {
    let mut handle = socket;
    handle.write_all(&[arreo_core::pty::adopt::HANDOFF_COMMIT_BYTE])
}

/// Wait for the incoming daemon's commit marker byte.
///
/// **EOF is not a commit**: it means the peer stopped writing, which is what a
/// peer that died mid-transfer does too. Only
/// [`arreo_core::pty::adopt::HANDOFF_COMMIT_BYTE`] commits this cut. Everything
/// else (EOF, a wrong byte, the deadline) is an abort and the outgoing daemon
/// keeps serving.
///
/// **The byte is authorisation, not proof that a peer is serving.** It can only
/// be sent by the process that asked for the handoff on the main socket, because
/// only that process was given the nonce — so it says "the requester has taken
/// the descriptors and is committing". Whether a daemon is *behind* it is not
/// something this side can see: a peer may send the byte and then do nothing.
/// That is a real gap and it is accepted, because the candidate fixes are worse:
/// the obvious one, connecting to `<socket>` and waiting for a `Welcome` after
/// the byte, is **two** decisions where the protocol has one — a probe that
/// times out on a slow-but-healthy daemon leaves the outgoing daemon serving
/// *and* the incoming daemon committed, i.e. two daemons on one socket (the F5
/// failure), reintroduced by the check meant to prevent a different one. It is
/// not implemented, and
/// `a_commit_by_the_requester_is_authorisation_not_proof_of_serving` (see the
/// handoff tests) turns red if someone adds one.
pub fn wait_for_commit(socket: &UnixStream, timeout: Duration) -> Result<(), String> {
    bound_read(socket, timeout)?;
    let mut byte = [0u8; 1];
    let mut handle = socket;
    loop {
        match handle.read(&mut byte) {
            Ok(0) => {
                return Err(
                    "the incoming daemon closed the transfer connection without committing \
                     (end-of-stream is not a commit)"
                        .to_string(),
                )
            }
            Ok(_) if byte[0] == arreo_core::pty::adopt::HANDOFF_COMMIT_BYTE => return Ok(()),
            Ok(_) => {
                return Err(format!(
                    "the incoming daemon sent {:#04x} instead of the commit marker {:#04x}",
                    byte[0],
                    arreo_core::pty::adopt::HANDOFF_COMMIT_BYTE
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(format!("no commit marker within {timeout:?}: {e}")),
        }
    }
}

/// The peer on the transfer connection must be this user.
///
/// What this bounds, exactly: the **transfer**. A peer that got this far already
/// held the nonce, so it is the process this daemon answered *or* one that read
/// the nonce from it; the uid makes the descriptor handover this-user-only
/// regardless. It is **not** what decides who may *ask* for a handoff — `Handoff`
/// is ungated and needs only `connect()`, which on a Unix socket requires write
/// permission on the main socket's inode. That socket is bound with the ambient
/// umask and never chmod'd (0775 measured here; 0755 under `umask 022`), so at
/// the default mode a same-**group** peer can request a handoff, read the nonce
/// and drive the cut. The main socket's mode is therefore the gate for the
/// request, and narrowing it is T-0078's decision — not this check's.
///
/// `Ok(())` when the platform cannot answer (`peer_uid` returns `None`): the
/// handoff stays possible there, with the nonce and the socket's own
/// permissions carrying the check. Nothing here ever treats an unanswerable
/// question as a "yes" for an *answered* one — a returned uid that is not ours
/// is always a refusal.
pub fn check_peer_uid(socket: &UnixStream) -> Result<(), String> {
    match arreo_core::pty::adopt::peer_uid(socket) {
        Ok(Some(uid)) if uid == arreo_core::pty::adopt::own_uid() => Ok(()),
        Ok(Some(uid)) => Err(format!(
            "the transfer connection is from uid {uid}, not this daemon's uid {}",
            arreo_core::pty::adopt::own_uid()
        )),
        Ok(None) => Ok(()),
        Err(e) => Err(format!("cannot read the transfer peer's credentials: {e}")),
    }
}

/// Validate a descriptor offered as the listener to inherit.
///
/// Three facts, all about the descriptor rather than about the message that
/// described it: it is a **listening stream socket**, and `getsockname()`
/// equals the path this process was told to take over. A descriptor that is a
/// connected `socketpair` end, or a listener for a *different* socket, is
/// refused naming what arrived — serving on either would accept connections
/// the operator's clients never reach.
pub fn validate_listener(fd: BorrowedFd<'_>, expected: &Path) -> Result<(), String> {
    match arreo_core::pty::adopt::socket_is_listening_stream(fd) {
        Ok(true) => {}
        Ok(false) => {
            return Err(
                "the descriptor in the listener slot is not a listening stream socket".to_string(),
            )
        }
        Err(e) => return Err(format!("cannot read the listener descriptor's type: {e}")),
    }
    match arreo_core::pty::adopt::socket_bound_path(fd) {
        Ok(Some(bound)) if bound == expected => Ok(()),
        Ok(Some(bound)) => Err(format!(
            "the listener descriptor is bound to {} but this process was told to take over {}",
            bound.display(),
            expected.display()
        )),
        Ok(None) => Err(format!(
            "the listener descriptor has no bound path (expected {})",
            expected.display()
        )),
        Err(e) => Err(format!(
            "cannot read the listener descriptor's address: {e}"
        )),
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
/// takes over closing it exactly once.
///
/// The *type* is the caller's warranty, and it is a real one: interpreting an
/// arbitrary descriptor as a `UnixListener` performs no I/O (no syscall reads
/// the handle), so this is memory-safe for any descriptor. It is **not**
/// identity-safe — a memory-safe misinterpretation is still a listener for some
/// other socket — which is why [`validate_listener`] runs on the descriptor
/// before this does, and why "the descriptor cannot misinterpret the handle"
/// was the wrong claim to make.
pub fn std_listener_from_fd(fd: OwnedFd) -> std::os::unix::net::UnixListener {
    let raw = fd.into_raw_fd();
    // SAFETY: `raw` came from an owned descriptor this line just consumed, so
    // it is valid, open, and uniquely owned — the three conditions
    // `from_raw_fd` requires.
    unsafe { std::os::unix::net::UnixListener::from_raw_fd(raw) }
}

/// Bound a read on the transfer connection by `timeout`, so no wait in the
/// handoff is unbounded.
fn bound_read(socket: &UnixStream, timeout: Duration) -> Result<(), String> {
    socket
        .set_read_timeout(Some(timeout))
        .map_err(|e| format!("cannot bound the handoff read by {timeout:?}: {e}"))
}

/// Bound an outgoing write on the transfer connection by the whole of
/// `deadline`, on the `recv_fd` model (F1 of the stage-2 review).
///
/// `SO_SNDTIMEO` bounds each blocking write; re-arming it with what is left of
/// the one shared deadline is what bounds the **total** of the writes. The
/// difference is the review's reproduction: a peer that presents the nonce,
/// takes the listener and lock, and then stops reading used to leave the
/// outgoing daemon blocked in `send_manifest`/`send_pane_count`/`send_one` for
/// ever — with every pump paused and the one-handoff lock held, which is the
/// whole machine stuck on a peer that is not even serving. With a total
/// deadline the transfer gives up within the handoff timeout, and giving up is
/// an abort: `PausedPanes`'s `Drop` resumes every pump, `.handoff` is unlinked
/// by the caller's `TransferSocket`, and an `handoff.abort` row is written.
///
/// `Err` when the deadline has already elapsed (the writes have no budget
/// left) or the timeout cannot be armed.
pub(crate) fn bound_write(socket: &UnixStream, deadline: std::time::Instant) -> Result<(), String> {
    let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
        return Err("the transfer's write deadline elapsed (the peer stopped reading)".to_string());
    };
    socket
        .set_write_timeout(Some(remaining))
        .map_err(|e| format!("cannot bound the handoff write by {remaining:?}: {e}"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arreo_core::proto::VERSION;

    /// The version window is the **incoming** daemon's, and it is asymmetric:
    /// a forward bump of one is accepted, the same version is accepted, a jump
    /// of two is refused, and a downgrade is refused.
    ///
    /// This is the test that tells the two directions apart: the old call
    /// (`negotiate(VERSION, [incoming])`) accepted a downgrade and refused
    /// every bump, so `VERSION + 1` here is exactly the case it got wrong.
    #[test]
    fn the_version_window_is_the_incoming_daemons() {
        assert_eq!(
            check_incoming_protocol(VERSION).expect("same version"),
            VERSION
        );
        assert_eq!(
            check_incoming_protocol(VERSION + 1).expect("one bump forward is in its window"),
            VERSION
        );
        assert!(
            check_incoming_protocol(VERSION + 2).is_err(),
            "a gap of two is a deferred update, not a silent one"
        );
        assert!(
            check_incoming_protocol(VERSION + 3).is_err(),
            "and a wider gap certainly is"
        );
        // The rule's shape at a version where a downgrade exists: the build's
        // `VERSION` is 0 today, so there is no older protocol for it to refuse,
        // and an absent `protocol` field (`#[serde(default)]`, ADR 0017) reads
        // as 0 — the same version, which is accepted. That is not a hole: the
        // requester still has to complete the whole transfer (nonce,
        // descriptors, commit) before anything is taken over.
        assert!(incoming_window(3, 3).is_ok(), "same version");
        assert!(incoming_window(3, 4).is_ok(), "one bump forward");
        assert!(incoming_window(3, 5).is_err(), "a gap of two");
        assert!(
            incoming_window(3, 2).is_err(),
            "a downgrade must never take the socket over"
        );
    }

    /// The peer's build string reaches an audit row, so it must not be able to
    /// carry a terminal escape or megabytes of padding.
    #[test]
    fn a_build_string_is_stripped_and_bounded() {
        let hostile = format!("evil\x1b[2J\x07{}", "x".repeat(900 * 1024));
        let clean = sanitize_build(&hostile);
        assert_eq!(clean, format!("evil[2J{}", "x".repeat(MAX_BUILD_CHARS - 7)));
        assert!(!clean.chars().any(char::is_control));
        assert!(clean.len() <= MAX_BUILD_CHARS);
        // A multi-byte character is never split: the result is valid UTF-8 by
        // construction (`String`), and ends on a boundary.
        assert!(sanitize_build(&"é".repeat(100)).chars().count() <= MAX_BUILD_CHARS / 2);
    }

    /// F3 (stage-2 review): the manifest read's deadline is **total**, not
    /// per-read — a peer that trickles a byte at a time cannot hold this side
    /// in the manifest read past the handoff timeout.
    ///
    /// The writer completes its declared body, one byte per tick, and a
    /// half-close is never involved: read-exact on an early EOF is an error for
    /// a different reason, and could not tell the two apart. Without the total
    /// deadline every read lands inside its per-read timeout (the old
    /// `recv_manifest` re-armed `SO_RCVTIMEO` once) and the read **completes**,
    /// so the assertion "abandoned at the deadline, with the body unfinished"
    /// fails. What removal turns red: re-arming the read timeout once per call
    /// instead of per read — the trickle finishes, the manifest decodes, and
    /// `expect_err` sees `Ok`.
    #[test]
    fn a_trickling_manifest_peer_is_abandoned_at_the_total_deadline() {
        use std::io::Write as _;
        let (read_end, write_end) = UnixStream::pair().expect("socketpair");
        // The body the writer will complete, slowly: 24 bytes at 150 ms each is
        // ~3.6 s, comfortably past the 700 ms total deadline the reader gets.
        const DECLARED: usize = 24;
        let writer = std::thread::spawn(move || {
            let mut write_end = write_end;
            write_end
                .write_all(&(DECLARED as u32).to_le_bytes())
                .expect("the length prefix");
            for _ in 0..DECLARED {
                write_end.write_all(&[0x00]).expect("a trickle byte");
                std::thread::sleep(Duration::from_millis(150));
            }
        });
        let started = std::time::Instant::now();
        let err = recv_manifest(&read_end, Duration::from_millis(700))
            .expect_err("the trickler is abandoned at the total deadline");
        assert!(
            err.contains("deadline"),
            "the refusal names the deadline that ran out: {err}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "given up at the deadline (700 ms), not after the trickle finished (~3.6 s)"
        );
        // The writer always finishes on its own — the test cannot hang even on
        // a total regression, because a completed-but-refused read is still an
        // error to `expect_err` only if it names the deadline.
        writer.join().expect("the writer finishes");
        drop(read_end);
    }

    /// F1 (stage-2 review): the total **write** deadline refuses once it has
    /// elapsed, and arms each write with what is left of it — the model that
    /// makes a peer which stops reading abort the transfer instead of freezing
    /// it. The write loop re-arms before every syscall, because Linux times
    /// out each blocked syscall separately and a plain `write_all` of a stream
    /// that fills mid-call can run **2×** its armed timeout (measured: 1 MiB
    /// armed with 2 s blocked for 4.02 s); `write_all_bounded` is what makes
    /// the total of the syscalls the deadline. Covered end to end by the
    /// handoff integration test of the same name; this pins the primitive.
    #[test]
    fn bound_write_gives_up_at_the_deadline() {
        let (read_end, write_end) = UnixStream::pair().expect("socketpair");
        // A deadline in the past: the writes have no budget left and must be
        // refused without touching the socket.
        let deadline = std::time::Instant::now() - Duration::from_secs(1);
        let err =
            bound_write(&write_end, deadline).expect_err("a spent deadline refuses the write");
        assert!(
            err.contains("deadline"),
            "the refusal names the bound that ran out: {err}"
        );
        // A live deadline arms a real write timeout: a peer that never reads
        // (this test holds the other end and does not drain it) leaves a big
        // write blocked, and the bounded loop gives up at the **total**
        // deadline — not at 2× it, which is what re-arming once per call would
        // allow (`write_all` on a stream that fills mid-call gets a fresh
        // timeout for the next syscall).
        let deadline = std::time::Instant::now() + Duration::from_millis(400);
        let payload = vec![0u8; 1 << 20];
        let started = std::time::Instant::now();
        let res = write_all_bounded(&write_end, deadline, &payload, "a test payload");
        assert!(
            res.is_err(),
            "a write into a socket nobody drains must give up at the armed deadline"
        );
        assert!(
            started.elapsed() < Duration::from_millis(600),
            "the bounded write gave up at the total deadline (~400 ms), not after a 2× per-syscall run (took {:?})",
            started.elapsed()
        );
        // The deadline passing is itself a refusal, even on a socket that
        // could take the write.
        let deadline = std::time::Instant::now() + Duration::from_millis(50);
        std::thread::sleep(Duration::from_millis(120));
        let err = bound_write(&write_end, deadline)
            .expect_err("by the time it arms, the deadline has elapsed");
        assert!(err.contains("deadline"), "names the bound: {err}");
        drop(read_end);
    }
}
