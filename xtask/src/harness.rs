//! Shared e2e harness for terminal-facing slices (T-0015, T-0016).
//!
//! One sentence: spawn the real binaries, drive a real pty, and assert on the
//! screen a human would see — no mocks, no fake terminal sizes.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn bins() -> (PathBuf, PathBuf, PathBuf) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .join("target")
        .join("debug");
    (
        dir.join("arreo-server"),
        dir.join("arreo"),
        dir.join("arreo-tui"),
    )
}

pub fn wait_bound(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(socket).is_err() {
        assert!(Instant::now() < deadline, "daemon never bound {socket:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub fn cli(cli_bin: &Path, socket: &Path, args: &[&str]) -> (bool, String) {
    let output = std::process::Command::new(cli_bin)
        .args(args)
        .arg("--socket")
        .arg(socket)
        .output();
    match output {
        Ok(output) => {
            let mut text = String::from_utf8_lossy(&output.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            (output.status.success(), text)
        }
        Err(e) => (false, e.to_string()),
    }
}

/// RAII server handle: kill + reap on drop.
pub struct TestServer {
    child: std::process::Child,
}

impl TestServer {
    pub fn spawn(server_bin: &Path, socket: &Path, what: &str) -> Result<Self, ExitCode> {
        match std::process::Command::new(server_bin)
            .arg("--socket")
            .arg(socket)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => Ok(Self { child }),
            Err(e) => {
                println!("[FAIL] tui: {what}: {e}");
                Err(ExitCode::FAILURE)
            }
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The TUI on a real pty (portable-pty, the same backend the product uses):
/// we own the master side and a winsize, so the app gets a terminal exactly
/// like a user's. Output is accumulated from the master reader; keys are
/// written to the master.
pub struct TuiSession {
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Option<Box<dyn std::io::Write + Send>>,
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    out: Arc<Mutex<Vec<u8>>>,
    rows: u16,
    cols: u16,
}

impl TuiSession {
    pub fn start(tui_bin: &Path, socket: &Path) -> Option<Self> {
        Self::start_with(tui_bin, socket, &[], &[])
    }

    /// Start with extra argv and environment. The terminal environment is
    /// pinned here (`TERM`, then whatever the caller wants to override) so a
    /// slice can reproduce a specific terminal shape without touching the
    /// test process's own environment.
    pub fn start_with(
        tui_bin: &Path,
        socket: &Path,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Option<Self> {
        Self::start_at(tui_bin, Some(socket), args, env)
    }

    /// The same, for a run that names its machine instead of a socket (T-0061):
    /// `--machine` and `--socket` are mutually exclusive on purpose — one names a
    /// machine, the other an address — so this variant passes no socket at all
    /// rather than a path the TUI would have to ignore.
    pub fn start_by_name(tui_bin: &Path, args: &[&str], env: &[(&str, &str)]) -> Option<Self> {
        Self::start_at(tui_bin, None, args, env)
    }

    fn start_at(
        tui_bin: &Path,
        socket: Option<&Path>,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Option<Self> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: 30,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .ok()?;
        let mut cmd = CommandBuilder::new(tui_bin);
        // The developer's own terminal must not decide what the slice proves:
        // clear the capability variables, then apply the shape's environment.
        // (A slice that wants `NO_COLOR` sets it explicitly below.)
        cmd.env_remove("NO_COLOR");
        cmd.env_remove("COLORTERM");
        cmd.env("TERM", "xterm-256color");
        for (key, value) in env {
            cmd.env(key, value);
        }
        for arg in args {
            cmd.arg(arg);
        }
        if let Some(socket) = socket {
            cmd.arg("--socket");
            cmd.arg(socket);
        }
        let child = pair.slave.spawn_command(cmd).ok()?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().ok()?;
        let writer = pair.master.take_writer().ok()?;
        let out = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&out);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if let Ok(mut sink) = sink.lock() {
                    sink.extend_from_slice(&buf[..n]);
                    // A frame is at most a few KB; bound the transcript so a
                    // run cannot grow without limit.
                    let len = sink.len();
                    if len > 4 << 20 {
                        sink.drain(..len - (2 << 20));
                    }
                }
            }
        });
        Some(Self {
            master: Some(pair.master),
            writer: Some(writer),
            child: Some(child),
            out,
            rows: 30,
            cols: 120,
        })
    }

    /// Raw bytes the app has written to the terminal so far.
    pub fn transcript(&self) -> Vec<u8> {
        self.out.lock().map(|b| b.clone()).unwrap_or_default()
    }

    /// Screen as painted, rebuilt from the escape stream (spaces included).
    pub fn screen(&self) -> String {
        let raw = self.out.lock().map(|b| b.clone()).unwrap_or_default();
        render_screen(
            &String::from_utf8_lossy(&raw),
            self.rows as usize,
            self.cols as usize,
        )
    }

    /// Resize the pty — the app must re-lay-out, exactly like a real terminal.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        if let Some(master) = self.master.as_ref() {
            let _ = master.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
        self.rows = rows;
        self.cols = cols;
    }

    pub fn send(&mut self, keys: &str) {
        if let Some(writer) = self.writer.as_mut() {
            let _ = writer.write_all(keys.as_bytes());
            let _ = writer.flush();
        }
    }

    pub fn exited(&mut self) -> bool {
        self.child
            .as_mut()
            .map(|c| matches!(c.try_wait(), Ok(Some(_))))
            .unwrap_or(false)
    }
}

impl Drop for TuiSession {
    fn drop(&mut self) {
        self.writer.take();
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.master.take();
    }
}

/// Minimal ANSI screen: cursor addressing is all ratatui needs to be read back
/// faithfully (SGR and mode switches are styling, not content).
pub fn render_screen(raw: &str, rows: usize, cols: usize) -> String {
    let (rows, cols) = (rows.max(1), cols.max(1));
    let mut grid = vec![vec![' '; cols]; rows];
    let (mut row, mut col) = (0usize, 0usize);
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    let mut params = String::new();
                    let mut final_byte = None;
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            final_byte = Some(c);
                            break;
                        }
                        params.push(c);
                    }
                    let head: &str = params.trim_start_matches('?');
                    let nums: Vec<usize> = head
                        .split(';')
                        .filter_map(|p| p.parse::<usize>().ok())
                        .collect();
                    match final_byte {
                        Some('H') | Some('f') => {
                            row = nums.first().copied().unwrap_or(1).saturating_sub(1);
                            col = nums.get(1).copied().unwrap_or(1).saturating_sub(1);
                        }
                        Some('A') => row = row.saturating_sub(nums.first().copied().unwrap_or(1)),
                        Some('B') => row = (row + nums.first().copied().unwrap_or(1)).min(rows - 1),
                        Some('C') => col = (col + nums.first().copied().unwrap_or(1)).min(cols - 1),
                        Some('D') => col = col.saturating_sub(nums.first().copied().unwrap_or(1)),
                        Some('J') => {
                            grid = vec![vec![' '; cols]; rows];
                            row = 0;
                            col = 0;
                        }
                        Some('K') if row < rows => {
                            for cell in grid[row].iter_mut().skip(col) {
                                *cell = ' ';
                            }
                        }
                        _ => {}
                    }
                }
                // Charset selection (ESC ( B) — consume the designator.
                Some('(') | Some(')') => {
                    chars.next();
                }
                _ => {}
            },
            '\r' => col = 0,
            '\n' => {
                row = (row + 1).min(rows - 1);
                col = 0;
            }
            '\u{8}' => col = col.saturating_sub(1),
            c if c.is_control() => {}
            c => {
                if row < rows {
                    if col < cols {
                        grid[row][col] = c;
                    }
                    col = (col + 1).min(cols - 1);
                }
            }
        }
    }
    grid.iter()
        .map(|line| {
            let s: String = line.iter().collect();
            s.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
