//! T-0015 TUI slice: drive the real `arreo-tui` binary on a PTY and assert
//! what a human would see.
//!
//! No mocks: a real daemon on a temp socket, real panes with marker output,
//! the real TUI on a pty (util-linux `script`, sized with `stty`), and
//! assertions on the reconstructed screen — sidebar groups, attention
//! ordering, RAM column, pane content, search prompt, clean quit.
//!
//! `--interactive-evidence` writes the per-step screens to
//! `.loop/evidence/T-0015/` (the frames a reviewer reads).

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn bins() -> (PathBuf, PathBuf, PathBuf) {
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

fn wait_bound(socket: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(socket).is_err() {
        assert!(Instant::now() < deadline, "daemon never bound {socket:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn cli(cli_bin: &Path, socket: &Path, args: &[&str]) -> (bool, String) {
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
struct TestServer {
    child: std::process::Child,
}

impl TestServer {
    fn spawn(server_bin: &Path, socket: &Path, what: &str) -> Result<Self, ExitCode> {
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
struct TuiSession {
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Option<Box<dyn std::io::Write + Send>>,
    child: Option<Box<dyn portable_pty::Child + Send + Sync>>,
    out: Arc<Mutex<Vec<u8>>>,
    rows: u16,
    cols: u16,
}

impl TuiSession {
    fn start(tui_bin: &Path, socket: &Path) -> Option<Self> {
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
        cmd.arg("--socket");
        cmd.arg(socket);
        cmd.env("TERM", "xterm-256color");
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
    fn transcript(&self) -> Vec<u8> {
        self.out.lock().map(|b| b.clone()).unwrap_or_default()
    }

    /// Screen as painted, rebuilt from the escape stream (spaces included).
    fn screen(&self) -> String {
        let raw = self.out.lock().map(|b| b.clone()).unwrap_or_default();
        render_screen(
            &String::from_utf8_lossy(&raw),
            self.rows as usize,
            self.cols as usize,
        )
    }

    /// Resize the pty — the app must re-lay-out, exactly like a real terminal.
    fn resize(&mut self, rows: u16, cols: u16) {
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

    fn send(&mut self, keys: &str) {
        if let Some(writer) = self.writer.as_mut() {
            let _ = writer.write_all(keys.as_bytes());
            let _ = writer.flush();
        }
    }

    fn exited(&mut self) -> bool {
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
fn render_screen(raw: &str, rows: usize, cols: usize) -> String {
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

pub fn run(rest: &[String]) -> ExitCode {
    let evidence = rest.iter().any(|a| a == "--interactive-evidence");
    let evidence_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(".loop")
        .join("evidence")
        .join("T-0015");
    if evidence {
        let _ = std::fs::create_dir_all(&evidence_dir);
    }

    let socket = std::env::temp_dir().join(format!("arreo-e2e-tui-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let (server_bin, cli_bin, tui_bin) = bins();
    for bin in [&server_bin, &cli_bin, &tui_bin] {
        if !bin.exists() {
            println!("[FAIL] tui: missing binary {} (build first)", bin.display());
            return ExitCode::FAILURE;
        }
    }

    let mut failures = 0usize;
    let mut passes = 0usize;
    let mut check = |name: &str, ok: bool, detail: &str| {
        if ok {
            println!("[PASS] tui: {name}");
            passes += 1;
        } else {
            println!("[FAIL] tui: {name}: {detail}");
            failures += 1;
        }
    };

    let server = match TestServer::spawn(&server_bin, &socket, "server start") {
        Ok(server) => server,
        Err(code) => return code,
    };
    wait_bound(&socket);

    // Three panes: one prompt-hung (must sort first), two working.
    let panes = [
        ("alpha", "echo ALPHA-MARKER && sleep 120"),
        ("beta", "sleep 0.5; printf 'Proceed? [y/n] '; sleep 120"),
        ("gamma", "echo GAMMA-MARKER && sleep 120"),
    ];
    for (id, body) in panes {
        let (ok, out) = cli(&cli_bin, &socket, &["spawn", id, "/bin/sh", "-c", body]);
        if !ok {
            println!("[FAIL] tui: spawn {id}: {out}");
            return ExitCode::FAILURE;
        }
    }
    std::thread::sleep(Duration::from_secs(2));
    // Precondition: the daemon must have classified beta's prompt as a
    // question. Waiting here keeps this slice about the UI, not about the
    // classifier's latency (covered by the state slice).
    let (ok, out) = cli(
        &cli_bin,
        &socket,
        &["wait", "beta", "--state", "question", "--timeout", "10s"],
    );
    if !ok {
        println!("[FAIL] tui: beta never entered question state: {out}");
        return ExitCode::FAILURE;
    }

    let mut session = match TuiSession::start(&tui_bin, &socket) {
        Some(session) => session,
        None => {
            println!("[FAIL] tui: could not start TUI under a pty");
            return ExitCode::FAILURE;
        }
    };

    // One poll cycle + a frame.
    std::thread::sleep(Duration::from_secs(3));
    let screen = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("01-overview.txt"), &screen);
    }
    check(
        "sidebar lists all panes",
        screen.contains("alpha") && screen.contains("beta") && screen.contains("gamma"),
        "pane ids missing from sidebar",
    );
    check(
        "attention order puts the hung agent's group first",
        match (screen.find("question"), screen.find("working")) {
            (Some(q), Some(w)) => q < w,
            (Some(_), None) => true,
            _ => false,
        },
        "question group not above working",
    );
    check(
        "state dots rendered",
        screen.contains('◉') && screen.contains('●'),
        "state dots missing",
    );
    check(
        "status bar advertises keys",
        screen.contains("q quit") && screen.contains("j/k move"),
        "key hints missing from status bar",
    );
    check(
        "RAM rendered for every live pane with a unit",
        screen
            .split_whitespace()
            .filter(|w| {
                w.len() > 1
                    && (w.ends_with('K') || w.ends_with('M'))
                    && w[..w.len() - 1].parse::<u64>().is_ok()
            })
            .count()
            >= 3,
        "no KiB/MiB reading for each pane",
    );

    // Focus a pane (j then Enter) — the focused pane streams its output.
    session.send("j");
    std::thread::sleep(Duration::from_millis(600));
    session.send("\r");
    std::thread::sleep(Duration::from_secs(2));
    let focused = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("03-focus-pane.txt"), &focused);
    }
    check(
        "focused pane header shows id + state",
        focused.contains("[question]")
            || focused.contains("[working]")
            || focused.contains("[done]"),
        "no focused-pane header",
    );
    check(
        "focused pane streams daemon output",
        focused.contains("Proceed?") || focused.contains("MARKER"),
        "no pane output in transcript",
    );

    // Search prompt owns the status line.
    session.send("/");
    std::thread::sleep(Duration::from_millis(600));
    session.send("MARK");
    std::thread::sleep(Duration::from_millis(600));
    let searching = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("04-search.txt"), &searching);
    }
    check(
        "search prompt rendered on the status line",
        searching.contains("Enter apply"),
        "search prompt not on screen",
    );
    // Apply the filter: the prompt yields to the result count.
    session.send("\r");
    std::thread::sleep(Duration::from_secs(2));
    let filtered = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("04b-search-applied.txt"), &filtered);
    }
    check(
        "applied search reports match count",
        filtered.contains("matching lines"),
        "no match count after applying the filter",
    );

    // Mouse: click the third pane's sidebar row (SGR report, 1-based).
    // Layout at 120x30: row 1 = question header, 2 = beta, 3 = working
    // header, 4 = alpha, 5 = gamma.
    session.send("\u{1b}[<0;6;5M");
    std::thread::sleep(Duration::from_secs(2));
    let clicked = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("05-mouse-click.txt"), &clicked);
    }
    check(
        "mouse click focuses the clicked row",
        clicked.contains("gamma ["),
        "clicked row did not take focus",
    );

    // Resize: the pty reports a new size; the frame must re-lay-out to it.
    session.resize(24, 100);
    std::thread::sleep(Duration::from_secs(2));
    let resized = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("06-resize-100x24.txt"), &resized);
    }
    let widest = resized
        .lines()
        .map(str::chars)
        .map(Iterator::count)
        .max()
        .unwrap_or(0);
    check(
        "resize re-lays-out to the new width",
        widest == 100 && resized.contains("["),
        "frame did not match the 100-column pty",
    );
    check(
        "no stale rows below the resized frame",
        resized.lines().count() == 24,
        "frame kept rows from the old size",
    );

    // Wall: every pane tiled at once, each tile streaming its own output.
    session.send("w");
    std::thread::sleep(Duration::from_secs(4));
    let wall = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("07-wall.txt"), &wall);
    }
    let tiles = ["alpha [", "beta [", "gamma ["]
        .iter()
        .filter(|title| wall.contains(*title))
        .count();
    check(
        "wall tiles every pane simultaneously",
        tiles == 3,
        &format!("{tiles}/3 pane titles rendered in the wall"),
    );
    check(
        "wall tiles stream their own pane's output",
        wall.contains("ALPHA-MARKER") && wall.contains("GAMMA-MARKER") && wall.contains("Proceed?"),
        "a wall tile is missing its pane's output",
    );

    // Border drag: grab the sidebar edge and widen it; the split must follow.
    let before = session.screen();
    let divider_before = before
        .lines()
        .next()
        .and_then(|l| l.chars().position(|c| c == '┐'));
    session.send("\u{1b}[<0;29;3M"); // press on the border (col 28, 0-based)
    std::thread::sleep(Duration::from_millis(300));
    session.send("\u{1b}[<32;41;3M"); // drag to col 40
    std::thread::sleep(Duration::from_millis(300));
    session.send("\u{1b}[<0;41;3M"); // release
    std::thread::sleep(Duration::from_secs(2));
    let dragged = session.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir.join("08-border-drag.txt"), &dragged);
    }
    let divider_after = dragged
        .lines()
        .next()
        .and_then(|l| l.chars().position(|c| c == '┐'));
    check(
        "mouse drag resizes the sidebar split",
        matches!((divider_before, divider_after), (Some(b), Some(a)) if a > b),
        &format!("divider {divider_before:?} -> {divider_after:?}"),
    );

    // Back to the focus view for the remaining key checks.
    session.send("w");
    std::thread::sleep(Duration::from_secs(2));

    // Steady state: the app must repaint cells, not the screen. A frame is
    // only what changed, so idle output stays tiny and never clears.
    let before = session.transcript().len();
    std::thread::sleep(Duration::from_secs(3));
    let during = session.transcript();
    let idle_bytes = during.len() - before;
    check(
        "steady state repaints deltas only",
        idle_bytes < 4096 && !during[before..].windows(4).any(|w| w == b"\x1b[2J"),
        &format!("{idle_bytes} bytes written while idle"),
    );

    // Quit returns the pty to its parent shell (the process exits).
    session.send("q");
    std::thread::sleep(Duration::from_secs(2));
    check(
        "q quits cleanly",
        session.exited(),
        "TUI still running after q",
    );

    drop(session);
    drop(server);
    let _ = std::fs::remove_file(&socket);
    let mut db = socket.into_os_string();
    db.push(".db");
    let _ = std::fs::remove_file(&db);

    if failures == 0 {
        println!("tui: {passes} passed, 0 failed");
        ExitCode::SUCCESS
    } else {
        println!("tui: {passes} passed, {failures} failed");
        ExitCode::FAILURE
    }
}
