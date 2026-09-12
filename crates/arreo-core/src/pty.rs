//! PTY manager core (T-0002): spawn, read, write, resize, kill per pane.
//!
//! One blocking reader thread per pane pumps the portable-pty master into a
//! bounded [`RingBuffer`]; writers go straight to the master. No unbounded
//! queues anywhere: when the buffer is full the oldest lines are dropped and
//! [`RingBuffer::dropped`] counts them, so a runaway child can never OOM us.

use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::io::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
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
}

/// How a pane was spawned (remembered for `split`).
#[derive(Debug, Clone)]
pub struct SpawnSpec {
    pub program: String,
    pub args: Vec<String>,
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
        )
    }

    /// Adopt a PTY master that was opened by another process and handed over as
    /// a file descriptor (T-0038: the zero-cut update).
    ///
    /// `master_fd` is a master whose child is already running — the daemon
    /// being replaced forked it, and this one inherits the pane rather than
    /// re-creating it. The resulting [`Pane`] is the same object
    /// [`Pane::spawn`] builds: same reader pump, same bounded ring, so
    /// `send`/`resize`/`drain`/`size`/`raw_snapshot` behave identically.
    ///
    /// `child_pid` is whatever the sender knew; `None` is accepted (an old
    /// sender, or a handoff that could not resolve it) and only costs the
    /// sharper exit detection. `Some(pid)` is only ever used when the inherited
    /// terminal confirms it — the master's session leader is that very pid — so
    /// a pid the terminal contradicts refuses the adoption
    /// ([`PtyError::ChildPidMismatch`]), and a terminal that names *no* session
    /// simply keeps no pid: it cannot confirm the claim, so the claim is
    /// dropped and the pane is watched the way a pane handed over with no pid
    /// is (`adopt::ClaimedPid::NoSession`). A pid that arrived over an untrusted
    /// channel never becomes a signal target on a claim alone.
    ///
    /// A refusal here has changed nothing about the sender's terminal: every
    /// check a handoff can trigger — the descriptor's validity, the pid's claim
    /// — runs before the one step that touches the inherited terminal, so a
    /// transfer this call declines leaves the live pane it came from exactly as
    /// it was. (The final assembly can still fail on resource exhaustion, which
    /// is not a refusal and not something the sender can bring about.)
    ///
    /// `size` is the geometry the sender believes it was serving, in
    /// [`adopt::AdoptSize`]'s named fields, and it is used only when the
    /// inherited master has none: the kernel's geometry is authoritative, so a
    /// stale sender cannot resize a live agent's terminal by lying about it.
    ///
    /// Refuses a descriptor that is not a terminal, one that is a terminal but
    /// not a master, and a pid the master contradicts
    /// ([`PtyError::NotATerminal`] / [`PtyError::NotPtyMaster`] /
    /// [`PtyError::ChildPidMismatch`]); the descriptor is closed in every case.
    #[cfg(unix)]
    pub fn adopt(
        master_fd: OwnedFd,
        child_pid: Option<u32>,
        size: adopt::AdoptSize,
        spec: SpawnSpec,
    ) -> Result<Self, PtyError> {
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
        master.repair_geometry(size)?;
        Self::assemble(Box::new(master), Box::new(child), closed, spec)
    }

    /// Assemble a pane around an already-open master and child handle.
    ///
    /// The single place that installs the reader pump, the bounded ring, the
    /// writer and the `closed` flag: [`Pane::spawn`] and [`Pane::adopt`] differ
    /// only in where `master` and `child` come from. `closed` is passed in
    /// because it is shared, not private here — an adopted child reads it to
    /// learn that the terminal reached end-of-stream.
    fn assemble(
        master: Box<dyn MasterPty + Send>,
        child: Box<dyn portable_pty::Child + Send + Sync>,
        closed: Arc<AtomicBool>,
        spec: SpawnSpec,
    ) -> Result<Self, PtyError> {
        let buffer = Arc::new(Mutex::new(RingBuffer::new(HOT_LINES)));
        let mut reader = master.try_clone_reader()?;
        let pump_buffer = Arc::clone(&buffer);
        let pump_closed = Arc::clone(&closed);
        std::thread::Builder::new()
            .name("arreo-pty-reader".to_string())
            .spawn(move || {
                let mut chunk = [0u8; 8192];
                loop {
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
