//! PTY manager core (T-0002): spawn, read, write, resize, kill per pane.
//!
//! One blocking reader thread per pane pumps the portable-pty master into a
//! bounded [`RingBuffer`]; writers go straight to the master. No unbounded
//! queues anywhere: when the buffer is full the oldest lines are dropped and
//! [`RingBuffer::dropped`] counts them, so a runaway child can never OOM us.

use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::io::{AsFd, BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
#[cfg(unix)]
use rustix::event::{poll, PollFd, PollFlags};
use thiserror::Error;

/// Adopting a PTY master that arrived as a file descriptor (T-0038), plus the
/// `SCM_RIGHTS` helpers the handoff passes it with. Unix-only: descriptor
/// passing has no Windows analogue (T-0039 routes that case).
#[cfg(unix)]
pub mod adopt;

/// Hot scrollback capacity: 512 lines (ROADMAP §3.1 memory discipline).
pub const HOT_LINES: usize = 512;

/// Upper bound on bytes kept per line. A single giant write (e.g. a 100 MB
/// JSON blob with no newlines) is truncated to this, so one line can never
/// blow the ≤ 3 MB per-pane budget on its own. Oversized lines increment
/// [`RingBuffer::dropped_bytes`].
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// Default PTY dimensions.
pub const DEFAULT_COLS: u16 = 80;
pub const DEFAULT_ROWS: u16 = 24;

#[derive(Debug, Error)]
pub enum PtyError {
    #[error("pty backend: {0}")]
    Backend(#[from] anyhow::Error),
    #[error("pty io: {0}")]
    Io(#[from] std::io::Error),
    #[error("pane is closed")]
    Closed,
    /// [`Pane::adopt`] was handed a descriptor that is not a terminal at all
    /// (a socket, a file, an eventfd). Refused, and the descriptor closed:
    /// adopting it would put an arbitrary descriptor behind the pane API.
    #[error("adopt: descriptor is not a terminal")]
    NotATerminal,
    /// [`Pane::adopt`] was handed a terminal that is not a PTY *master* — the
    /// slave end, most plausibly. A slave is a tty, so `isatty` accepts it;
    /// `ptsname` does not, and adopting the wrong end would leave the pane
    /// reading its own output instead of the child's.
    #[error("adopt: descriptor is not a pty master")]
    NotPtyMaster,
    /// [`Pane::adopt`] was handed a `child_pid` the terminal disagrees with:
    /// the master's session leader — the process the sending daemon created,
    /// and the one a kill would reach — is `session`, not `claimed`.
    ///
    /// The pid arrives over the same untrusted channel as the descriptor, so it
    /// is a number an attacker picks until the terminal confirms it. A pid the
    /// terminal names *no* session for is not refused this way: it is dropped,
    /// so it can never become a signal target, without giving up a pane whose
    /// agent has already exited (see `AdoptedMaster::claimed_pid`). Here the
    /// terminal contradicted the claim outright.
    #[error("adopt: claimed child pid {claimed}, but the terminal's session leader is {session}")]
    ChildPidMismatch { claimed: u32, session: u32 },
    /// The reader pump did not reach the handoff's quiescence point within the
    /// bound (T-0038 stage 2). A failed handoff, never a slower one: the
    /// outgoing daemon resumes every paused pump and keeps serving, and the
    /// update is retried rather than taken with bytes in flight.
    #[error("the reader pump did not pause within {0:?}")]
    PauseTimeout(Duration),
    /// This pane's master has no descriptor to hand over ([`Pane::with_master_fd`]).
    /// Reported rather than guessed: a handoff that dropped this pane's
    /// descriptor would leave the agent with the daemon that exits.
    #[error("the pane's master has no descriptor to transfer")]
    NoMasterFd,
}

/// Bounded line-oriented ring buffer.
///
/// Invariants: `lines.len() <= capacity` always; `dropped` counts evicted
/// lines; `dropped_bytes` counts bytes discarded by line truncation.
#[derive(Debug)]
pub struct RingBuffer {
    lines: std::collections::VecDeque<String>,
    capacity: usize,
    pending: String,
    dropped: u64,
    dropped_bytes: u64,
    /// Raw byte journal: every byte ever pushed (capped). Powers byte-exact
    /// fixture recording (T-0011) without a second PTY read path.
    raw: Vec<u8>,
    raw_truncated: bool,
}

/// Cap for the raw journal (1 MiB — fixtures must stay small; larger
/// sessions record truncated with `raw_truncated` set).
pub const MAX_RAW_JOURNAL: usize = 1024 * 1024;

impl RingBuffer {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            lines: std::collections::VecDeque::with_capacity(capacity.min(1024)),
            capacity,
            pending: String::new(),
            dropped: 0,
            dropped_bytes: 0,
            raw: Vec::new(),
            raw_truncated: false,
        }
    }

    /// Feed raw bytes; splits on `\n` (tolerates `\r\n`). Partial lines stay
    /// in `pending` until terminated. Also appends to the raw journal.
    pub fn push_bytes(&mut self, bytes: &[u8]) {
        if !self.raw_truncated {
            let room = MAX_RAW_JOURNAL.saturating_sub(self.raw.len());
            if bytes.len() <= room {
                self.raw.extend_from_slice(bytes);
            } else {
                self.raw.extend_from_slice(&bytes[..room]);
                self.raw_truncated = true;
            }
        }
        let text = String::from_utf8_lossy(bytes);
        self.pending.push_str(&text);
        while let Some(pos) = self.pending.find('\n') {
            let mut line: String = self.pending.drain(..=pos).collect();
            while line.ends_with('\n') || line.ends_with('\r') {
                line.pop();
            }
            self.push_line(line);
        }
        // A pending partial line that never terminates must also be bounded.
        if self.pending.len() > MAX_LINE_BYTES {
            self.dropped_bytes += (self.pending.len() - MAX_LINE_BYTES) as u64;
            self.pending.truncate(MAX_LINE_BYTES);
        }
    }

    fn push_line(&mut self, mut line: String) {
        if line.len() > MAX_LINE_BYTES {
            self.dropped_bytes += (line.len() - MAX_LINE_BYTES) as u64;
            line.truncate(MAX_LINE_BYTES);
        }
        if self.lines.len() >= self.capacity {
            self.lines.pop_front();
            self.dropped += 1;
        }
        self.lines.push_back(line);
    }

    /// Pre-seed restored history (T-0018): push lines directly into the ring
    /// WITHOUT touching the PTY. This is the ONLY safe restore mechanism —
    /// typing history as keystrokes would EXECUTE metachar lines in the fresh
    /// shell (proven hazard, T-0018 adversarial pass). Restored lines read
    /// back through `drain()` ahead of live output, byte-identical.
    pub fn prepend_history(&mut self, history: &[String]) {
        // Oldest first, through the same bounded path (capacity respected,
        // oldest restored lines evict first if history exceeds capacity).
        for line in history {
            self.push_line(line.clone());
        }
    }

    /// Flush an unterminated trailing partial line (e.g. a shell prompt).
    pub fn flush_partial(&mut self) {
        if !self.pending.is_empty() {
            let mut line: String = std::mem::take(&mut self.pending)
                .chars()
                .take(MAX_LINE_BYTES)
                .collect();
            while line.ends_with('\r') {
                line.pop();
            }
            self.push_line(line);
        }
    }

    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.lines.iter().cloned().collect()
    }

    /// The unterminated trailing partial line, **without** flushing it.
    ///
    /// Distinct from [`Self::flush_partial`] on purpose: the handoff's snapshot
    /// must not mutate the ring it is reading (T-0038 stage 2) — a flush would
    /// move the partial into `lines` on the pane that goes on serving if the
    /// handoff does not commit, and the transfer would then carry a line the
    /// sender's own ring no longer has as a partial.
    #[must_use]
    pub fn pending_line(&self) -> &str {
        &self.pending
    }

    /// Seed this ring from another pane's captured scrollback (T-0038 stage 2).
    ///
    /// The inverse of [`Scrollback`] + [`Self::lines`]/[`Self::pending_line`]:
    /// `lines` go through the same bounded path [`Self::prepend_history`] uses
    /// (so a hostile or simply larger sender cannot exceed this ring's
    /// capacity), and the counters are written *after*, so the values that
    /// travelled are the values reported — seeding must not invent evictions
    /// the sender never had.
    pub fn seed(&mut self, scrollback: &Scrollback) {
        self.prepend_history(&scrollback.lines);
        let mut end = scrollback.pending.len().min(MAX_LINE_BYTES);
        while !scrollback.pending.is_char_boundary(end) {
            end -= 1;
        }
        if end < scrollback.pending.len() {
            self.dropped_bytes += (scrollback.pending.len() - end) as u64;
        }
        self.pending = scrollback.pending[..end].to_string();
        self.raw.clone_from(&scrollback.raw);
        self.raw_truncated = scrollback.raw_truncated;
        self.dropped = scrollback.dropped;
        self.dropped_bytes = scrollback.dropped_bytes;
        if self.raw.len() > MAX_RAW_JOURNAL {
            self.raw.truncate(MAX_RAW_JOURNAL);
            self.raw_truncated = true;
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    #[must_use]
    pub fn dropped_bytes(&self) -> u64 {
        self.dropped_bytes
    }

    /// Byte-exact journal of everything pushed (capped at `MAX_RAW_JOURNAL`).
    #[must_use]
    pub fn raw_bytes(&self) -> Vec<u8> {
        self.raw.clone()
    }

    #[must_use]
    pub fn raw_truncated(&self) -> bool {
        self.raw_truncated
    }

    /// Rough heap footprint of buffered text + raw journal (for the ≤ 3 MB
    /// budget test). The journal is capped at 1 MiB, so worst case is still
    /// inside the per-pane budget alongside a full 512-line hot buffer.
    #[must_use]
    pub fn bytes_held(&self) -> usize {
        self.lines.iter().map(String::len).sum::<usize>() + self.pending.len() + self.raw.len()
    }
}

/// A pane's hot scrollback and raw journal, captured without mutating the ring
/// (T-0038 stage 2).
///
/// The whole of what a pane's memory holds that is *not* in the kernel: the
/// bytes nobody has read are still in the terminal's buffer and cross the cut on
/// their own, but the lines already read live only here. [`Pane::scrollback`]
/// takes one, [`Pane::adopt_seeded`] seeds a ring from one.
///
/// Everything in it travels: `dropped`/`dropped_bytes` are **not** derivable
/// from `lines` (they count what the bounded ring already evicted and
/// truncated), so resetting them at the cut would make the new daemon report
/// "0 dropped" about a pane whose history was evicted — a lie in the direction
/// of looking healthy. The raw journal is not derivable either, and it is the
/// state engine's *input* (`PaneEntry::pump` feeds it), so without it a pane
/// that was `Question` becomes `Unknown` at the cut.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scrollback {
    pub lines: Vec<String>,
    pub pending: String,
    pub raw: Vec<u8>,
    pub raw_truncated: bool,
    pub dropped: u64,
    pub dropped_bytes: u64,
}

/// The handoff's pause gate: the reader pump's quiescence point (T-0038 stage 2).
///
/// **Why a gate at all.** A snapshot of the ring is only meaningful if no byte
/// is in flight. The pump does `read()` and then `buffer.lock().push_bytes()`,
/// and the lock covers only the push — so holding the buffer lock is *not* a
/// quiescence point, and the bytes between a completed `read` and its `push` are
/// in neither the kernel's pipe nor the ring. The window is closed by making the
/// pump itself acknowledge: it checks the gate at the top of every iteration,
/// and a pause is acknowledged there — i.e. only between a fully completed
/// read+push and the next read.
///
/// **Why the wake pipe.** A pump blocked in `read()` on an idle pane would never
/// reach the gate, and a snapshot would have to wait for output that may not
/// come. So the pump waits in `poll` over {master, wake} instead of blocking in
/// `read`, and a pause writes one byte to the wake end: the pump comes out of
/// `poll`, sees the flag, acknowledges, and parks on the condvar until resumed.
/// Bytes that arrive while it is parked stay in the kernel's terminal buffer and
/// are read by whoever owns the pane after the cut — which is why the pause
/// loses nothing.
///
/// A pump that has **ended** (end-of-stream) is quiescent for ever and says so
/// ([`PauseGate::finished`]): a dead pane has no bytes in flight, and waiting
/// for an acknowledgement from a thread that has already returned would turn
/// every handoff that includes a dead pane into a timeout.
#[derive(Debug)]
struct PauseGate {
    state: Mutex<GateState>,
    /// Wakes the pump out of `poll`. One wait-set, not a sleep-poll loop.
    ack: Condvar,
    /// The pump's end of the wake channel, handed to the pump when its thread
    /// starts (a handoff pause is the only writer).
    #[cfg(unix)]
    wake_reader: Mutex<WakeReader>,
    /// The end a pause writes one byte to.
    #[cfg(unix)]
    wake_writer: WakeWriter,
}

#[derive(Debug, Default)]
struct GateState {
    /// A handoff has asked the pump to stop.
    pause_requested: bool,
    /// No byte can be read or pushed right now: the pump has arrived at the gate
    /// and observed the pause, or has ended for good.
    quiesced: bool,
    /// The pump has ended. Sticky: a finished pump never re-acknowledges, so a
    /// resume must not clear `quiesced` for it.
    finished: bool,
}

/// The pump's end of the handoff's wake channel (see [`PauseGate`]).
#[cfg(unix)]
type WakeReader = Option<OwnedFd>;
/// Nothing where the platform has no descriptor to poll on: the pause is then
/// observed only between reads (see [`Pane::pause`]).
#[cfg(not(unix))]
type WakeReader = ();
/// The end of the wake channel a pause writes to (see [`WakeReader`]).
#[cfg(unix)]
type WakeWriter = Option<OwnedFd>;
#[cfg(not(unix))]
type WakeWriter = ();

impl PauseGate {
    fn new() -> Self {
        #[cfg(unix)]
        {
            let (wake_reader, wake_writer) = wake_channel();
            Self {
                state: Mutex::new(GateState::default()),
                ack: Condvar::new(),
                wake_reader: Mutex::new(wake_reader),
                wake_writer,
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                state: Mutex::new(GateState::default()),
                ack: Condvar::new(),
            }
        }
    }

    /// The pump's end, taken once by the pump thread.
    fn take_reader(&self) -> WakeReader {
        #[cfg(unix)]
        {
            self.wake_reader
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take()
        }
        #[cfg(not(unix))]
        {
            ()
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The pump's arrival at the gate: called at the top of every iteration,
    /// before the read it guards. Blocks while a pause is in effect.
    fn arrive(&self) {
        let mut state = self.lock();
        if !state.pause_requested {
            return;
        }
        state.quiesced = true;
        self.ack.notify_all();
        while state.pause_requested {
            state = self.ack.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// The pump's last act: it will never read again, so it is quiescent for
    /// ever and never needs waking.
    fn finished(&self) {
        let mut state = self.lock();
        state.finished = true;
        state.quiesced = true;
        self.ack.notify_all();
    }

    /// Ask the pump to stop *and wait for it to say it has*. `Ok(())` means the
    /// read→push window is closed: every byte the child has written is either in
    /// the ring/journal or still in the terminal's buffer.
    ///
    /// Bounded by `timeout`: a pump that does not answer costs a failed handoff,
    /// never a snapshot with bytes in flight.
    fn pause(&self, timeout: Duration) -> Result<(), PtyError> {
        let mut state = self.lock();
        if state.quiesced {
            // Already parked at the gate, or ended. A second pause inside one
            // handoff is not a new request.
            return Ok(());
        }
        state.pause_requested = true;
        drop(state);
        #[cfg(unix)]
        if let Some(wake) = self.wake_writer.as_ref() {
            // Best effort: the flag above is what stops a busy pump, and this
            // byte is what brings a parked one out of `poll`. A full channel
            // means a wake byte is already queued, which is just as good.
            let _ = rustix::io::write(wake, &[0u8]);
        }
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        while !state.quiesced {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return Err(PtyError::PauseTimeout(timeout));
            };
            let (guard, _) = self
                .ack
                .wait_timeout(state, remaining)
                .unwrap_or_else(|e| e.into_inner());
            state = guard;
        }
        Ok(())
    }

    /// Let a paused pump run again. Safe on a pump that has ended (it stays
    /// quiescent) and on one that was never paused.
    fn resume(&self) {
        let mut state = self.lock();
        state.pause_requested = false;
        if !state.finished {
            state.quiesced = false;
        }
        self.ack.notify_all();
    }
}

/// Create the wake channel: a `socketpair`, because what the pump needs is a
/// descriptor it can `poll` beside the master, and both ends must be
/// close-on-exec (this process goes on to spawn the next generation's panes).
///
/// `(None, None)` when the platform cannot make one: the pause then works by
/// the flag alone, which a busy pump observes between reads and a parked one
/// does not — [`Pane::pause`] is bounded precisely so that case is a failed
/// handoff rather than a hang.
#[cfg(unix)]
fn wake_channel() -> (WakeReader, WakeWriter) {
    match rustix::net::socketpair(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::STREAM,
        rustix::net::SocketFlags::NONBLOCK | rustix::net::SocketFlags::CLOEXEC,
        None,
    ) {
        Ok((reader, writer)) => (Some(reader), Some(writer)),
        Err(_) => (None, None),
    }
}

#[cfg(not(unix))]
fn wake_channel() -> (WakeReader, WakeWriter) {
    ((), ())
}

/// A `dup` of a master's descriptor, close-on-exec, for the pump to wait on.
///
/// # Soundness
///
/// `raw` came from `MasterPty::as_raw_fd` on a master this pane owns — it is
/// inside the `Mutex` this call borrows it through — so it is an open descriptor
/// for the duration of the call, and `fcntl_dupfd_cloexec` only reads it. What
/// comes back is an `OwnedFd` of this process's own, which is what keeps the
/// pump's wait-set valid even if the master is dropped while the pump runs: a
/// bare fd *number* would be recycled by the next `open` in this process.
#[cfg(unix)]
fn dup_master_fd(raw: std::os::unix::io::RawFd) -> Option<OwnedFd> {
    let borrowed = unsafe { BorrowedFd::borrow_raw(raw) };
    rustix::io::fcntl_dupfd_cloexec(borrowed, 0).ok()
}

/// What the pump's wait found.
#[cfg(unix)]
enum Ready {
    /// The master has something to read (`read` is next).
    Master,
    /// The handoff's wake byte arrived: go back to the gate.
    Wake,
}

/// Wait until the master is readable or the wake channel fires.
///
/// `wake: None` waits on the master alone. A `poll` failure falls back to the
/// pre-gate behaviour — attempt the read — because inventing an end-of-stream
/// from a transient error would report a live agent as exited.
#[cfg(unix)]
fn wait_for_master(master: BorrowedFd<'_>, wake: Option<&OwnedFd>) -> Ready {
    let mut fds = [
        PollFd::new(&master, PollFlags::IN),
        PollFd::new(&master, PollFlags::IN),
    ];
    let watched = match wake {
        Some(wake) => {
            fds[1] = PollFd::new(wake, PollFlags::IN);
            2
        }
        None => 1,
    };
    match poll(&mut fds[..watched], -1) {
        // A negative timeout cannot return 0; looping keeps the compiler's
        // exhaustiveness honest without pretending the case is reachable.
        Ok(0) => Ready::Master,
        Ok(_) => {
            // The wake end is checked first: a pause must never be mistaken for
            // output, and the byte must be drained before the next wait.
            if watched == 2 && !fds[1].revents().is_empty() {
                Ready::Wake
            } else {
                Ready::Master
            }
        }
        Err(rustix::io::Errno::INTR) => Ready::Master,
        Err(_) => Ready::Master,
    }
}

/// Drain the wake channel. Returns whether the pump's end is still usable: a
/// closed peer (only reachable if the gate is dropped, which the pump's own
/// `Arc` prevents) would otherwise read as permanently readable and spin.
#[cfg(unix)]
fn drain_wake(wake: Option<&OwnedFd>) -> bool {
    let Some(wake) = wake else { return false };
    let mut buf = [0u8; 64];
    loop {
        match rustix::io::read(wake, &mut buf) {
            Ok(0) => return false,
            Ok(_) => continue,
            Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => return true,
            Err(_) => return false,
        }
    }
}

/// What `try_wait` observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitState {
    Running,
    Exited(u32),
}

/// Exit code reported for a pane whose process this daemon did not fork
/// ([`Pane::adopt`]): the process is known to be gone, but `waitpid` can never
/// be called on it, so the status it exited with died with the daemon that
/// forked it.
///
/// `u32::MAX` is the sentinel, and it cannot be confused with a real status: a
/// process exit code is `0..=255`. It is a constant rather than a third
/// [`ExitState`] variant because a new variant would break every exhaustive
/// `match` on it downstream (`arreo-server`'s daemon has one), and widening the
/// enum is not this task's business.
pub const UNKNOWN_EXIT: u32 = u32::MAX;

/// A spawned pane: owned PTY master, reader pump, bounded buffer, child handle.
///
/// `Send + Sync`: the daemon shares panes across threads/tasks (`Arc<Pane>`),
/// so all handle access goes through interior mutability. `portable-pty`
/// handles are `Send` but not `Sync`; the `Mutex`es below bridge that gap.
/// Contention is trivial (short syscalls), never held across blocking waits.
pub struct Pane {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    buffer: Arc<Mutex<RingBuffer>>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    killer: Mutex<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
    closed: Arc<std::sync::atomic::AtomicBool>,
    /// Spawn record for `split` (daemon re-spawns the same program).
    spawn: SpawnSpec,
    /// The handoff's quiescence gate (T-0038 stage 2): shared with the pump.
    gate: Arc<PauseGate>,
}

/// How a pane was spawned (remembered for `split`).
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
}

/// Everything an adopted pane arrives with (T-0038 stage 2).
///
/// Named fields rather than a parameter list: [`Pane::adopt`] grew from four
/// arguments to six, three of them `Vec`/`String` — and a positional call site
/// is exactly where a scrollback ends up in the pid slot.
#[cfg(unix)]
#[derive(Debug, Clone)]
pub struct AdoptSeed {
    /// The pid the sender believes it forked; checked against the terminal
    /// before it is used (see [`Pane::adopt_seeded`]).
    pub child_pid: Option<u32>,
    /// The sender's geometry — used only when the inherited master has none.
    pub size: adopt::AdoptSize,
    /// The program and args, for `split` and for the audit trail.
    pub spec: SpawnSpec,
    /// The lines, partial line and raw journal the sender had already read.
    pub scrollback: Scrollback,
}

impl Pane {
    /// Spawn `program` with `args` in a fresh PTY of `cols`×`rows`.
    pub fn spawn(program: &str, args: &[&str], cols: u16, rows: u16) -> Result<Self, PtyError> {
        let pty_system = native_pty_system();
        let pair = pty_system.openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        let child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        Self::assemble(
            pair.master,
            child,
            Arc::new(AtomicBool::new(false)),
            SpawnSpec {
                program: program.to_string(),
                args: args.iter().map(|s| s.to_string()).collect(),
            },
            Scrollback::default(),
            false,
        )
    }

    /// Adopt a PTY master that was opened by another process and handed over as
    /// a file descriptor (T-0038: the zero-cut update).
    ///
    /// This is [`Pane::adopt_seeded`] with an empty scrollback and a pump that
    /// starts reading at once — the stage-0/1 shape, unchanged for its callers.
    #[cfg(unix)]
    pub fn adopt(
        master_fd: OwnedFd,
        child_pid: Option<u32>,
        size: adopt::AdoptSize,
        spec: SpawnSpec,
    ) -> Result<Self, PtyError> {
        Self::adopt_inner(
            master_fd,
            AdoptSeed {
                child_pid,
                size,
                spec,
                scrollback: Scrollback::default(),
            },
            // Never parked: this entry point has no cut to protect, so the pane
            // reads from the moment it exists.
            false,
        )
    }

    /// Adopt a pane **mid-flight** (T-0038 stage 2): the master descriptor plus
    /// the scrollback and journal the sender had already read.
    ///
    /// Everything [`Pane::adopt`] documents about the descriptor and the pid
    /// holds here unchanged (the same checks, in the same order, with the same
    /// refusals). Two things are added, and they are the whole of stage 2's
    /// adoption:
    ///
    /// - `seed.scrollback` is pushed into the fresh ring before the pump starts,
    ///   so the lines and the raw journal the sender had already read are this
    ///   pane's — see [`Scrollback`] for why the journal has to travel (it is
    ///   the state engine's input) and why the counters are not derivable.
    /// - `seed.paused` parks the pump **before its first read**. The incoming
    ///   daemon must not consume a byte until the cut is committed: if it read
    ///   and then aborted, those bytes would be gone from the terminal's buffer
    ///   and the outgoing daemon — which resumes and goes on serving — would
    ///   have a hole in its scrollback that it cannot recover. Bytes written in
    ///   the meantime stay in the terminal's buffer, which is what makes this
    ///   safe. [`Pane::resume`] starts the pump.
    ///
    /// **The pump is parked, always, and there is no way to ask otherwise.** The
    /// incoming daemon must not consume a byte from an inherited terminal before
    /// the cut commits: a byte read by a daemon that then aborts is gone, and the
    /// outgoing daemon resumes with a hole in the agent's output that nothing can
    /// detect afterwards. That rule is enforced here rather than left to the
    /// caller's literal, because the window it protects is milliseconds wide with
    /// a real peer — **a test cannot race it** (one was written and could not
    /// catch the mutant that passed `false`; it is recorded in
    /// `crates/arreo-server/tests/handoff.rs`'s history as the reason this is
    /// structural). [`Pane::resume`] starts the pump once the cut is committed.
    #[cfg(unix)]
    pub fn adopt_seeded(master_fd: OwnedFd, seed: AdoptSeed) -> Result<Self, PtyError> {
        Self::adopt_inner(master_fd, seed, true)
    }

    /// Adopt with an explicit parking decision — private, because the decision is
    /// not a caller's to make: [`Pane::adopt_seeded`] always parks (a handoff) and
    /// [`Pane::adopt`] never does (the plain primitive).
    #[cfg(unix)]
    fn adopt_inner(master_fd: OwnedFd, seed: AdoptSeed, paused: bool) -> Result<Self, PtyError> {
        let AdoptSeed {
            child_pid,
            size,
            spec,
            scrollback,
        } = seed;
        // Shared with the adopted child before the pane exists: the pump sets
        // it at end-of-stream, which is that child's fallback exit signal.
        let closed = Arc::new(AtomicBool::new(false));
        let master = adopt::AdoptedMaster::new(master_fd)?;
        // Before the pid reaches anything that can signal: the descriptor
        // arrived on an untrusted channel, and so did the pid that came with
        // it. Refusing here drops `master` and with it the descriptor, having
        // touched nothing else.
        let pid = match child_pid {
            None => None,
            Some(pid) => match master.claimed_pid(pid)? {
                adopt::ClaimedPid::SessionLeader => Some(pid),
                // The terminal names no session, so it cannot confirm the
                // claim: the pid is dropped, and *nothing* is concluded about
                // the child. A session-less terminal can belong to a child
                // that is running right now, so the pane is watched by
                // end-of-stream — the no-pid path — and the child's actual
                // death is what ends up being reported.
                adopt::ClaimedPid::NoSession => None,
            },
        };
        // Every check that can still refuse has run, and so has every step that
        // can fail on a resource (the child's terminal `dup`): only now is the
        // inherited terminal changed, and nothing that a handoff can trigger
        // changes it again. What remains below is the final assembly, whose
        // failures are resource exhaustion (a thread that will not start) —
        // not a refusal of the transfer, and not reachable by anything the
        // sender controls.
        let child = adopt::AdoptedChild::new(pid, &master, Arc::clone(&closed))?;
        // The geometry repair is *not* run here for a parked (handoff) pane:
        // it mutates the inherited terminal — `TIOCSWINSZ`, a `SIGWINCH` to the
        // child — and the handoff's rule is that nothing mutates a terminal it
        // may yet give back before the cut is committed (F6, stage-2 review:
        // on an abort the outgoing daemon would keep serving a resized
        // terminal). The incoming daemon runs [`Pane::repair_geometry`] after
        // the commit, with the guards and the pump start. The unparked path
        // ([`Pane::adopt`]) keeps the repair here, exactly where the refusal
        // checks end.
        if !paused {
            master.repair_geometry(size)?;
        }
        Self::assemble(
            Box::new(master),
            Box::new(child),
            closed,
            spec,
            scrollback,
            paused,
        )
    }

    /// Assemble a pane around an already-open master and child handle.
    ///
    /// The single place that installs the reader pump, the bounded ring, the
    /// writer, the `closed` flag and the handoff's pause gate:
    /// [`Pane::spawn`] and [`Pane::adopt_seeded`] differ only in where `master`
    /// and `child` come from and in what the ring is seeded with. `closed` is
    /// passed in because it is shared, not private here — an adopted child
    /// reads it to learn that the terminal reached end-of-stream.
    ///
    /// `scrollback` seeds the ring before the pump starts (empty for a fresh
    /// spawn), and `paused` parks the pump before its first read (the incoming
    /// daemon must not consume a byte before the cut commits — see
    /// [`Pane::adopt_seeded`]).
    fn assemble(
        master: Box<dyn MasterPty + Send>,
        child: Box<dyn portable_pty::Child + Send + Sync>,
        closed: Arc<AtomicBool>,
        spec: SpawnSpec,
        scrollback: Scrollback,
        paused: bool,
    ) -> Result<Self, PtyError> {
        let gate = Arc::new(PauseGate::new());
        if paused {
            // Set before the pump exists, so its first act is to observe it:
            // the `arrive` check below is the pump's first statement, and the
            // gate is already asking it to stop.
            gate.lock().pause_requested = true;
        }
        let mut ring = RingBuffer::new(HOT_LINES);
        ring.seed(&scrollback);
        let buffer = Arc::new(Mutex::new(ring));
        let mut reader = master.try_clone_reader()?;
        // The descriptor the pump waits on instead of blocking in `read`
        // (see `PauseGate`), and the wake channel it waits beside it.
        #[cfg(unix)]
        let poll_master = master.as_raw_fd().and_then(dup_master_fd);
        let pump_buffer = Arc::clone(&buffer);
        let pump_closed = Arc::clone(&closed);
        let pump_gate = Arc::clone(&gate);
        std::thread::Builder::new()
            .name("arreo-pty-reader".to_string())
            .spawn(move || {
                #[cfg(unix)]
                let mut wake: WakeReader = pump_gate.take_reader();
                let mut chunk = [0u8; 8192];
                loop {
                    // The handoff's quiescence point: acknowledged here and
                    // nowhere else, so a pause means "no byte is in flight" —
                    // not "the buffer lock happens to be free".
                    pump_gate.arrive();
                    #[cfg(unix)]
                    if let Some(poll_master) = poll_master.as_ref() {
                        // Wait for output rather than blocking in `read`, so a
                        // pause can interrupt a pump parked on an idle pane.
                        match wait_for_master(poll_master.as_fd(), wake.as_ref()) {
                            Ready::Master => {}
                            Ready::Wake => {
                                if !drain_wake(wake.as_ref()) {
                                    // The wake end is gone: wait on the master
                                    // alone from here (and never spin on a
                                    // permanently readable dead descriptor).
                                    wake = None;
                                }
                                continue;
                            }
                        }
                    }
                    // A master read reports end-of-stream as `Ok(0)` or, on
                    // Linux, as `EIO` once the last slave closes: both end the
                    // pump, and `closed` is what callers see.
                    match reader.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Ok(mut buf) = pump_buffer.lock() {
                                buf.push_bytes(&chunk[..n]);
                            }
                        }
                    }
                }
                pump_closed.store(true, Ordering::SeqCst);
                // A pump that has ended is quiescent for ever: it will never
                // read again, so a handoff that pauses this pane (a dead pane
                // crosses the cut like any other) must not wait for an
                // acknowledgement from a thread that has returned.
                pump_gate.finished();
            })
            .map_err(PtyError::Io)?;

        let killer = Mutex::new(child.clone_killer());
        let master = Mutex::new(master);
        let writer = Mutex::new(master.lock().map_err(|_| PtyError::Closed)?.take_writer()?);
        Ok(Self {
            master,
            writer,
            buffer,
            child: Arc::new(Mutex::new(child)),
            killer,
            closed,
            spawn: spec,
            gate,
        })
    }

    /// How this pane was spawned (for `split`).
    #[must_use]
    pub fn spawn_spec(&self) -> SpawnSpec {
        self.spawn.clone()
    }

    /// Write input bytes to the child (e.g. `b"ls\r"`).
    pub fn send(&self, input: &[u8]) -> Result<(), PtyError> {
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(PtyError::Closed);
        }
        let mut writer = self.writer.lock().map_err(|_| PtyError::Closed)?;
        writer.write_all(input)?;
        writer.flush()?;
        Ok(())
    }

    /// Propagate a resize to the kernel + child (SIGWINCH on Unix).
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), PtyError> {
        self.master
            .lock()
            .map_err(|_| PtyError::Closed)?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
        Ok(())
    }

    /// Current kernel-known PTY size.
    pub fn size(&self) -> Result<(u16, u16), PtyError> {
        let size = self
            .master
            .lock()
            .map_err(|_| PtyError::Closed)?
            .get_size()?;
        Ok((size.cols, size.rows))
    }

    /// Repair a terminal whose geometry was never set, to `cols`×`rows`.
    ///
    /// The handoff's post-commit half of [`adopt::AdoptedMaster::repair_geometry`]
    /// (F6, stage-2 review): [`Pane::adopt_seeded`] no longer sizes the
    /// inherited terminal — the incoming daemon must not mutate a terminal it
    /// may yet give back, or an abort would leave the outgoing daemon serving a
    /// resized one — so the repair moves to after the cut, run by the daemon
    /// that now owns the pane, alongside the guards and the pump start. The
    /// rule is the primitive's: only a 0×0 terminal (both dimensions) is the
    /// sender's numbers to set; a live geometry wins, because a stale or lying
    /// sender must not be able to resize an agent's terminal.
    pub fn repair_geometry(&self, cols: u16, rows: u16) -> Result<(), PtyError> {
        let master = self.master.lock().map_err(|_| PtyError::Closed)?;
        let unset = master
            .get_size()
            .map(|current| current.cols == 0 && current.rows == 0)
            .unwrap_or(true);
        if unset {
            master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
        }
        Ok(())
    }

    /// Snapshot of buffered lines (oldest first), flushing any partial line.
    #[must_use]
    pub fn drain(&self) -> Vec<String> {
        if let Ok(mut buf) = self.buffer.lock() {
            buf.flush_partial();
            buf.lines()
        } else {
            Vec::new()
        }
    }

    /// Capture this pane's scrollback and raw journal **without mutating the
    /// ring** (T-0038 stage 2).
    ///
    /// Unlike [`Pane::drain`] this does not flush the partial line: the sender
    /// goes on serving if the handoff does not commit, and a snapshot that
    /// moved its partial into `lines` would have changed the pane it was
    /// describing. The partial travels as a partial ([`Scrollback::pending`])
    /// and is re-seeded as one, so the lines read back afterwards — on either
    /// side of the cut — are byte-identical either way.
    ///
    /// Call it only while the pump is paused ([`Pane::pause`]): that is what
    /// makes the snapshot complete rather than merely consistent, because
    /// otherwise a byte in the read→push window is in neither the ring nor the
    /// terminal's buffer.
    #[must_use]
    pub fn scrollback(&self) -> Scrollback {
        match self.buffer.lock() {
            Ok(buf) => Scrollback {
                lines: buf.lines(),
                pending: buf.pending_line().to_string(),
                raw: buf.raw_bytes(),
                raw_truncated: buf.raw_truncated(),
                dropped: buf.dropped(),
                dropped_bytes: buf.dropped_bytes(),
            },
            Err(_) => Scrollback::default(),
        }
    }

    /// Stop this pane's reader pump and wait for it to acknowledge (T-0038
    /// stage 2): on `Ok(())` no byte is in flight, so a snapshot taken now is
    /// complete.
    ///
    /// **What quiescence means here**: every byte the child has already written
    /// is in the ring/journal, and every byte it writes from now on is still in
    /// the terminal's buffer, to be read by whoever owns the pane after the cut.
    /// The pump checks the gate before each read, so the acknowledgement can
    /// only happen between a completed read+push and the next read — the one
    /// point where that statement is true.
    ///
    /// **Bounded, and the bound is a failed handoff.** `timeout` is the whole
    /// wait for the pump's acknowledgement; a pump that does not answer — a
    /// platform with no wake channel whose pump is parked on an idle pane, or a
    /// thread that will not be scheduled — returns
    /// [`PtyError::PauseTimeout`]. The caller must then
    /// [`Pane::resume`] every pane it paused and keep serving: a paused pane
    /// whose child keeps writing fills the terminal's buffer (measured at
    /// ~8–12 KiB on a Linux pty — *not* a pipe's 64 KiB, see
    /// `Pane::resume`) and the child blocks until someone reads.
    ///
    /// Idempotent: a second pause inside one handoff is not a new request.
    pub fn pause(&self, timeout: Duration) -> Result<(), PtyError> {
        self.gate.pause(timeout)
    }

    /// Let this pane's reader pump run again. **Every abort path on the sending
    /// side must call this before it returns to serving**, or the agent behind
    /// the pane blocks for ever once its output passes the terminal buffer's
    /// ~8–12 KiB: a stuck agent is a worse outcome than an update that did not
    /// happen. Safe on a pane that was never paused and on one whose pump has
    /// ended (it stays quiescent).
    pub fn resume(&self) {
        self.gate.resume();
    }

    /// Byte-exact snapshot of all raw output so far (for fixture recording).
    /// Returns `(bytes, truncated)`.
    #[must_use]
    pub fn raw_snapshot(&self) -> (Vec<u8>, bool) {
        self.buffer
            .lock()
            .map(|b| (b.raw_bytes(), b.raw_truncated()))
            .unwrap_or_default()
    }

    /// Restore pre-crash history into the ring (T-0018). Safe: bypasses the
    /// PTY entirely (see `prepend_history` — keystroke replay would execute
    /// metachar lines). Call immediately after spawn, before live output.
    pub fn restore_history(&self, history: &[String]) {
        if let Ok(mut buf) = self.buffer.lock() {
            buf.prepend_history(history);
        }
    }

    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.buffer.lock().map(|b| b.dropped()).unwrap_or(0)
    }

    #[must_use]
    pub fn bytes_held(&self) -> usize {
        self.buffer.lock().map(|b| b.bytes_held()).unwrap_or(0)
    }

    #[must_use]
    pub fn child_pid(&self) -> Option<u32> {
        self.child.lock().ok().and_then(|c| c.process_id())
    }

    /// This pane's master descriptor, for a handoff to pass over `SCM_RIGHTS`.
    ///
    /// Borrowed, not owned: the descriptor belongs to the pane, which the caller
    /// must keep alive for as long as it uses the number (the handoff holds an
    /// `Arc<Pane>`). `None` where the platform's master has no descriptor — a
    /// pane that cannot be transferred, which the handoff reports rather than
    /// silently dropping.
    #[cfg(unix)]
    #[must_use]
    pub fn master_fd(&self) -> Option<std::os::unix::io::RawFd> {
        let master = self.master.lock().unwrap_or_else(|e| e.into_inner());
        master.as_raw_fd()
    }

    /// Non-blocking exit poll. Observes EOF as well as the child handle, so a
    /// pane whose child already reaped elsewhere still reports termination.
    /// An adopted pane ([`Pane::adopt`]) reports
    /// [`ExitState::Exited(UNKNOWN_EXIT)`]: it is known to be gone, but this
    /// process can never reap the status it went with.
    #[must_use]
    pub fn try_wait(&self) -> ExitState {
        if let Ok(mut child) = self.child.lock() {
            match child.try_wait() {
                Ok(Some(status)) => return ExitState::Exited(status.exit_code()),
                Ok(None) => {}
                Err(_) => return ExitState::Exited(1),
            }
        }
        if self.closed.load(std::sync::atomic::Ordering::SeqCst) {
            return ExitState::Exited(0);
        }
        ExitState::Running
    }

    /// Blocking wait with timeout. Returns `None` on timeout.
    pub fn wait_timeout(&self, timeout: Duration) -> Option<ExitState> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.try_wait() {
                ExitState::Running => {
                    if std::time::Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                exited => return Some(exited),
            }
        }
    }

    /// Kill the child process (exclusive handle).
    pub fn kill(&mut self) -> Result<(), PtyError> {
        self.killer.lock().map_err(|_| PtyError::Closed)?.kill()?;
        Ok(())
    }

    /// Kill through a shared handle (daemon registry holds `Arc<Pane>`).
    /// Same syscall as `kill`; interior mutability bridges `&self`.
    pub fn kill_shared(&self) -> Result<(), PtyError> {
        self.killer.lock().map_err(|_| PtyError::Closed)?.kill()?;
        Ok(())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
    use std::os::unix::io::BorrowedFd;

    /// The sending side of the adoption, in miniature: a real pty whose child
    /// keeps the master alive, with the test holding the other view of the
    /// terminal so a mutation by the pane is visible.
    struct Fixture {
        master: Box<dyn MasterPty + Send>,
        child: Box<dyn Child + Send + Sync>,
    }

    impl Fixture {
        fn open_unsized() -> (Self, std::os::unix::io::OwnedFd) {
            let pair = native_pty_system()
                .openpty(PtySize {
                    rows: 24,
                    cols: 80,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .expect("openpty");
            let mut cmd = CommandBuilder::new("sh");
            cmd.args(["-c", "sleep 60"]);
            cmd.set_controlling_tty(true);
            let child = pair.slave.spawn_command(cmd).expect("spawn in the pty");
            drop(pair.slave);
            let raw = pair
                .master
                .as_raw_fd()
                .expect("a pty master has a descriptor");
            // SAFETY: `master` owns the descriptor and outlives this borrow,
            // which is used only to read the geometry and take a duplicate.
            let borrowed = unsafe { BorrowedFd::borrow_raw(raw) };
            // A terminal nobody has sized yet: the one state the repair exists
            // for, and therefore the one where a stray repair is visible.
            rustix::termios::tcsetwinsize(
                borrowed,
                rustix::termios::Winsize {
                    ws_row: 0,
                    ws_col: 0,
                    ws_xpixel: 0,
                    ws_ypixel: 0,
                },
            )
            .expect("clear the terminal size");
            let fd = rustix::io::fcntl_dupfd_cloexec(borrowed, 0).expect("duplicate the master");
            (
                Self {
                    master: pair.master,
                    child,
                },
                fd,
            )
        }
    }

    /// F6 (stage-2 review): **a handoff adoption does not mutate the inherited
    /// terminal before the commit.** `adopt_seeded` — the handoff path, whose
    /// pump parks until the cut — must leave a 0×0 terminal at 0×0 even though
    /// the seed claims 80×24: an abort after the adoption would send the
    /// terminal back to an outgoing daemon that keeps serving, and a resized
    /// terminal is a SIGWINCHed agent the handoff never earned the right to
    /// disturb. The repair is a separate step (`Pane::repair_geometry`) the
    /// incoming daemon runs after the commit, and the failure mode redone
    /// there is exactly the one the adoption used to do early.
    ///
    /// What removal turns red: moving `repair_geometry` back inside
    /// `adopt_seeded`/`adopt_inner` — the terminal reads (80, 24) right after
    /// the adoption and the first assert fails; `Pane::repair_geometry` losing
    /// the 0×0-only rule — a sized terminal gets overwritten by the claim.
    #[test]
    fn a_handoff_adoption_leaves_the_geometry_until_the_pane_owns_it() {
        let (mut fixture, fd) = Fixture::open_unsized();
        let seed = AdoptSeed {
            child_pid: None,
            size: adopt::AdoptSize { cols: 80, rows: 24 },
            spec: SpawnSpec {
                program: "sh".to_string(),
                args: vec!["-c".to_string(), "sleep 60".to_string()],
            },
            scrollback: Scrollback::default(),
        };
        let pane = Pane::adopt_seeded(fd, seed).expect("adopt the seeded pane");
        let after_adoption = fixture.master.get_size().expect("the sender's view");
        assert_eq!(
            (after_adoption.cols, after_adoption.rows),
            (0, 0),
            "the adoption must not have resized a terminal it may yet give back"
        );
        // The repair, run by the daemon that now owns the pane (after the
        // commit): only 0×0 is set, and the sender's numbers are the claim.
        pane.repair_geometry(80, 24)
            .expect("repair the 0×0 terminal");
        let after_repair = fixture.master.get_size().expect("the sender's view");
        assert_eq!(
            (after_repair.cols, after_repair.rows),
            (80, 24),
            "the post-commit repair applies the sender's numbers to an unset terminal"
        );
        // The repair is idempotent and never overwrites a live geometry: a
        // second call with different numbers changes nothing.
        pane.repair_geometry(200, 50)
            .expect("repair is a no-op on a live geometry");
        let after_second = fixture.master.get_size().expect("the sender's view");
        assert_eq!(
            (after_second.cols, after_second.rows),
            (80, 24),
            "a stale claim must not resize an agent's terminal"
        );
        drop(pane);
        let _ = fixture.child.kill();
        let _ = fixture.child.wait();
    }
}
