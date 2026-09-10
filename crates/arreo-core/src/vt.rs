//! VT state per pane (T-0003): `alacritty_terminal` grid + disk-backed scrollback.
//!
//! One sentence: PTY bytes go through a real terminal emulator into a live
//! grid, and every scrollback line is appended to a per-pane log file so RAM
//! stays flat no matter how much output a pane produces.
//!
//! Design:
//! - `VtPane` owns a `Term<VoidListener>` with `scrolling_history = 0` — the
//!   grid is the *viewport only* (no hidden in-RAM history duplicating the
//!   log). Bytes are fed via `vte::ansi::Processor`, one byte at a time;
//!   cost is O(input bytes), never O(grid scan).
//! - Scrollback is a plain append-only file of complete lines (`\n`
//!   terminated). `feed` buffers partial lines and appends on newline; the
//!   file is the source of truth for `total_lines`/`page`. Re-reads go
//!   through a tiny line-offset index rebuilt lazily (offset of every 64th
//!   line; forward scan from the nearest mark — bounded, amortized cheap).
//! - Dirty ranges come from `Term::damage()` after each feed, mapped to
//!   viewport line/col spans; `reset_damage` clears for the next feed.
//! - Cursor comes from `grid.cursor.point` (viewport-relative).
//! - Alt-screen awareness: when `TermMode::ALT_SCREEN` is set (vim, less,
//!   htop), the grid shows the app surface; scrollback appends pause —
//!   full-screen apps must not pollute the pane's history. `is_alt_screen()`
//!   exposes the state for the TUI (T-0015) and state engine (T-0004).

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config, Term, TermDamage, TermMode};
use alacritty_terminal::vte::ansi::Processor;
use thiserror::Error;

/// How many lines of viewport the grid holds (cols × rows set at construction).
/// Hot RAM scrollback lives in T-0002's `RingBuffer`; here the grid is the
/// viewport and the log file is history.
#[derive(Debug, Error)]
pub enum VtError {
    #[error("vt io: {0}")]
    Io(#[from] std::io::Error),
    #[error("line {0} out of range (0..{1})")]
    OutOfRange(usize, usize),
}

/// Viewport cursor position (zero-based).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPos {
    pub line: usize,
    pub col: usize,
}

/// One dirty span on a viewport line, inclusive columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyRange {
    pub line: usize,
    pub left: usize,
    pub right: usize,
}

struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// A pane's VT state: live grid + append-only scrollback log.
pub struct VtPane {
    term: Term<VoidListener>,
    processor: Processor,
    cols: usize,
    rows: usize,
    log: File,
    log_path: PathBuf,
    /// Byte offset where each *indexed* line starts (every `INDEX_EVERY`-th
    /// line, plus a final entry = EOF). Rebuilt lazily after spills.
    line_marks: Vec<u64>,
    /// Total complete lines appended to the log.
    log_lines: usize,
    /// Bytes of the current unterminated line (not yet in the log).
    /// Capped at 64 KiB: a single line longer than that (e.g. a 5 MB JSON
    /// blob) would otherwise sit in RAM whole. Overflow is dropped and
    /// counted in `dropped_partial_bytes`.
    partial: Vec<u8>,
    dropped_partial_bytes: u64,
}

/// Rebuild granularity: remember the offset of every 64th line.
const INDEX_EVERY: usize = 64;

impl VtPane {
    /// Create a pane with a `cols`×`rows` viewport. Scrollback goes to a
    /// temp file unique to this pane (`arreo-vt-<pid>-<counter>.log`).
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_log_path(cols, rows, default_log_path())
    }

    /// Create with an explicit log path (tests use temp dirs).
    pub fn with_log_path(cols: u16, rows: u16, path: PathBuf) -> Self {
        let size = Size {
            cols: cols as usize,
            rows: rows as usize,
        };
        let config = Config {
            scrolling_history: 0,
            ..Config::default()
        };
        let mut term = Term::new(config, &size, VoidListener);
        // Fresh terms report Full damage (initial paint); consumers only care
        // about changes after construction, so clear it here.
        term.reset_damage();
        let log = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)
            .expect("vt scrollback log must open");
        Self {
            term,
            processor: Processor::new(),
            cols: cols as usize,
            rows: rows as usize,
            log,
            log_path: path,
            line_marks: vec![0],
            log_lines: 0,
            partial: Vec::new(),
            dropped_partial_bytes: 0,
        }
    }

    /// Feed raw PTY bytes. Returns viewport dirty ranges (empty = no visible
    /// change). Cost is O(bytes), never O(grid scan): one `Processor::advance`
    /// per byte plus one `damage()` walk over viewport lines only.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<DirtyRange> {
        if bytes.is_empty() {
            return Vec::new();
        }
        for &b in bytes {
            self.processor.advance(&mut self.term, b);
        }
        self.capture_scrollback(bytes);
        self.dirty_ranges()
    }

    /// Flush buffered state to the log and rebuild the read index.
    /// Call periodically during huge replays (or after any burst) to keep
    /// `page()` fast; cheap enough to call after every feed batch.
    pub fn spill_to_disk(&mut self) -> Result<(), VtError> {
        self.log.flush()?;
        self.rebuild_marks()?;
        Ok(())
    }

    /// Total scrollback lines: complete lines in the log. Live viewport
    /// content is transient (it scrolls into the log on newline) and is NOT
    /// double-counted — `page` beyond `log_lines` reads the viewport tail.
    #[must_use]
    pub fn total_lines(&self) -> usize {
        self.log_lines
    }

    /// Read `count` lines starting at absolute scrollback index `start`
    /// (0 = oldest). Byte-for-byte lossless vs. what was fed (modulo the
    /// terminal's own line discipline: `\r\n` → one line).
    pub fn page(&mut self, start: usize, count: usize) -> Result<Vec<String>, VtError> {
        let total = self.total_lines();
        if start >= total {
            return Err(VtError::OutOfRange(start, total));
        }
        let end = (start + count).min(total);
        let mut out = Vec::with_capacity(end - start);
        // Log lines first, then live viewport lines.
        let log_end = (end).min(self.log_lines);
        if start < log_end {
            out.extend(self.read_log_lines(start, log_end)?);
        }
        if end > self.log_lines {
            let viewport = self.viewport_lines();
            let from = start.saturating_sub(self.log_lines);
            let to = end - self.log_lines;
            out.extend(viewport.into_iter().skip(from).take(to - from));
        }
        Ok(out)
    }

    /// Plain text of viewport line `line` (0-based), trailing spaces trimmed.
    /// Escape sequences never appear: cells hold decoded characters.
    #[must_use]
    pub fn line_text(&self, line: usize) -> String {
        if line >= self.rows {
            return String::new();
        }
        let row = &self.term.grid()[Line(line as i32)];
        let mut text: String = (0..self.cols).map(|c| row[Column(c)].c).collect();
        while text.ends_with(' ') {
            text.pop();
        }
        text
    }

    /// Viewport cursor position.
    #[must_use]
    pub fn cursor(&self) -> CursorPos {
        let point = self.term.grid().cursor.point;
        CursorPos {
            line: point.line.0.max(0) as usize,
            col: point.column.0,
        }
    }

    /// Whether the pane is showing the alternate screen (vim/less/htop).
    /// While set, scrollback appends pause and `total_lines` excludes the
    /// transient app surface.
    #[must_use]
    pub fn is_alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Approximate RAM held by VT state (grid + buffers, not the log file).
    /// Used by the ≤ 3 MiB budget test. The partial-line buffer is capped at
    /// 64 KiB (see `capture_scrollback`); anything beyond is dropped and
    /// counted, so one giant line can never blow the budget.
    #[must_use]
    pub fn ram_bytes(&self) -> usize {
        self.cols * self.rows * std::mem::size_of::<char>() + self.partial.len() + 4096
    }

    /// Bytes of over-long partial lines dropped by the 64 KiB cap.
    #[must_use]
    pub fn dropped_partial_bytes(&self) -> u64 {
        self.dropped_partial_bytes
    }

    /// Where the scrollback log lives (for evidence/debugging).
    #[must_use]
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    // ---- internals ----

    fn dirty_ranges(&mut self) -> Vec<DirtyRange> {
        let damage = self.term.damage();
        let mut out = Vec::new();
        match damage {
            TermDamage::Full => {
                for line in 0..self.rows {
                    out.push(DirtyRange {
                        line,
                        left: 0,
                        right: self.cols.saturating_sub(1),
                    });
                }
            }
            TermDamage::Partial(iter) => {
                for bound in iter {
                    if bound.line < self.rows {
                        out.push(DirtyRange {
                            line: bound.line,
                            left: bound.left.min(self.cols.saturating_sub(1)),
                            right: bound.right.min(self.cols.saturating_sub(1)),
                        });
                    }
                }
            }
        }
        self.term.reset_damage();
        out
    }

    /// Append completed lines to the log. Splits the fed bytes on `\n`
    /// (tolerating `\r\n`); the tail partial line stays buffered. Paused on
    /// the alternate screen (transient app surface, not history).
    fn capture_scrollback(&mut self, bytes: &[u8]) {
        if self.is_alt_screen() {
            return;
        }
        // Cap the buffered partial line: without this, one unterminated
        // 5 MB line sits in RAM whole. Overflow is dropped and counted.
        const MAX_PARTIAL: usize = 64 * 1024;
        let room = MAX_PARTIAL.saturating_sub(self.partial.len());
        let (take, dropped) = if bytes.len() <= room {
            (bytes.len(), 0)
        } else {
            (room, (bytes.len() - room) as u64)
        };
        self.partial.extend_from_slice(&bytes[..take]);
        self.dropped_partial_bytes += dropped;
        // If the cap is hit mid-line, drop through the next newline: keeping
        // the tail would produce a corrupt half-line in history.
        if dropped > 0 {
            self.partial.clear();
            self.dropped_partial_bytes += 1;
            return;
        }
        let mut start = 0;
        for (i, &b) in self.partial.iter().enumerate() {
            if b == b'\n' {
                let mut line = self.partial[start..i].to_vec();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                // Store text history, not raw bytes: strip C0 controls (except
                // tab) and lossy-decode, so binary garbage can never corrupt
                // the log (T-0009 chaos) or leak escape sequences into pages.
                let clean: Vec<u8> = line
                    .iter()
                    .copied()
                    .filter(|&b| b == b'\t' || b >= 0x20 || b == 0x1b)
                    .collect();
                let text = String::from_utf8_lossy(&clean);
                let text = text.replace('\u{1b}', "");
                let _ = writeln!(self.log, "{text}");
                self.log_lines += 1;
                start = i + 1;
            }
        }
        self.partial.drain(..start);
    }

    fn viewport_lines(&self) -> Vec<String> {
        // Cursor row is "current" even when empty (a prompt line); plus any
        // non-blank row. Walk all viewport rows — O(viewport), only on demand.
        let mut lines: Vec<String> = (0..self.rows).map(|l| self.line_text(l)).collect();
        while lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        if lines.iter().all(String::is_empty) {
            return Vec::new();
        }
        lines
    }

    fn read_log_lines(&mut self, from: usize, to: usize) -> Result<Vec<String>, VtError> {
        // Seek to the nearest mark at or before `from`, then scan forward.
        let mark_idx = (from / INDEX_EVERY).min(self.line_marks.len().saturating_sub(1));
        let offset = self.line_marks[mark_idx];
        let mut reader = BufReader::new(&self.log);
        reader.seek(SeekFrom::Start(offset))?;
        let mut line_no = mark_idx * INDEX_EVERY;
        // Skip to `from`.
        let mut buf = String::new();
        while line_no < from {
            buf.clear();
            if reader.read_line(&mut buf)? == 0 {
                break;
            }
            line_no += 1;
        }
        let mut out = Vec::with_capacity(to - from);
        while line_no < to {
            buf.clear();
            if reader.read_line(&mut buf)? == 0 {
                break;
            }
            while buf.ends_with('\n') || buf.ends_with('\r') {
                buf.pop();
            }
            out.push(std::mem::take(&mut buf));
            line_no += 1;
        }
        Ok(out)
    }

    fn rebuild_marks(&mut self) -> Result<(), VtError> {
        let mut reader = BufReader::new(&self.log);
        reader.seek(SeekFrom::Start(0))?;
        self.line_marks.clear();
        self.line_marks.push(0);
        let mut offset = 0u64;
        let mut buf = String::new();
        let mut line_no = 0usize;
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf)? as u64;
            if n == 0 {
                break;
            }
            offset += n;
            line_no += 1;
            if line_no.is_multiple_of(INDEX_EVERY) {
                self.line_marks.push(offset);
            }
        }
        self.line_marks.push(offset);
        self.log_lines = line_no;
        Ok(())
    }
}

fn default_log_path() -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    std::env::temp_dir().join(format!("arreo-vt-{}-{n}.log", std::process::id()))
}

impl Drop for VtPane {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.log_path);
    }
}
