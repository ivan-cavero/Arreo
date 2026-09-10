//! PTY manager core (T-0002): spawn, read, write, resize, kill per pane.
//!
//! One blocking reader thread per pane pumps the portable-pty master into a
//! bounded [`RingBuffer`]; writers go straight to the master. No unbounded
//! queues anywhere: when the buffer is full the oldest lines are dropped and
//! [`RingBuffer::dropped`] counts them, so a runaway child can never OOM us.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use thiserror::Error;

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

        let buffer = Arc::new(Mutex::new(RingBuffer::new(HOT_LINES)));
        let closed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut reader = pair.master.try_clone_reader()?;
        let pump_buffer = Arc::clone(&buffer);
        let pump_closed = Arc::clone(&closed);
        std::thread::Builder::new()
            .name("arreo-pty-reader".to_string())
            .spawn(move || {
                let mut chunk = [0u8; 8192];
                loop {
                    match reader.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Ok(mut buf) = pump_buffer.lock() {
                                buf.push_bytes(&chunk[..n]);
                            }
                        }
                    }
                }
                pump_closed.store(true, std::sync::atomic::Ordering::SeqCst);
            })
            .map_err(PtyError::Io)?;

        let killer = Mutex::new(child.clone_killer());
        let master = Mutex::new(pair.master);
        let writer = Mutex::new(master.lock().map_err(|_| PtyError::Closed)?.take_writer()?);
        Ok(Self {
            master,
            writer,
            buffer,
            child: Arc::new(Mutex::new(child)),
            killer,
            closed,
        })
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
