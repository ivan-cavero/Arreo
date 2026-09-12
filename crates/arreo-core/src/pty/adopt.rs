//! PTY adoption (T-0038 stage 0): take over a master descriptor that arrived
//! over `SCM_RIGHTS`, plus the descriptor-passing helpers both ends of the
//! handoff use.
//!
//! Nothing here forks, execs or reaps. The child behind an adopted master
//! belongs to the daemon being replaced, and a process that is not our child
//! can never be `waitpid`ed — that single fact shapes [`AdoptedChild`], which
//! detects exit from the process itself (`pidfd_open`) or from end-of-stream on
//! the master, and reports the status as [`UNKNOWN_EXIT`] instead of guessing
//! one.

use std::ffi::OsStr;
use std::io::{self, IoSlice, IoSliceMut};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use portable_pty::{Child, ChildKiller, ExitStatus, MasterPty, PtySize};
use rustix::event::{poll, PollFd, PollFlags};
use rustix::io::{Errno, FdFlags};
use rustix::net::sockopt::get_socket_type;
use rustix::net::{
    recvmsg, sendmsg, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, SocketType,
};
use rustix::process::{kill_process, Pid, Signal};
#[cfg(target_os = "linux")]
use rustix::process::{pidfd_open, PidfdFlags};
use rustix::termios::Winsize;
use thiserror::Error;

use super::{PtyError, UNKNOWN_EXIT};

/// The payload byte that accompanies every descriptor on the wire.
///
/// It is a *synchronisation marker*, not an integrity control. `SCM_RIGHTS`
/// rides on data, and on a stream socket the kernel treats ancillary data as a
/// barrier (unix(7)): a descriptor is delivered together with whatever bytes
/// were already queued ahead of it, and a byte queued before a transfer is
/// handed over on its own, without the descriptor. So this byte cannot bind a
/// transfer to a descriptor *on the channel's say-so*: the byte this side reads
/// is this transfer's marker only when the channel has been drained to the
/// transfer boundary, and draining it is the caller's guarantee to make — a
/// dedicated `socketpair` per transfer (what the handoff uses), or framing on a
/// channel that also carries protocol bytes.
///
/// What this crate does guarantee, on a channel used that way, is
/// refuse-rather-than-guess: a message carrying a descriptor and no byte is
/// refused, a message carrying bytes and no descriptor is refused, and a
/// message whose descriptor arrived with the wrong byte is refused with that
/// descriptor closed. Only a descriptor the peer actually passed over
/// `SCM_RIGHTS` is ever handed out.
pub const FD_PASS_BYTE: u8 = 0x01;

/// Default bound for [`recv_fd`]: the wait for a descriptor is never unbounded,
/// so a peer that dies mid-handshake cannot hang the handoff.
pub const DEFAULT_FD_TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a descriptor transfer failed.
///
/// Every variant means no descriptor was handed out, so a caller never has to
/// close anything: a descriptor that arrived but was refused is closed inside
/// [`recv_fd`] before the error is returned.
#[derive(Debug, Error)]
pub enum FdTransferError {
    #[error("descriptor transfer io: {0}")]
    Io(#[from] io::Error),
    #[error("no message within {0:?}: the peer never sent a descriptor")]
    TimedOut(Duration),
    /// End-of-stream with nothing transferred: on the stream socket the
    /// handoff uses, the only way a peer reports that it is gone.
    #[error("peer closed the descriptor socket without sending a descriptor")]
    PeerClosed,
    #[error("message carried a payload byte but no descriptor")]
    NoDescriptor,
    #[error("descriptor arrived without its payload byte")]
    NoPayload,
    #[error("payload byte {got:#04x}, expected {expected:#04x}")]
    WrongPayload { got: u8, expected: u8 },
    /// The descriptor did not fit the buffer [`send_fd`] sized for it, which
    /// would mean rustix's own accounting disagrees with itself. Sending a
    /// descriptor-less message would be worse than refusing.
    #[error("descriptor did not fit the control message buffer")]
    ControlBufferTooSmall,
    #[error("message sent {sent} of {expected} payload bytes")]
    ShortSend { sent: usize, expected: usize },
    /// More than one descriptor in one message. Only the descriptors that fit
    /// the receive buffer can be counted; a sender that packs even more has the
    /// surplus closed by the kernel as truncation, and its message is refused
    /// here one way or the other.
    #[error("message carried {got} descriptors, expected exactly 1")]
    TooManyDescriptors { got: usize },
}

/// So `?` works on rustix calls, which fail with its own `Errno` rather than
/// `std::io::Error`: one error type for the caller, whatever the layer.
impl From<Errno> for FdTransferError {
    fn from(err: Errno) -> Self {
        Self::Io(err.into())
    }
}

/// Send `fd` to the peer of `socket` as `SCM_RIGHTS`.
///
/// The wire format is one byte ([`FD_PASS_BYTE`]) plus one descriptor, sent as
/// one message. Both ends of the handoff are this crate, so the grammar is
/// ours to fix, but the *byte* binds nothing on a stream socket (see
/// [`FD_PASS_BYTE`]): a transfer is unambiguous only on a channel drained to
/// the transfer boundary. The caller owns that discipline here as well as on
/// the receive side — bytes queued before this call reach the peer ahead of the
/// descriptor.
///
/// The send is `MSG_NOSIGNAL`. A peer that dies mid-transfer must not take the
/// sending daemon down with `SIGPIPE`: that daemon has to go on serving until
/// the handoff commits, which is the state this whole mechanism exists to
/// preserve, and nothing in this crate establishes that the process ignores the
/// signal.
///
/// The caller owns the socket and must have established who is on the other
/// end — a descriptor transfer to an unauthenticated peer hands over a
/// terminal, so this is only ever used on a `socketpair` shared with the peer
/// being replaced, or on a connection whose peer has been identified. Passing
/// the socket also means `send_fd` never has to close it on error: nothing was
/// transferred until the kernel accepted the message.
pub fn send_fd(socket: impl AsFd, fd: BorrowedFd<'_>) -> Result<(), FdTransferError> {
    let fds = [fd];
    let message = SendAncillaryMessage::ScmRights(&fds);
    let mut space = [0u8; rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    // The buffer is sized for exactly one descriptor, so this cannot fail; a
    // `false` would mean rustix's accounting disagreed with itself, and sending
    // a descriptor-less message would be worse than an error.
    if !control.push(message) {
        return Err(FdTransferError::ControlBufferTooSmall);
    }
    let payload = [FD_PASS_BYTE];
    let sent = sendmsg(
        socket.as_fd(),
        &[IoSlice::new(&payload)],
        &mut control,
        SendFlags::NOSIGNAL,
    )?;
    if sent != payload.len() {
        return Err(FdTransferError::ShortSend {
            sent,
            expected: payload.len(),
        });
    }
    Ok(())
}

/// Receive one descriptor from `socket`, waiting at most `timeout` in total.
///
/// The bound covers the whole call rather than each wake-up, so a peer that
/// trickles bytes cannot hold the handoff open: on expiry the answer is
/// [`FdTransferError::TimedOut`]. [`DEFAULT_FD_TRANSFER_TIMEOUT`] is the bound
/// the handoff uses.
///
/// The socket may be a stream or a packet socket, and the type is read once
/// (`SO_TYPE`): on a stream socket zero bytes and no descriptor is
/// end-of-stream, while on a packet socket an empty message is a *message*
/// (recv(2)) and the wait continues to the deadline. `SOCK_SEQPACKET` is
/// deliberately *not* required — macOS has no `AF_UNIX` `SOCK_SEQPACKET`, so
/// demanding one would make its handoff impossible — but a caller that brings
/// one gets its message boundaries honoured rather than mistaken for a hangup.
///
/// The `socket` must be one the caller owns and expects a descriptor on —
/// never a listening socket an attacker could reach: this side accepts from
/// whoever is connected to it, and a terminal descriptor is worth stealing.
/// Ownership is the caller's to establish (a `socketpair`, or an accepted
/// connection whose peer was identified before the first byte).
pub fn recv_fd(socket: impl AsFd, timeout: Duration) -> Result<OwnedFd, FdTransferError> {
    let deadline = Instant::now() + timeout;
    // Read once, before the loop: what an empty message means depends on the
    // socket kind, and a caller that hands over a socket this call cannot ask
    // about is refused rather than guessed at.
    let is_stream = get_socket_type(&socket)? == SocketType::STREAM;
    // Room for two descriptors, not one: one is the protocol, and the second is
    // what makes a violation *visible* — the `extra` count below can report a
    // message that packed two, instead of losing the second one to truncation.
    let mut space = [0u8; rustix::cmsg_space!(ScmRights(2))];
    let mut payload = [0u8; 1];
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(FdTransferError::TimedOut(timeout))?;
        // Only `poll` waits, and only for what is left of the deadline;
        // `recvmsg` below never blocks (`MSG_DONTWAIT`), so a ready-but-empty
        // socket cannot turn into an indefinite wait here.
        let millis = i32::try_from(remaining.as_millis()).unwrap_or(i32::MAX);
        let mut fds = [PollFd::new(&socket, PollFlags::IN)];
        match poll(&mut fds, millis) {
            Ok(0) => return Err(FdTransferError::TimedOut(timeout)),
            Ok(_) => {}
            Err(Errno::INTR) => continue,
            Err(err) => return Err(err.into()),
        }

        let mut control = RecvAncillaryBuffer::new(&mut space);
        let mut flags = RecvFlags::DONTWAIT;
        // A received descriptor must not survive an `exec` in this process.
        // `MSG_CMSG_CLOEXEC` makes the kernel install it close-on-exec before
        // it is visible at all — and it is Linux/Android-only. Elsewhere the
        // flag does not exist and the `fcntl` below is the whole of the
        // protection: it guarantees that no `exec` *after* it inherits the
        // descriptor, not that a thread exec'ing in the moment between the
        // kernel installing it and that call would not.
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            flags |= RecvFlags::CMSG_CLOEXEC;
        }
        let received = match recvmsg(
            socket.as_fd(),
            &mut [IoSliceMut::new(&mut payload)],
            &mut control,
            flags,
        ) {
            // Readiness raced with someone else, or a signal: re-poll, still
            // inside the deadline.
            Err(Errno::AGAIN | Errno::INTR) => continue,
            Err(err) => return Err(err.into()),
            Ok(received) => received,
        };

        // Take every descriptor out of the buffer, then judge. Nothing may
        // `break` out of this loop: a control message the drain never reached
        // is a descriptor the process keeps open forever. What is collected
        // here but not returned is closed by `OwnedFd`'s `Drop`.
        let mut first: Option<OwnedFd> = None;
        let mut extra = 0usize;
        for message in control.drain() {
            let RecvAncillaryMessage::ScmRights(descriptors) = message else {
                // Credentials carry no descriptor; nothing to close.
                continue;
            };
            for fd in descriptors {
                if first.is_none() {
                    first = Some(fd);
                } else {
                    extra += 1;
                }
            }
        }

        if extra > 0 {
            return Err(FdTransferError::TooManyDescriptors { got: extra + 1 });
        }
        let Some(fd) = first else {
            if received.bytes == 0 {
                if is_stream {
                    // End-of-stream before any payload byte: the peer is gone
                    // and transferred nothing.
                    return Err(FdTransferError::PeerClosed);
                }
                // An empty message on a packet socket is a message, not
                // end-of-stream (recv(2)). A live peer sends one whenever it
                // likes, and ending the receive on it would abandon a transfer
                // that is still coming: keep waiting, inside the deadline.
                continue;
            }
            return Err(FdTransferError::NoDescriptor);
        };
        if received.bytes == 0 {
            return Err(FdTransferError::NoPayload);
        }
        if payload[0] != FD_PASS_BYTE {
            return Err(FdTransferError::WrongPayload {
                got: payload[0],
                expected: FD_PASS_BYTE,
            });
        }
        // The `fcntl` that makes close-on-exec unconditional: on Linux/Android
        // `MSG_CMSG_CLOEXEC` has already done it, and everywhere else this is
        // the only thing that does — see the flag's comment above for exactly
        // how far the guarantee reaches there.
        rustix::io::fcntl_setfd(&fd, FdFlags::CLOEXEC)?;
        return Ok(fd);
    }
}

/// The geometry a sender claims for a master it is handing over.
///
/// Named rather than a `(u16, u16)` on purpose. This crate speaks `(cols,
/// rows)` where it speaks in pairs ([`super::Pane::size`],
/// [`super::Pane::resize`]) while `portable_pty::PtySize` — which a stage-1
/// manifest is built from — is declared rows-first, so an unnamed pair passed
/// positionally transposes silently. And it transposes in the one branch where
/// the value is used at all (a master that has no geometry of its own), where a
/// transposed value is not a wrong report but an agent's terminal resized the
/// wrong way round. The fields below are public and named: there is no
/// positional way to build one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdoptSize {
    pub cols: u16,
    pub rows: u16,
}

/// A PTY master that arrived as a descriptor rather than from `openpty`.
#[derive(Debug)]
pub(crate) struct AdoptedMaster {
    fd: OwnedFd,
    took_writer: AtomicBool,
    /// Resolved once, at adoption: `ptsname` on a master names its slave.
    /// `None` only where the platform has no such lookup (see [`slave_name`]).
    tty_name: Option<PathBuf>,
}

impl AdoptedMaster {
    /// Validate `fd` as a PTY master. Changes nothing about it.
    ///
    /// The refusal is deliberate: the handoff can offer this primitive any
    /// descriptor it likes, and a pane holding something that is not a
    /// terminal would put an arbitrary descriptor behind the PTY API.
    ///
    /// Nothing here touches the *state* of the terminal. A caller that can
    /// still refuse the transfer must be able to refuse it without leaving a
    /// mark: the sender is a live daemon that goes on serving this very pane if
    /// the handoff does not commit, so a refusal that had already resized its
    /// terminal would damage the pane it declined to adopt.
    /// [`Self::repair_geometry`] is the one mutating step, and it is the
    /// caller's to place after every check that can still refuse.
    pub(crate) fn new(fd: OwnedFd) -> Result<Self, PtyError> {
        // `isatty` refuses a socket, a file, an eventfd — anything that is not
        // a terminal at all. `ptsname` refuses the other end of a PTY: a slave
        // *is* a terminal, so `isatty` accepts it, and adopting it would leave
        // the pane reading its own input instead of the child's output. On
        // Linux `ptsname` is the `TIOCGPTN` ioctl, which only a master answers.
        // Every early return drops `fd`, closing it.
        if !rustix::termios::isatty(&fd) {
            return Err(PtyError::NotATerminal);
        }
        let tty_name = match slave_name(&fd) {
            Some(Ok(name)) => Some(name),
            // A terminal that will not name its slave is the slave end itself.
            Some(Err(_)) => return Err(PtyError::NotPtyMaster),
            None => None,
        };
        Ok(Self {
            fd,
            took_writer: AtomicBool::new(false),
            tty_name,
        })
    }

    /// A descriptor on this same terminal for a caller that must be able to
    /// re-read the terminal later, without borrowing the master.
    ///
    /// One `dup` of the master, close-on-exec. It keeps the *master* open, not
    /// the slave, so it cannot hold a dead pane's end-of-stream back.
    pub(crate) fn verification_fd(&self) -> io::Result<Arc<OwnedFd>> {
        cloexec_dup(self.fd.as_fd()).map(Arc::new)
    }

    /// Put the kernel's geometry in agreement with the sender's `size`.
    ///
    /// The only step that changes the inherited terminal, and therefore the
    /// caller's to order last: see [`Self::new`].
    ///
    /// The kernel's geometry is the one that counts — it is what the child sees
    /// and what `size()` reports — so `size` is used only to repair a master
    /// that has no geometry at all (a terminal is 0×0 until someone sets it).
    /// Re-applying the caller's numbers over a live geometry would let a stale
    /// sender resize an agent's terminal, which is the damage the handoff
    /// exists to avoid; and a master whose size cannot be read at all is
    /// treated as unset, because setting it is the safe repair.
    ///
    /// "No geometry at all" is `0×0`, both dimensions — not either one of them.
    /// One dimension at 0 is a live terminal that was *set* that way
    /// (`TIOCSWINSZ`, `stty rows 0`), and it is exactly what the rule above
    /// must leave alone: treating it as unset would resize a live agent's
    /// terminal and `SIGWINCH` its child, which is the damage this branch
    /// exists to prevent.
    pub(crate) fn repair_geometry(&self, size: AdoptSize) -> Result<(), PtyError> {
        let unset = self
            .get_size()
            .map(|current| current.cols == 0 && current.rows == 0)
            .unwrap_or(true);
        if unset {
            self.resize(PtySize {
                rows: size.rows,
                cols: size.cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
        }
        Ok(())
    }

    /// What the terminal makes of a child pid the sender claimed.
    ///
    /// The pid travels in the same untrusted message as the descriptor, and a
    /// pid is not inert: `pidfd_open` on a same-user process needs no
    /// permission check, so a pid an attacker chose makes the killer built from
    /// this pane signal an unrelated process, and `try_wait` attribute *that*
    /// process's death to this pane. The one thing that binds the two is the
    /// terminal itself: portable-pty puts the child in a session of its own as
    /// its leader (`setsid` + `TIOCSCTTY`), so the master's session leader *is*
    /// the pid the sender forked, and `tcgetsid` on this master names it. A
    /// named session that is a different pid is a claim the terminal has
    /// contradicted, and that refuses the adoption.
    ///
    /// A terminal that names *no* session is a third answer, and it is not a
    /// contradiction — nor a death. `tcgetsid` failing means the terminal has
    /// no session, which is not the same as the child having exited: a child
    /// that never had the terminal as its controlling tty has none while it is
    /// perfectly alive, and so does one that dropped it (`TIOCNOTTY`) and kept
    /// running. The terminal cannot confirm the claim there, so the claim is
    /// dropped — the pid never becomes a signal target, and none of it is
    /// attributed to the pane — but *nothing* is concluded about the child.
    /// [`ClaimedPid::NoSession`] therefore means "no pid" and not "dead"; the
    /// pane is watched the way a pane handed over with no pid is, by
    /// end-of-stream on the master, which a genuinely dead child reaches on its
    /// own and which a live one does not. Inventing a death here would report a
    /// live agent as exited while it kept writing to the pane.
    ///
    /// Only called when the sender claimed a pid; `None` needs no check, and a
    /// pane handed over that way is watched by end-of-stream alone.
    pub(crate) fn claimed_pid(&self, claimed: u32) -> Result<ClaimedPid, PtyError> {
        match session_leader(self.fd.as_fd()) {
            None => Ok(ClaimedPid::NoSession),
            Some(session) if session == claimed => Ok(ClaimedPid::SessionLeader),
            Some(session) => Err(PtyError::ChildPidMismatch { claimed, session }),
        }
    }
}

/// What the terminal made of a child pid the sender claimed (see
/// [`AdoptedMaster::claimed_pid`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ClaimedPid {
    /// The master's session leader is that very pid: the process the sending
    /// daemon forked. Safe to watch, and the only thing safe to signal.
    SessionLeader,
    /// The terminal names no session, so nothing confirmed the claim: the pid
    /// is dropped rather than trusted, and the pane is watched by end-of-stream
    /// alone — the no-pid semantics. Not a death: a session-less terminal can
    /// belong to a child that is running right now.
    NoSession,
}

/// The session leader of `fd`'s terminal, where the kernel names one.
///
/// `None` is "this terminal has no session", which rustix reports as an error
/// (`ENOTTY` on Linux, from `TIOCGSID`). It says nothing about whether anything
/// is alive behind the terminal — see [`AdoptedMaster::claimed_pid`].
fn session_leader(fd: BorrowedFd<'_>) -> Option<u32> {
    let session = rustix::termios::tcgetsid(fd).ok()?;
    // A session id is a positive pid. One that is not is not a session the
    // handoff could compare a claim against.
    pid_number(session).filter(|session| *session > 0)
}

/// A `Pid` as the raw number the handoff's manifest speaks, where it fits.
fn pid_number(pid: Pid) -> Option<u32> {
    u32::try_from(pid.as_raw_nonzero().get()).ok()
}

/// `ptsname` of a master: its slave's path, and — because it is the `TIOCGPTN`
/// ioctl on Linux — a refusal for anything that is not a master.
///
/// The two definitions below carry mirrored `cfg` predicates: the platforms
/// where rustix implements `ptsname` (the same set rustix itself compiles it
/// for) get the lookup and the master check, and the rest accept any terminal,
/// which is what portable-pty does everywhere — it never distinguishes the two
/// ends at all. `None` is "this platform cannot tell", `Some(Err(..))` is "this
/// descriptor is not a master".
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "fuchsia",
    target_os = "illumos",
    target_vendor = "apple"
))]
fn slave_name(fd: &OwnedFd) -> Option<io::Result<PathBuf>> {
    Some(
        rustix::pty::ptsname(fd, Vec::new())
            .map(|name| PathBuf::from(OsStr::from_bytes(name.to_bytes())))
            .map_err(io::Error::from),
    )
}

/// The mirror of the definition above: no `ptsname`, so no master check.
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "freebsd",
    target_os = "fuchsia",
    target_os = "illumos",
    target_vendor = "apple"
)))]
fn slave_name(_fd: &OwnedFd) -> Option<io::Result<PathBuf>> {
    None
}

impl MasterPty for AdoptedMaster {
    fn resize(&self, size: PtySize) -> Result<(), anyhow::Error> {
        rustix::termios::tcsetwinsize(
            self.fd.as_fd(),
            Winsize {
                ws_row: size.rows,
                ws_col: size.cols,
                ws_xpixel: size.pixel_width,
                ws_ypixel: size.pixel_height,
            },
        )?;
        Ok(())
    }

    fn get_size(&self) -> Result<PtySize, anyhow::Error> {
        let size = rustix::termios::tcgetwinsize(self.fd.as_fd())?;
        Ok(PtySize {
            rows: size.ws_row,
            cols: size.ws_col,
            pixel_width: size.ws_xpixel,
            pixel_height: size.ws_ypixel,
        })
    }

    fn try_clone_reader(&self) -> Result<Box<dyn std::io::Read + Send>, anyhow::Error> {
        // A `File` only wraps the descriptor. A read on a master whose last
        // slave closed reports `EIO` on Linux; the pane's pump ends on that
        // exactly as it ends on a 0-length read.
        Ok(Box::new(std::fs::File::from(cloexec_dup(self.fd.as_fd())?)))
    }

    fn take_writer(&self) -> Result<Box<dyn std::io::Write + Send>, anyhow::Error> {
        if self.took_writer.swap(true, Ordering::SeqCst) {
            anyhow::bail!("cannot take writer more than once");
        }
        // Deliberately not portable-pty's writer: that one writes a newline and
        // VEOF when dropped, which is right for a pane this process spawned
        // (dropping the last writer hangs the shell up) and wrong for an
        // adopted one. What this code guarantees is its own half of that: the
        // adopted writer must not, and does not, send an EOT when the adopting
        // daemon drops it — a hangup at that instant would kill the very agent
        // the handoff exists to keep alive.
        //
        // The sender's half is not in this file and is not guaranteed here: the
        // outgoing daemon's writer is portable-pty's, whose `Drop` writes a
        // newline and VEOF. Nothing an adopted pane does can stop that, so the
        // sender must not drop its master or writer while the agent is still
        // meant to live — a rule for stage 1's side of the handoff, not for the
        // adopted one.
        Ok(Box::new(std::fs::File::from(cloexec_dup(self.fd.as_fd())?)))
    }

    fn as_raw_fd(&self) -> Option<RawFd> {
        Some(self.fd.as_raw_fd())
    }

    fn tty_name(&self) -> Option<PathBuf> {
        self.tty_name.clone()
    }

    fn process_group_leader(&self) -> Option<i32> {
        // The trait spells this as `libc::pid_t`, which is `i32` on every
        // platform this crate builds for.
        rustix::termios::tcgetpgrp(self.fd.as_fd())
            .ok()
            .map(|pid| pid.as_raw_nonzero().get())
    }
}

/// Where an adopted pane's exit signal comes from.
#[derive(Debug)]
enum ExitProbe {
    /// `pidfd_open` succeeded (Linux 5.3+): a poll on this descriptor reports
    /// the process's death exactly, and — unlike a bare pid — cannot be
    /// confused by pid reuse.
    #[cfg(target_os = "linux")]
    Pidfd(Arc<OwnedFd>),
    /// No usable process handle: a Linux kernel without `pidfd_open` (older
    /// than 5.3), a call that failed for another reason, or a platform with no
    /// such descriptor at all (the BSDs and macOS watch processes through
    /// kqueue, which is not worth a second mechanism for this). End-of-stream
    /// is what is left, and it is strictly weaker: it lags the death until the
    /// last slave descriptor closes (a grandchild still holding the terminal
    /// keeps it open), and it cannot tell "exited" from "the terminal went
    /// away".
    Unavailable,
    /// `pidfd_open` answered `ESRCH`: the pid is already gone.
    AlreadyGone,
}

/// The child behind an adopted master.
///
/// It is not this process's child, so `waitpid` can never be called on it and
/// its status is unreachable by construction. What *is* reachable is the fact
/// of death, and that is what this handle reports.
#[derive(Debug)]
pub(crate) struct AdoptedChild {
    pid: Option<Pid>,
    probe: ExitProbe,
    /// The terminal the pid was confirmed against, kept only where the pid
    /// itself is the only remaining handle on the process (see
    /// [`AdoptedChild::new`]). [`AdoptedKiller`] re-reads it before signalling.
    terminal: Option<Arc<OwnedFd>>,
    /// The pane's reader-pump flag, set when the master reaches
    /// end-of-stream. Shared rather than copied: it is the same fact.
    closed: Arc<AtomicBool>,
    /// Death is decided once and cached. A pid can be reused after the process
    /// is reaped, and a later poll must never contradict an earlier answer.
    exited: Option<ExitStatus>,
}

impl AdoptedChild {
    /// Watch a process adopted from another daemon.
    ///
    /// A pid that is `Some` has already been checked against the terminal it
    /// arrived with ([`AdoptedMaster::claimed_pid`], through `Pane::adopt`);
    /// this handle trusts its input, and must not be built from an unverified
    /// one. When the sharpest probe this kernel offers is a bare pid — no
    /// pidfd, so the number is the only handle on the process — the terminal is
    /// kept too, for [`AdoptedKiller`] to re-check before it signals: a pid
    /// confirmed once at adoption can name an unrelated process by the time a
    /// kill arrives.
    ///
    /// Failing on that `dup` is the only way this can fail, and it fails the
    /// adoption rather than the pane: keeping the terminal is what makes a
    /// signal safe, so a pane that cannot keep it is not one this code should
    /// build a killer for.
    pub(crate) fn new(
        pid: Option<u32>,
        master: &AdoptedMaster,
        closed: Arc<AtomicBool>,
    ) -> Result<Self, PtyError> {
        let pid = pid
            .and_then(|raw| i32::try_from(raw).ok())
            .and_then(Pid::from_raw);
        let probe = match pid {
            None => ExitProbe::Unavailable,
            Some(pid) => probe_for(pid),
        };
        let terminal = match (pid, &probe) {
            // A pidfd refers to the process itself and cannot be re-pointed at
            // another one, so it needs no second opinion — which is also why
            // the extra descriptor is not paid for on kernels that have one.
            (Some(_), ExitProbe::Pidfd(_)) => None,
            (Some(_), _) => Some(master.verification_fd()?),
            (None, _) => None,
        };
        Ok(Self {
            pid,
            probe,
            terminal,
            closed,
            exited: None,
        })
    }

    /// Signal the process, preferring the pidfd: the process is not a child, so
    /// nothing keeps its pid reserved for us and a bare `kill` could reach an
    /// unrelated process that reused it.
    fn killer(&self) -> AdoptedKiller {
        killer_for(self.pid, &self.probe, self.terminal.as_ref())
    }
}

impl Child for AdoptedChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = &self.exited {
            return Ok(Some(status.clone()));
        }
        // Either signal ends the pane. A pidfd is definitive; end-of-stream is
        // weaker — a process that closes its terminal and keeps running raises
        // it too — but it is the only signal a kernel without `pidfd_open`
        // offers, and it is the very signal the spawned-pane path already
        // treats as the end of the child.
        let gone = match &self.probe {
            ExitProbe::AlreadyGone => true,
            #[cfg(target_os = "linux")]
            ExitProbe::Pidfd(pidfd) => pidfd_reports_exit(pidfd),
            ExitProbe::Unavailable => false,
        } || self.closed.load(Ordering::SeqCst);
        if !gone {
            return Ok(None);
        }
        // Gone — but the code it exited with died with the daemon that forked
        // it, and no syscall can recover it. `UNKNOWN_EXIT` says exactly that;
        // reporting 0 here would claim a clean exit nobody observed.
        let status = ExitStatus::with_exit_code(UNKNOWN_EXIT);
        self.exited = Some(status.clone());
        Ok(Some(status))
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        // Unbounded on purpose: `wait` means "block until it is over", and both
        // probes can reach that answer. `Pane::wait_timeout` is the bounded
        // call, and it polls through `try_wait` itself.
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn process_id(&self) -> Option<u32> {
        self.pid.and_then(pid_number)
    }
}

impl ChildKiller for AdoptedChild {
    fn kill(&mut self) -> io::Result<()> {
        self.killer().signal()
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(self.killer())
    }
}

/// A killer that outlives the pane, for a caller that may block in `wait`
/// elsewhere (what `ChildKiller::clone_killer` is for).
#[derive(Debug)]
struct AdoptedKiller {
    pid: Option<Pid>,
    /// Signalling through the pidfd rather than the pid, where the kernel has
    /// one: see [`killer_for`].
    #[cfg(target_os = "linux")]
    pidfd: Option<Arc<OwnedFd>>,
    /// The terminal the pid was confirmed against, where the pid is the only
    /// handle on the process: see [`AdoptedKiller::signal`].
    terminal: Option<Arc<OwnedFd>>,
}

impl AdoptedKiller {
    fn signal(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            if let Some(pidfd) = &self.pidfd {
                return rustix::process::pidfd_send_signal(pidfd.as_fd(), Signal::Kill)
                    .map_err(io::Error::from);
            }
        }
        let Some(pid) = self.pid else {
            // Nothing was handed over to signal: report it, rather than a kill
            // that cannot have happened.
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "adopted pane has no pid to signal",
            ));
        };
        // No pidfd: a bare number is the whole of what this call has, and the
        // number was confirmed *once*, at adoption. The child may have exited
        // and been reaped since — nobody keeps its pid reserved for a process
        // that is not our child — so the same number can now name an unrelated
        // process, and killing that would be far worse than not killing a
        // corpse. The terminal is asked again, immediately before the signal:
        // it is the one witness that was never guessing.
        //
        // This refuses a process the terminal no longer names as its session
        // leader. That is not a claim that such a process is dead — a live child
        // can drop its controlling tty — but a pid that cannot be re-identified
        // is not one this code may signal, and the caller gets the error rather
        // than a guess.
        if let Some(terminal) = &self.terminal {
            if session_leader(terminal.as_fd()) != pid_number(pid) {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "the terminal no longer names this pid as its session leader: \
                     the process it was confirmed against is gone, and the number \
                     may have been reused",
                ));
            }
        }
        kill_process(pid, Signal::Kill).map_err(io::Error::from)
    }
}

impl ChildKiller for AdoptedKiller {
    fn kill(&mut self) -> io::Result<()> {
        self.signal()
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        #[cfg(target_os = "linux")]
        {
            Box::new(Self {
                pid: self.pid,
                pidfd: self.pidfd.clone(),
                terminal: self.terminal.clone(),
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            Box::new(Self {
                pid: self.pid,
                terminal: self.terminal.clone(),
            })
        }
    }
}

/// Open the sharpest process handle this kernel offers for `pid`.
#[cfg(target_os = "linux")]
fn probe_for(pid: Pid) -> ExitProbe {
    match pidfd_open(pid, PidfdFlags::empty()) {
        Ok(pidfd) => ExitProbe::Pidfd(Arc::new(pidfd)),
        Err(Errno::SRCH) => ExitProbe::AlreadyGone,
        // No `pidfd_open` (kernel older than 5.3) or a refusal: end-of-stream
        // is what is left, and it does not cost us the pane.
        Err(_) => ExitProbe::Unavailable,
    }
}

/// No kernel process descriptor: end-of-stream only.
#[cfg(not(target_os = "linux"))]
fn probe_for(_pid: Pid) -> ExitProbe {
    ExitProbe::Unavailable
}

/// The killer for an adopted child: `pidfd_send_signal` where there is a pidfd,
/// a plain `kill` — re-checked against `terminal` first — otherwise.
#[cfg(target_os = "linux")]
fn killer_for(
    pid: Option<Pid>,
    probe: &ExitProbe,
    terminal: Option<&Arc<OwnedFd>>,
) -> AdoptedKiller {
    AdoptedKiller {
        pid,
        pidfd: match probe {
            ExitProbe::Pidfd(pidfd) => Some(Arc::clone(pidfd)),
            _ => None,
        },
        // Only meaningful without a pidfd, and `AdoptedChild::new` does not keep
        // one where there is: see [`AdoptedKiller::signal`].
        terminal: terminal.cloned(),
    }
}

#[cfg(not(target_os = "linux"))]
fn killer_for(
    pid: Option<Pid>,
    _probe: &ExitProbe,
    terminal: Option<&Arc<OwnedFd>>,
) -> AdoptedKiller {
    AdoptedKiller {
        pid,
        terminal: terminal.cloned(),
    }
}

/// Does the pidfd report that its process has exited?
///
/// A pidfd becomes readable when the process behind it exits and never
/// elsewhere, so a zero-timeout poll is the probe. A failing poll reports "not
/// yet": inventing a death from a transient error would be worse than waiting
/// for the next poll, and end-of-stream still covers this pane.
#[cfg(target_os = "linux")]
fn pidfd_reports_exit(pidfd: &OwnedFd) -> bool {
    let mut fds = [PollFd::new(pidfd, PollFlags::IN)];
    matches!(poll(&mut fds, 0), Ok(ready) if ready > 0)
}

/// `dup` with `CLOEXEC` set in the same call: the pump's reader and the pane's
/// writer must not survive an `exec` in the adopting daemon, which goes on to
/// spawn the next generation's panes.
fn cloexec_dup(fd: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    rustix::io::fcntl_dupfd_cloexec(fd, 0).map_err(io::Error::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A killer of the shape `AdoptedChild::new` builds where the kernel offers
    /// no pidfd — which is every platform but Linux, and every Linux older than
    /// 5.3, so no test on this machine reaches it through `Pane::adopt`.
    fn bare_pid_killer(pid: Pid, terminal: &Arc<OwnedFd>) -> AdoptedKiller {
        AdoptedKiller {
            pid: Some(pid),
            #[cfg(target_os = "linux")]
            pidfd: None,
            terminal: Some(Arc::clone(terminal)),
        }
    }

    /// The bare-pid fallback is the one signal path whose handle can be
    /// re-pointed: the pid was confirmed against the terminal *once*, at
    /// adoption, and a process that is not our child has nobody keeping its
    /// number reserved. A pid that has exited and been reaped can therefore
    /// name an unrelated process by the time a kill arrives, so the terminal is
    /// asked again immediately before the signal.
    ///
    /// What breaks this test if the re-check is dropped: `signal` falls through
    /// to `kill_process` and SIGKILLs the bystander, so the `expect_err` fails
    /// — and the comment below records what the second assertion would have
    /// been looking at.
    #[test]
    fn a_bare_pid_is_rechecked_against_the_terminal_before_it_is_signalled() {
        // A terminal that names no session: what the confirmation of a pid
        // decays to once the child behind it has exited.
        let terminal = Arc::new(
            rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)
                .expect("open a pty master"),
        );
        assert_eq!(
            session_leader(terminal.as_fd()),
            None,
            "this test means nothing unless the terminal names no session"
        );

        // A live process the reused number would stand for: it must survive the
        // attempt, because nothing the terminal can identify was addressed.
        let mut bystander = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn a bystander");
        let pid = Pid::from_raw(i32::try_from(bystander.id()).expect("a pid fits an i32"))
            .expect("a positive pid");

        let err = bare_pid_killer(pid, &terminal)
            .signal()
            .expect_err("a pid the terminal no longer names must not be signalled");
        assert_eq!(err.kind(), io::ErrorKind::NotFound, "got {err:?}");
        assert!(
            bystander.try_wait().expect("poll the bystander").is_none(),
            "the re-check must have refused before any signal was delivered"
        );

        bystander.kill().expect("clean up the bystander");
        bystander.wait().expect("reap the bystander");
    }
}
