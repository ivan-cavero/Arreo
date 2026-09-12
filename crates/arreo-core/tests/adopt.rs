//! T-0038 stage 0 acceptance: adopt a PTY master that arrived as a descriptor.
//!
//! Unix-only by nature — the primitive exists to receive a master over
//! `SCM_RIGHTS`, which has no Windows analogue (T-0039 routes that case).
//!
//! The child below is spawned by this very process, which the real handoff's
//! shape is not: there the adopted child belongs to the daemon being replaced.
//! That difference is not load-bearing for what these tests prove, because
//! nothing on the adopted path asks about parentage — `waitpid` is never called
//! on an adopted pane — so the exit detection exercised here is exactly what a
//! daemon that did not fork the child gets.

#![cfg(unix)]

use std::io::{IoSlice, Read, Write};
use std::os::unix::io::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use arreo_core::pty::adopt::{
    recv_fd, send_fd, AdoptSize, FdTransferError, DEFAULT_FD_TRANSFER_TIMEOUT, FD_PASS_BYTE,
};
use arreo_core::pty::{ExitState, Pane, PtyError, SpawnSpec, UNKNOWN_EXIT};
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtyPair, PtySize};
use rustix::net::{sendmsg, SendAncillaryBuffer, SendAncillaryMessage, SendFlags};
use rustix::termios::Winsize;

/// Reap the child the way the daemon that forked it can — through its own
/// handle — with a deadline, so a lie in the adopted pane cannot hang the test.
///
/// Not instant on purpose: the adopted pane learns of the death from the
/// terminal reaching end-of-stream (and from the pidfd once the process is
/// gone), and closing the descriptors is a step of exiting that happens just
/// before the process becomes reapable. Waiting on the reap is the honest
/// assertion, not demanding it in the same instant.
fn reap(sender: &mut Sender, timeout: Duration) -> portable_pty::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = sender.child.try_wait().expect("reap on the sending side") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "the forking side never reaped the child"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The sending side of a handoff: a live pty whose child is already running,
/// kept whole so a test can inspect the same terminal from the daemon that
/// opened it.
struct Sender {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl Sender {
    /// Open a pty, start `program` in it, and hand the master over the way the
    /// handoff does: duplicated, sent as `SCM_RIGHTS` over a `socketpair`, and
    /// received back. The test then adopts what came out of the socket, which
    /// is what the daemon being replaced actually delivers.
    fn open(program: &str, args: &[&str]) -> (Self, OwnedFd, u32) {
        Self::open_with(program, args, true)
    }

    /// The same, but with a child that never takes the terminal as its
    /// controlling tty: portable-pty's `setsid` without the `TIOCSCTTY` that
    /// normally follows it (`CommandBuilder::set_controlling_tty(false)`). The
    /// child is in a session of its own and the terminal belongs to none, which
    /// is a state a live child can be in — see
    /// `adopt_does_not_read_a_session_less_terminal_as_a_dead_pane`.
    fn open_sessionless(program: &str, args: &[&str]) -> (Self, OwnedFd, u32) {
        Self::open_with(program, args, false)
    }

    /// `controlling_tty` selects whether the child calls `TIOCSCTTY` after
    /// `setsid` — the one difference between an ordinary pane and a session-less
    /// one.
    fn open_with(program: &str, args: &[&str], controlling_tty: bool) -> (Self, OwnedFd, u32) {
        let PtyPair { master, slave } = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.set_controlling_tty(controlling_tty);
        let child = slave.spawn_command(cmd).expect("spawn in the pty");
        // Only the child may hold the slave: a copy kept here would keep the
        // master from ever reaching end-of-stream when the child dies.
        drop(slave);
        let pid = child.process_id().expect("the child has a pid");
        let master_fd = hand_over_master(master.as_ref());
        (Self { master, child }, master_fd, pid)
    }
}

/// The whole transfer, as the two daemons do it: duplicate the master, send it
/// over a `socketpair`, and return the descriptor the receiving side adopts.
fn hand_over_master(master: &dyn MasterPty) -> OwnedFd {
    let (sender, receiver) = UnixStream::pair().expect("transfer socketpair");
    send_fd(&sender, dup_master(master).as_fd()).expect("send the master");
    recv_fd(&receiver, DEFAULT_FD_TRANSFER_TIMEOUT).expect("receive the master")
}

/// Duplicate the master the way the outgoing daemon would before sending it:
/// an independently owned descriptor for the same terminal.
fn dup_master(master: &dyn MasterPty) -> OwnedFd {
    let raw = master.as_raw_fd().expect("a pty master has a descriptor");
    // SAFETY: `master` owns the descriptor and outlives this borrow, which is
    // used only to take a duplicate; nothing here closes the original.
    let borrowed = unsafe { BorrowedFd::borrow_raw(raw) };
    rustix::io::fcntl_dupfd_cloexec(borrowed, 0).expect("duplicate the master")
}

/// What the outgoing daemon remembers about the pane (T-0038 hands it over too).
fn spec() -> SpawnSpec {
    SpawnSpec {
        program: "/bin/sh".to_string(),
        args: Vec::new(),
    }
}

fn wait_for(pane: &Pane, needle: &str, timeout: Duration) -> Vec<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let lines = pane.drain();
        if lines.iter().any(|line| line.contains(needle)) {
            return lines;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {needle:?}; got: {lines:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The inode of the open file description `fd` refers to.
///
/// Two descriptors with the same inode are two handles on one object, which is
/// what "the received descriptor is the one that was sent" means, and what
/// "nothing is left open" is checked against.
fn descriptor_inode<T: AsRawFd>(fd: &T) -> u64 {
    let meta = std::fs::metadata(format!("/proc/self/fd/{}", fd.as_raw_fd())).expect("stat");
    std::os::unix::fs::MetadataExt::ino(&meta)
}

/// Every descriptor in this process that still refers to `inode`.
///
/// Read from the process's own descriptor table rather than inferred from a
/// proxy such as "the peer end became unwritable": a `fork` in a test running
/// in parallel briefly copies the whole table into the child, and that child's
/// copy keeps a socket alive without any leak on our side.
#[cfg(target_os = "linux")]
fn descriptor_copies(inode: u64) -> Vec<String> {
    let mut copies = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fd")
        .expect("read the descriptor table")
        .flatten()
    {
        let path = entry.path();
        if let Ok(meta) = std::fs::metadata(&path) {
            if std::os::unix::fs::MetadataExt::ino(&meta) == inode {
                copies.push(entry.file_name().to_string_lossy().to_string());
            }
        }
    }
    copies
}

/// Assert that exactly `expected` descriptors of this process refer to `inode`
/// — for a refusal, that is "no copy beyond the one the test still holds".
///
/// The observation needs the kernel to expose the descriptor table, which is
/// `/proc/self/fd` on Linux; elsewhere this is a no-op rather than a guess. The
/// close it stands for is unconditional on every platform: the descriptor is an
/// `OwnedFd`, dropped on every path out of `recv_fd` and `Pane::adopt`.
fn assert_descriptor_copies(inode: u64, expected: usize) {
    #[cfg(target_os = "linux")]
    {
        let copies = descriptor_copies(inode);
        assert_eq!(
            copies.len(),
            expected,
            "expected {expected} descriptor(s) for inode {inode}, found {copies:?}"
        );
    }
    #[cfg(not(target_os = "linux"))]
    let _ = (inode, expected);
}

/// The `tty-index` a Linux `fdinfo` file carries for a tty descriptor.
#[cfg(target_os = "linux")]
fn tty_index_of(fdinfo: &str) -> Option<u32> {
    fdinfo
        .lines()
        .find_map(|line| line.strip_prefix("tty-index:"))
        .and_then(|value| value.trim().parse().ok())
}

/// The pty index this process's descriptor is a handle on.
///
/// A pty master's inode cannot stand in for it the way a socket's does: every
/// master's `/proc/self/fd/N` resolves to `/dev/ptmx`, so *all* masters in the
/// process share one inode and an inode scan cannot see this descriptor at all.
/// The kernel's per-terminal index — also what `TIOCGPTN` reports — is the
/// identity that distinguishes them, and a `dup` of one master shares it.
#[cfg(target_os = "linux")]
fn master_tty_index<T: AsRawFd>(fd: &T) -> u32 {
    let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{}", fd.as_raw_fd()))
        .expect("read the descriptor's fdinfo");
    tty_index_of(&info).expect("a pty master's fdinfo carries its tty index")
}

/// Every descriptor in this process that is a handle on the master with
/// `tty_index`: the twin of [`descriptor_copies`] for a pty master.
#[cfg(target_os = "linux")]
fn master_copies(tty_index: u32) -> Vec<String> {
    let mut copies = Vec::new();
    for entry in std::fs::read_dir("/proc/self/fd")
        .expect("read the descriptor table")
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().to_string();
        // A concurrent close can take an entry away between the listing and
        // the read; that is not this test's descriptor and not a failure.
        let Ok(info) = std::fs::read_to_string(format!("/proc/self/fdinfo/{name}")) else {
            continue;
        };
        if tty_index_of(&info) == Some(tty_index) {
            copies.push(name);
        }
    }
    copies
}

/// Assert that exactly `expected` descriptors of this process are handles on
/// the master with `tty_index`.
///
/// Linux-only, and only ever called from Linux-gated assertions: the
/// observation is `/proc/self/fdinfo`, and the close it stands for does not
/// depend on the platform — the master is an `OwnedFd` dropped on every
/// refusal path.
#[cfg(target_os = "linux")]
fn assert_master_copies(tty_index: u32, expected: usize) {
    let copies = master_copies(tty_index);
    assert_eq!(
        copies.len(),
        expected,
        "expected {expected} descriptor(s) for tty index {tty_index}, found {copies:?}"
    );
}

/// Add one descriptor for `passed` and a payload byte to a control message.
fn send_raw(socket: impl AsFd, passed: impl AsFd, payload: &[u8]) -> usize {
    let descriptors = [passed.as_fd()];
    let message = SendAncillaryMessage::ScmRights(&descriptors);
    let mut space = [0u8; rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    assert!(control.push(message), "the control buffer is sized for one");
    sendmsg(
        socket.as_fd(),
        &[IoSlice::new(payload)],
        &mut control,
        SendFlags::empty(),
    )
    .expect("send the message")
}

#[test]
fn send_fd_round_trips_a_usable_cloexec_descriptor() {
    let (sender, receiver) = UnixStream::pair().expect("transfer socketpair");
    let (passed, mut peer) = UnixStream::pair().expect("descriptor to pass");
    let inode = descriptor_inode(&passed);

    send_fd(&sender, passed.as_fd()).expect("send the descriptor");
    let received = recv_fd(&receiver, DEFAULT_FD_TRANSFER_TIMEOUT).expect("receive it");

    // It is the same open file description, not a look-alike socket: the
    // process now holds exactly two handles on that object, and bytes written
    // at the peer end come out of the received one.
    assert_descriptor_copies(inode, 2);
    const MARKER: &[u8] = b"same-open-file-description";
    peer.write_all(MARKER).expect("write at the peer end");
    let mut received = std::fs::File::from(received);
    let mut buf = vec![0u8; MARKER.len()];
    received
        .read_exact(&mut buf)
        .expect("read through the received descriptor");
    assert_eq!(buf.as_slice(), MARKER);

    // Close-on-exec is not decoration: the adopting daemon goes on to spawn
    // children, and a terminal descriptor that survives `exec` would be handed
    // to every one of them.
    let flags = rustix::io::fcntl_getfd(&received).expect("F_GETFD");
    assert!(
        flags.contains(rustix::io::FdFlags::CLOEXEC),
        "the received descriptor must be close-on-exec, got {flags:?}"
    );
}

#[test]
fn recv_fd_waits_for_a_late_sender() {
    let (sender, receiver) = UnixStream::pair().expect("transfer socketpair");
    let (passed, _keep) = UnixStream::pair().expect("descriptor to pass");

    // Not a one-shot peek: a descriptor that arrives after the receive started
    // is still received, which is what makes the timeout a bound rather than a
    // delay.
    let late = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        send_fd(&sender, passed.as_fd()).expect("send after the delay");
    });
    let received = recv_fd(&receiver, Duration::from_secs(5)).expect("a late descriptor arrives");
    late.join().expect("the sender thread");
    rustix::io::fcntl_getfd(&received).expect("the received descriptor is live");
}

#[test]
fn recv_fd_gives_up_at_its_deadline() {
    let (_sender, receiver) = UnixStream::pair().expect("transfer socketpair");
    let timeout = Duration::from_millis(150);

    let started = Instant::now();
    let err = recv_fd(&receiver, timeout).expect_err("nothing is ever sent");
    let waited = started.elapsed();
    assert!(
        matches!(err, FdTransferError::TimedOut(got) if got == timeout),
        "got {err:?}"
    );
    assert!(
        waited < Duration::from_secs(5),
        "the receive is bounded, but waited {waited:?}"
    );
}

#[test]
fn recv_fd_distinguishes_the_ways_a_descriptor_can_fail_to_arrive() {
    // The peer went away without transferring anything.
    let (sender, receiver) = UnixStream::pair().expect("transfer socketpair");
    drop(sender);
    let err = recv_fd(&receiver, DEFAULT_FD_TRANSFER_TIMEOUT).expect_err("the peer is gone");
    assert!(
        matches!(err, FdTransferError::PeerClosed),
        "a closed peer is not a timeout: got {err:?}"
    );

    // A message with data but no descriptor is not a transfer.
    let (mut sender, receiver) = UnixStream::pair().expect("transfer socketpair");
    sender.write_all(&[FD_PASS_BYTE]).expect("payload only");
    let err = recv_fd(&receiver, DEFAULT_FD_TRANSFER_TIMEOUT).expect_err("no descriptor");
    assert!(matches!(err, FdTransferError::NoDescriptor), "got {err:?}");
}

#[test]
fn recv_fd_adopts_at_most_one_descriptor() {
    let (sender, receiver) = UnixStream::pair().expect("transfer socketpair");
    let (first, _first_peer) = UnixStream::pair().expect("first descriptor");
    let (second, _second_peer) = UnixStream::pair().expect("second descriptor");
    let (first_inode, second_inode) = (descriptor_inode(&first), descriptor_inode(&second));

    // Two descriptors in one message, the way a sender that leaked a
    // descriptor into its control buffer would.
    let descriptors = [first.as_fd(), second.as_fd()];
    let mut space = [0u8; rustix::cmsg_space!(ScmRights(2))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    assert!(control.push(SendAncillaryMessage::ScmRights(&descriptors)));
    let payload = [FD_PASS_BYTE];
    sendmsg(
        sender.as_fd(),
        &[IoSlice::new(&payload)],
        &mut control,
        SendFlags::empty(),
    )
    .expect("send two descriptors");

    let err = recv_fd(&receiver, DEFAULT_FD_TRANSFER_TIMEOUT).expect_err("the protocol is one");
    assert!(
        matches!(err, FdTransferError::TooManyDescriptors { got: 2 }),
        "got {err:?}"
    );

    // Neither was adopted or stashed: the only handles left on each object are
    // the ones this test made. A refusal that took the first, or dropped the
    // second on the floor, would leave an extra descriptor behind.
    assert_descriptor_copies(first_inode, 1);
    assert_descriptor_copies(second_inode, 1);
}

/// The marker byte cannot bind a transfer on a stream socket — the kernel
/// treats ancillary data as a barrier (unix(7)), so a byte queued ahead of the
/// descriptor is what the receive sees first — and this is what that costs: the
/// transfer is refused rather than read as if the marker belonged to it, and
/// the descriptor is not handed out. It is not lost either: a caller that
/// drains the channel to the transfer boundary first (the dedicated
/// `socketpair` the handoff uses) gets it intact.
///
/// What breaks this test is dropping the refusal of a message that carries
/// bytes and no descriptor: a receive that read the stale byte as its transfer
/// marker and kept waiting would return the descriptor on the *first* call
/// instead of `NoDescriptor`, and this test's `expect_err` fails. The byte is
/// deliberately the marker's own value, which is exactly the byte that must not
/// be mistaken for one on its own.
///
/// Linux-only: the observation is that a byte queued before the descriptor
/// arrives on its own, without the control message, which is the barrier
/// behaviour `FD_PASS_BYTE` describes for a stream socket.
#[cfg(target_os = "linux")]
#[test]
fn recv_fd_refuses_a_stream_transfer_queued_behind_a_stale_byte() {
    let (mut sender, receiver) = UnixStream::pair().expect("transfer socketpair");
    let (passed, _keep) = UnixStream::pair().expect("descriptor to pass");
    let inode = descriptor_inode(&passed);

    // A byte some other writer of a shared channel left in flight — the value
    // of the marker itself.
    sender
        .write_all(&[FD_PASS_BYTE])
        .expect("a byte already queued");
    send_fd(&sender, passed.as_fd()).expect("the legitimate transfer");

    let err = recv_fd(&receiver, DEFAULT_FD_TRANSFER_TIMEOUT)
        .expect_err("the stale byte is not a transfer");
    assert!(
        matches!(err, FdTransferError::NoDescriptor),
        "the byte arrived without a descriptor and must be refused as one: got {err:?}"
    );
    // Refused, nothing adopted: the only handle on the passed descriptor is
    // still the test's own.
    assert_descriptor_copies(inode, 1);

    // And nothing was burned: the transfer is still in the channel, whole, for
    // a caller that reads to the boundary.
    let received =
        recv_fd(&receiver, DEFAULT_FD_TRANSFER_TIMEOUT).expect("the transfer is still queued");
    assert_eq!(
        descriptor_inode(&received),
        inode,
        "the second receive is the descriptor that was sent"
    );
    assert_descriptor_copies(inode, 2);
    drop(received);
    assert_descriptor_copies(inode, 1);
}

#[test]
fn recv_fd_closes_a_descriptor_whose_payload_byte_is_wrong() {
    let (sender, receiver) = UnixStream::pair().expect("transfer socketpair");
    let (passed, _keep) = UnixStream::pair().expect("descriptor to pass");
    let inode = descriptor_inode(&passed);

    let payload = [FD_PASS_BYTE.wrapping_add(1)];
    assert_eq!(send_raw(&sender, &passed, &payload), payload.len());

    let err = recv_fd(&receiver, DEFAULT_FD_TRANSFER_TIMEOUT).expect_err("not this byte");
    assert!(
        matches!(err, FdTransferError::WrongPayload { got, .. } if got == payload[0]),
        "got {err:?}"
    );
    assert_descriptor_copies(inode, 1);
}

#[cfg(target_os = "linux")]
#[test]
fn recv_fd_closes_a_descriptor_that_arrives_with_no_data_at_all() {
    // A packet-oriented pair, because that is where a zero-length message can
    // carry a descriptor: on a stream socket a data-less `sendmsg` is not a
    // message at all and the kernel drops the descriptor with it. The receive
    // side must handle the shape regardless of the kind of socket it is handed,
    // and close what it refuses.
    //
    // Linux-only because that is where an `AF_UNIX` packet pair exists: macOS
    // has no `AF_UNIX` `SOCK_SEQPACKET` (ADR 0021 §2b), so the socket kind
    // itself cannot be created there, and the receive's packet-socket branch is
    // exercised where such a socket does.
    let (sender, receiver) = rustix::net::socketpair(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::SEQPACKET,
        rustix::net::SocketFlags::CLOEXEC,
        None,
    )
    .expect("seqpacket socketpair");
    let (passed, _keep) = UnixStream::pair().expect("descriptor to pass");
    let inode = descriptor_inode(&passed);

    assert_eq!(send_raw(&sender, &passed, &[]), 0, "no data bytes");

    let err = recv_fd(&receiver, DEFAULT_FD_TRANSFER_TIMEOUT).expect_err("no payload byte");
    assert!(matches!(err, FdTransferError::NoPayload), "got {err:?}");
    assert_descriptor_copies(inode, 1);
}

/// An empty message on a packet socket is a message, not end-of-stream
/// (recv(2)): a live peer that sends one must not end the receive, or a
/// transfer still on its way never arrives. Stream sockets keep their EOF
/// meaning — that is pinned by
/// `recv_fd_distinguishes_the_ways_a_descriptor_can_fail_to_arrive` — so it is
/// the socket type that decides.
///
/// What breaks this test if the type is not consulted: the receive reads the
/// empty message as a hangup and returns `PeerClosed` at once, instead of
/// waiting out its deadline for the descriptor that is still coming.
///
/// Linux-only for the same reason as the test above: macOS has no `AF_UNIX`
/// `SOCK_SEQPACKET` to send an empty message on (ADR 0021 §2b).
#[cfg(target_os = "linux")]
#[test]
fn recv_fd_waits_out_an_empty_packet_message_instead_of_reading_it_as_eof() {
    let (sender, receiver) = rustix::net::socketpair(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::SEQPACKET,
        rustix::net::SocketFlags::CLOEXEC,
        None,
    )
    .expect("seqpacket socketpair");
    assert_eq!(
        rustix::net::send(&sender, b"", SendFlags::empty()).expect("send an empty message"),
        0,
        "an empty message carries no bytes"
    );

    let timeout = Duration::from_millis(150);
    let err = recv_fd(&receiver, timeout).expect_err("no descriptor is ever sent");
    assert!(
        matches!(err, FdTransferError::TimedOut(got) if got == timeout),
        "an empty message is not a hangup: got {err:?}"
    );
}

#[test]
fn adopt_refuses_a_descriptor_that_is_not_a_terminal_and_closes_it() {
    let (not_a_tty, _keep) = UnixStream::pair().expect("socketpair");
    let inode = descriptor_inode(&not_a_tty);
    // Hand over the only copy this process holds, so the refusal is the only
    // thing that can close it.
    let fd: OwnedFd = not_a_tty.into();

    let err = Pane::adopt(fd, None, AdoptSize { cols: 80, rows: 24 }, spec())
        .err()
        .expect("a socket is not a terminal");
    assert!(matches!(err, PtyError::NotATerminal), "got {err:?}");
    assert_descriptor_copies(inode, 0);
}

/// The slave end of a terminal is a terminal too, so only the master check
/// refuses it — and adopting it would give the pane its own input. Linux-only:
/// there `ptsname` is the `TIOCGPTN` ioctl, which is what tells the ends apart.
///
/// The descriptor is moved into the call, so the refusal is the only thing that
/// can close it; this is the one refusal path that did not say so.
#[cfg(target_os = "linux")]
#[test]
fn adopt_refuses_the_slave_end_of_the_terminal() {
    let (sender, master_fd, _pid) = Sender::open("/bin/cat", &[]);
    let slave_path = sender.master.tty_name().expect("a master names its slave");
    let slave = std::fs::File::open(&slave_path).expect("open the slave end");
    let inode = descriptor_inode(&slave);
    assert_descriptor_copies(inode, 1);

    let err = Pane::adopt(slave.into(), None, AdoptSize { cols: 80, rows: 24 }, spec())
        .err()
        .expect("a slave is not a master");
    assert!(matches!(err, PtyError::NotPtyMaster), "got {err:?}");
    assert_descriptor_copies(inode, 0);
    drop(master_fd);
}

/// The pid arrives over the same untrusted channel as the descriptor, and it is
/// not inert: `pidfd_open` on a same-user process needs no permission check, so
/// an unchecked pid means `kill_shared` signals an unrelated process and
/// `try_wait` reports that process's death as this pane's. The terminal is what
/// settles it — the master's session leader is the process the sending daemon
/// forked — so a claim the terminal does not confirm is refused outright.
///
/// What breaks this test when the check is removed: the adoption *succeeds*,
/// the pane is built around a pid the terminal never agreed to, and this test
/// dies on the `expect_err` below (and, without the closure assertion, on the
/// leftover descriptor).
#[test]
fn adopt_refuses_a_pid_the_terminal_disagrees_with() {
    let (sender, master_fd, pid) = Sender::open("/bin/cat", &[]);
    let claimed = pid + 1;

    // What the refusal has to close, measured the way a pty master can be: see
    // `master_copies` for why the inode scan every socket refusal uses cannot
    // see a master at all.
    #[cfg(target_os = "linux")]
    let tty_index = {
        let index = master_tty_index(&master_fd);
        assert_eq!(
            master_copies(index).len(),
            2,
            "the sender's own master and the duplicate handed over"
        );
        index
    };

    let err = Pane::adopt(
        master_fd,
        Some(claimed),
        AdoptSize { cols: 80, rows: 24 },
        spec(),
    )
    .err()
    .expect("a pid the terminal does not confirm is not a pid this pane may signal");
    assert!(
        matches!(
            &err,
            PtyError::ChildPidMismatch { claimed: got_claimed, session }
                if *got_claimed == claimed && *session == pid
        ),
        "the refusal must name the claim ({claimed}) and the terminal's session ({pid}), got {err:?}"
    );

    // Refused *and* closed: the duplicate handed over is gone, the sender's own
    // master is not, and no handle on an unrelated process's terminal is left.
    #[cfg(target_os = "linux")]
    assert_master_copies(tty_index, 1);
    drop(sender);
}

/// A terminal with no session is not a dead pane.
///
/// `tcgetsid` failing means only that the terminal names no session: a child
/// that never took the terminal as its controlling tty has none while it is
/// running perfectly well (portable-pty reaches this state with
/// `set_controlling_tty(false)`, and a child can also drop its tty with
/// `TIOCNOTTY`). Reading that as a death would report a live agent as exited —
/// while it kept writing into the very ring buffer this pane reads.
///
/// The claim cannot be confirmed against such a terminal, so no pid is kept;
/// what the pane must *not* do is conclude anything from that. It reports
/// `Running` until the terminal itself reaches end-of-stream, which is the
/// pre-existing no-pid contract.
///
/// What breaks this test if the missing session is read as an exit: the
/// assertion after the adoption sees `Exited(UNKNOWN_EXIT)` instead of
/// `Running`, and the ring never grows because nothing is being pumped.
/// What breaks it if a session-less pane were built with a pid anyway: the
/// `child_pid`/kill assertions below would report and signal a pid nothing
/// confirmed.
#[cfg(target_os = "linux")]
#[test]
fn adopt_does_not_read_a_session_less_terminal_as_a_dead_pane() {
    let (mut sender, master_fd, pid) = Sender::open_sessionless(
        "/bin/sh",
        &[
            "-c",
            "i=0; while [ $i -lt 100 ]; do echo tick-$i; i=$((i+1)); sleep 0.1; done",
        ],
    );

    // The premise, stated as the kernel states it: the terminal names no
    // session, and the child is running.
    assert!(
        rustix::termios::tcgetsid(master_fd.as_fd()).is_err(),
        "the terminal must have no session for this test to mean anything"
    );
    assert!(
        sender.child.try_wait().expect("poll the child").is_none(),
        "the child must be alive for this test to mean anything"
    );

    let pane = Pane::adopt(
        master_fd,
        Some(pid),
        AdoptSize { cols: 80, rows: 24 },
        spec(),
    )
    .expect("adopt a live session-less pane");

    assert_eq!(
        pane.try_wait(),
        ExitState::Running,
        "a live child whose terminal has no session is not an exited pane"
    );
    // And it stays running while it works: the pump is reading the child's
    // output through the same master, so the pane is neither dead nor mute.
    wait_for(&pane, "tick-3", Duration::from_secs(10));
    assert_eq!(
        pane.try_wait(),
        ExitState::Running,
        "still running after proving the pane is being read"
    );

    // The pid was never confirmed against this terminal, so it is dropped: the
    // pane holds no number it could signal by mistake, and says so.
    assert!(
        pane.child_pid().is_none(),
        "a pid the terminal could not confirm is not the pane's to report"
    );
    let err = pane
        .kill_shared()
        .expect_err("nothing was confirmed to signal");
    assert!(
        matches!(&err, PtyError::Io(io) if io.kind() == std::io::ErrorKind::Unsupported),
        "got {err:?}"
    );

    // The child really is alive and is not our casualty: the side that forked
    // it still has it.
    assert!(
        sender.child.try_wait().expect("poll the child").is_none(),
        "the adopted pane must not have killed the child it declines to signal"
    );
    drop(sender);
}

/// A refused handoff must leave the pane it refused untouched.
///
/// The old daemon goes on serving this very terminal when the handoff does not
/// commit, so a refusal that had already resized it would damage the pane it
/// declined to adopt — and nothing would put the geometry back. The repair is
/// only correct once the transfer is going to succeed, so it has to run after
/// every check that can still refuse the pid.
///
/// What breaks this test if the repair runs before the pid check (or inside the
/// validation step): the refusal is reported but the sender's terminal has
/// already been resized to the numbers the *rejected* claim carried, and the
/// assertion below sees `(200, 50)` instead of `(0, 0)`.
#[test]
fn a_refused_adoption_leaves_the_geometry_it_declined_to_take() {
    let (sender, master_fd, pid) = Sender::open("/bin/sh", &[]);
    // A terminal nobody has sized yet: the one state the repair exists for, and
    // therefore the one where a stray repair is visible.
    rustix::termios::tcsetwinsize(
        master_fd.as_fd(),
        Winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .expect("clear the terminal size");

    let err = Pane::adopt(
        master_fd,
        Some(pid + 1),
        AdoptSize {
            cols: 200,
            rows: 50,
        },
        spec(),
    )
    .err()
    .expect("a pid the terminal does not confirm");
    assert!(
        matches!(err, PtyError::ChildPidMismatch { .. }),
        "got {err:?}"
    );

    let after = sender.master.get_size().expect("the sender's view");
    assert_eq!(
        (after.cols, after.rows),
        (0, 0),
        "a refused adoption must not have resized the terminal it refused"
    );
    drop(sender);
}

#[test]
fn adopted_pane_carries_the_child_output_and_the_input_that_follows() {
    let (sender, master_fd, pid) = Sender::open(
        "/bin/sh",
        &[
            "-c",
            "echo adopted-ready; while read line; do echo got:$line; done",
        ],
    );
    let pane = Pane::adopt(
        master_fd,
        Some(pid),
        AdoptSize { cols: 80, rows: 24 },
        spec(),
    )
    .expect("adopt the master");

    // Output the child wrote while the other daemon owned the pane reaches the
    // ring through the inherited master and the same reader pump a spawned pane
    // uses.
    wait_for(&pane, "adopted-ready", Duration::from_secs(10));
    assert_eq!(
        pane.child_pid(),
        Some(pid),
        "the pane reports the pid it did not fork"
    );

    // Input through the adopted writer reaches that same child.
    pane.send(b"handoff\r").expect("send into the adopted pane");
    wait_for(&pane, "got:handoff", Duration::from_secs(10));

    drop(sender);
}

#[test]
fn adopted_pane_resize_reaches_the_kernel_and_the_child() {
    let (sender, master_fd, pid) = Sender::open("/bin/sh", &[]);
    let pane = Pane::adopt(
        master_fd,
        Some(pid),
        AdoptSize { cols: 80, rows: 24 },
        spec(),
    )
    .expect("adopt");

    pane.resize(120, 40).expect("resize the adopted pane");
    assert_eq!(pane.size().expect("size"), (120, 40));
    // Not a shadow copy: the daemon that opened the terminal sees the same
    // geometry, because it is the same kernel object.
    let seen_by_sender = sender.master.get_size().expect("the sender's view");
    assert_eq!((seen_by_sender.cols, seen_by_sender.rows), (120, 40));
    // And the child was told: `stty` asks its own terminal.
    pane.send(b"stty size\r").expect("send stty");
    wait_for(&pane, "40 120", Duration::from_secs(10));

    drop(sender);
}

#[test]
fn adopt_keeps_the_geometry_the_kernel_already_has() {
    let (sender, master_fd, pid) = Sender::open("/bin/sh", &[]);
    // The sender claims 200x50; the terminal itself is 80x24. The kernel wins,
    // on both sides: a sender that is stale (or lying) must not be able to
    // resize a live agent's terminal through the adoption.
    let pane = Pane::adopt(
        master_fd,
        Some(pid),
        AdoptSize {
            cols: 200,
            rows: 50,
        },
        spec(),
    )
    .expect("adopt");
    assert_eq!(pane.size().expect("size"), (80, 24));
    let seen_by_sender = sender.master.get_size().expect("the sender's view");
    assert_eq!((seen_by_sender.cols, seen_by_sender.rows), (80, 24));

    drop(sender);
}

/// A geometry with one dimension at 0 is still a geometry: that is what
/// `TIOCSWINSZ` (or `stty rows 0`) leaves on a live terminal, and the rule is
/// that the kernel's terminal wins. Reading it as "unset" would let a stale or
/// lying sender resize a live agent's terminal and `SIGWINCH` its child — with
/// numbers the terminal never agreed to, which is the damage this branch
/// exists to prevent.
///
/// What breaks this test while the predicate is `cols == 0 || rows == 0`: the
/// partially set terminal counts as unconfigured, the sender's 90×30 is applied
/// over it, and both this pane's report and the daemon that still owns the
/// terminal see the overwritten size.
#[test]
fn adopt_keeps_a_partially_set_geometry() {
    let (sender, master_fd, pid) = Sender::open("/bin/sh", &[]);
    rustix::termios::tcsetwinsize(
        master_fd.as_fd(),
        Winsize {
            ws_row: 0,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .expect("set a zero-row geometry");

    let pane = Pane::adopt(
        master_fd,
        Some(pid),
        AdoptSize { cols: 90, rows: 30 },
        spec(),
    )
    .expect("adopt");
    assert_eq!(
        pane.size().expect("size"),
        (80, 0),
        "the sender's numbers must not overwrite a geometry the terminal has"
    );
    let seen_by_sender = sender.master.get_size().expect("the sender's view");
    assert_eq!((seen_by_sender.cols, seen_by_sender.rows), (80, 0));

    drop(sender);
}

#[test]
fn adopt_repairs_a_terminal_whose_geometry_was_never_set() {
    let (sender, master_fd, pid) = Sender::open("/bin/sh", &[]);
    // A terminal is 0x0 until someone sets it — the one case where the sender's
    // numbers are the only information there is.
    rustix::termios::tcsetwinsize(
        master_fd.as_fd(),
        Winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .expect("clear the terminal size");

    let pane = Pane::adopt(
        master_fd,
        Some(pid),
        AdoptSize { cols: 90, rows: 30 },
        spec(),
    )
    .expect("adopt");
    assert_eq!(pane.size().expect("size"), (90, 30));
    let seen_by_sender = sender.master.get_size().expect("the sender's view");
    assert_eq!((seen_by_sender.cols, seen_by_sender.rows), (90, 30));

    drop(sender);
}

#[test]
fn adopted_pane_sees_the_child_exit_without_inventing_a_code() {
    let (mut sender, master_fd, pid) = Sender::open("/bin/sh", &["-c", "exit 7"]);
    let pane = Pane::adopt(
        master_fd,
        Some(pid),
        AdoptSize { cols: 80, rows: 24 },
        spec(),
    )
    .expect("adopt");

    let exit = pane
        .wait_timeout(Duration::from_secs(10))
        .expect("the child exits");
    assert_eq!(
        exit,
        ExitState::Exited(UNKNOWN_EXIT),
        "a status this process cannot reap is unknown, got {exit:?}"
    );

    // The daemon that forked it can still reap it, and the code is 7 — which is
    // exactly why the adopted pane must report neither 7 nor 0.
    assert_eq!(reap(&mut sender, Duration::from_secs(10)).exit_code(), 7);
    assert_ne!(exit, ExitState::Exited(7));
}

/// The pane died while the descriptor was in flight, and the daemon that forked
/// it reaped it before the adopting side looked: the pid is gone outright, which
/// is a different answer than "still running" and must not be one.
///
/// The pid is dropped rather than trusted here — a terminal whose session
/// leader has exited names no session, so nothing confirmed the claim, and a
/// reused number must never become a signal target. The death is *not* inferred
/// from that absence, though: a session-less terminal can belong to a live
/// child (see `adopt_does_not_read_a_session_less_terminal_as_a_dead_pane`), so
/// what ends this pane is the terminal's own end-of-stream, exactly as for a
/// pane handed over with no pid. The bounded wait below is therefore for the
/// pump to observe that, rather than for the adoption to return.
///
/// What breaks this test if end-of-stream never ends the pane: the bounded wait
/// runs out and reports `Running` for a process that is gone. What breaks it if
/// an unconfirmed pid were kept: the `child_pid` and kill assertions below.
#[test]
fn adopting_a_pane_whose_child_died_first_reports_exit() {
    let (mut sender, master_fd, pid) = Sender::open("/bin/sh", &["-c", "exit 5"]);
    assert_eq!(reap(&mut sender, Duration::from_secs(10)).exit_code(), 5);

    let pane = Pane::adopt(
        master_fd,
        Some(pid),
        AdoptSize { cols: 80, rows: 24 },
        spec(),
    )
    .expect("adopt");
    let exit = pane
        .wait_timeout(Duration::from_secs(10))
        .expect("the dead pane reports its death");
    assert_eq!(exit, ExitState::Exited(UNKNOWN_EXIT), "got {exit:?}");

    // The claim was never confirmed against the terminal — it had no session to
    // confirm it with — so the pane holds no pid: `child_pid` is empty and a
    // kill answers `Unsupported` instead of firing at a number that is free to
    // have been reused by an unrelated process. What breaks this if the pid is
    // kept anyway: `child_pid` reports it and the kill below succeeds.
    assert!(
        pane.child_pid().is_none(),
        "an unconfirmed pid is not the pane's to report"
    );
    let err = pane
        .kill_shared()
        .expect_err("nothing was confirmed to signal");
    assert!(
        matches!(&err, PtyError::Io(io) if io.kind() == std::io::ErrorKind::Unsupported),
        "got {err:?}"
    );
    drop(sender);
}

#[test]
fn adopted_pane_kills_the_process_it_did_not_fork() {
    let (mut sender, master_fd, pid) = Sender::open("/bin/sleep", &["60"]);
    let pane = Pane::adopt(
        master_fd,
        Some(pid),
        AdoptSize { cols: 80, rows: 24 },
        spec(),
    )
    .expect("adopt");
    assert_eq!(pane.try_wait(), ExitState::Running, "sleep is alive");

    pane.kill_shared().expect("kill through the shared handle");
    // The signal really reached that process: the side that forked it reaps a
    // signalled status, not a clean exit.
    let status = reap(&mut sender, Duration::from_secs(10));
    assert!(
        status.signal().is_some(),
        "expected a signalled status, got {status:?}"
    );
    assert_eq!(pane.try_wait(), ExitState::Exited(UNKNOWN_EXIT));
    drop(sender);
}

#[test]
fn adopted_pane_without_a_pid_refuses_to_pretend_it_killed_something() {
    let (sender, master_fd, _pid) = Sender::open("/bin/cat", &[]);
    let pane =
        Pane::adopt(master_fd, None, AdoptSize { cols: 80, rows: 24 }, spec()).expect("adopt");

    // No pid was handed over, so there is nothing to signal — an error is the
    // truthful answer; `Ok(())` would tell the daemon it had killed the agent.
    let err = pane.kill_shared().expect_err("nothing to signal");
    assert!(
        matches!(&err, PtyError::Io(io) if io.kind() == std::io::ErrorKind::Unsupported),
        "got {err:?}"
    );
    drop(sender);
}

#[test]
fn adopted_pane_without_a_pid_learns_exit_from_end_of_stream() {
    let (sender, master_fd, _pid) = Sender::open("/bin/sh", &["-c", "sleep 0.3; exit 3"]);
    let pane =
        Pane::adopt(master_fd, None, AdoptSize { cols: 80, rows: 24 }, spec()).expect("adopt");

    assert!(pane.child_pid().is_none(), "no pid was handed over");
    // End-of-stream is not a guess made at construction time: while the child
    // lives, the pane says so.
    assert_eq!(pane.try_wait(), ExitState::Running);
    let exit = pane
        .wait_timeout(Duration::from_secs(10))
        .expect("the child exits");
    assert_eq!(exit, ExitState::Exited(UNKNOWN_EXIT), "got {exit:?}");

    drop(sender);
}
