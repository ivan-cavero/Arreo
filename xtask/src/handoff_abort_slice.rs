//! T-0038 stage 4 — **abort leaves the old daemon whole**: kill -9 a real
//! incoming daemon at three moments of a real cut, and assert the whole bundle
//! after each one.
//!
//! One sentence: a daemon serves eight panes that tick (`tick-N pid=$$`) while a
//! live `arreo attach` streams them; a *real* `arreo-server --handoff-from`
//! (exactly the process `arreo update --server` spawns) starts the cut; the
//! incoming daemon is **SIGKILLed** at one of three moments; and then, every
//! time: every pane is alive and still producing contiguously, the old daemon is
//! still serving, the attached client never noticed anything but the pause, a
//! retry (`--handoff-from` again) succeeds and carries the panes, and exactly
//! one daemon serves the socket (a third is refused — the `flock` invariant).
//!
//! ## Why kill -9, and why three moments
//!
//! ADR 0021 §2's claim is that **no failure before the commit can leave the
//! machine without a serving daemon**. Every abort path in the daemon has a unit
//! test (the piecemeal coverage in `crates/arreo-server/tests/handoff.rs`), but a
//! process that dies by signal closes its descriptors *without* running any of
//! that code — so the claim has to be exercised with the one abort that cannot
//! be simulated from inside: `SIGKILL`. Three kill points, because the cut has
//! three states and a daemon killed in each leaves a different repair job:
//!
//! - **A — before the transfer.** The incoming daemon dies before it has done
//!   anything. Nothing has moved; the machine must be exactly as it was.
//! - **B — mid-transfer.** The incoming daemon dies while the panes are paused
//!   (every pump stopped, nothing in flight). This is the moment the design
//!   calls atomic, and the one that strands an agent if the abort is mishandled:
//!   a pty with no reader absorbs only ~8–12 KiB before the child blocks
//!   (measured; `.loop/evidence/T-0038/stage2-pty-buffer.txt`).
//! - **C — after the transfer, before the commit.** Every descriptor and the
//!   manifest have arrived at the incoming daemon, and the commit marker has
//!   not: the outgoing daemon is in its commit wait. Everything a *successful*
//!   cut moves has moved, and the success is withheld — the window §2c exists
//!   for.
//!
//! ## How each moment is made deterministic (the part that matters)
//!
//! Timing heuristics are not determinism, so none of the three is a sleep:
//!
//! - **A** needs no trigger: the process is killed immediately after `spawn(2)`,
//!   which is *before* it can have run its handshake (the binary must still be
//!   loading). The evidence is positive, not assumed: the panes are never paused
//!   at all (the client's tick stream shows no interruption) and no `handoff`
//!   commit row is ever written.
//! - **B** is triggered by the **frozen ring**: the attached client's own
//!   transcript is the instrument (`arreo attach` receives each line as the
//!   daemon's pump pushes it), so "no new line for [`FREEZE_MS`]" is a
//!   sub-`sleep` observation of the pause — no polling of a fresh connection,
//!   which would be 50–100 ms of jitter. The kill then lands while `.handoff` is
//!   bound and the pumps are stopped, which is what "mid-transfer" means. The
//!   pause is measured in the hundreds of milliseconds (the evidence records the
//!   window), while the detection lag is one [`FREEZE_MS`].
//! - **C** is triggered by **what has arrived at the incoming daemon**, read from
//!   outside it: the distinct PTY masters in `/proc/<incoming>/fd` (counted by
//!   `tty-index`, so it is "how many pane terminals", not "how many descriptors").
//!   The transfer sends one master per pane, so `distinct == PANES` is positive
//!   evidence that the manifest and every master have arrived — and the commit
//!   still needs the incoming daemon to adopt them, start its accept loop and
//!   send the marker, so the window is wide (measured: 100–400 ms after the last
//!   master, before the old daemon exits). The old daemon being *alive* at the
//!   kill and no `handoff` row existing afterwards is the proof that the commit
//!   had not happened.
//!
//! Each of B and C asserts *why* it was its moment, so a slice that silently
//! killed in the wrong window is a failure rather than a green run.
//!
//! ## The cut is `--handoff-from`, not `arreo update --server`
//!
//! Same reasoning as `reattach_slice`: `update --server`'s added steps (stage the
//! candidate, install it) are T-0070's, and running them against `target/debug`
//! writes to the build tree. The primitive this slice exercises is exactly the
//! one `update --server` spawns (`<staged> --handoff-from <socket>`).
//!
//! ## Hermetic
//!
//! HOME and the XDG directories point inside the slice's scratch directory, the
//! socket and SQLite live beside it, and every process is killed and reaped
//! before the slice returns — a leaked process on a pid-scoped path is
//! indistinguishable from a product defect. The `Scratch`/`Sandbox`/`Daemon`/
//! `AttachChild` helpers duplicate the ones in `xtask/src/reattach_slice.rs` on
//! purpose (a helper a sibling owns must not be refactored under it), with a
//! pointer here instead of a silent second edition.

use crate::harness::bins;
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long one one-shot `arreo` invocation may take before the slice calls it a
/// failure. Every healthy one finishes in well under a second.
const CLI_DEADLINE: Duration = Duration::from_secs(15);

/// The slice name, for `[PASS]`/`[FAIL]` lines and the transcript.
const SLICE: &str = "handoff-abort";

/// How many panes the cut carries. Eight is stage 2's shape (a full machine), and
/// it is also what widens the paused window C has to hit: the outgoing daemon
/// pauses and acknowledges every pump, and the incoming adopts every pane, so
/// the window scales with the count.
const PANES: usize = 8;

/// The pane the attached client watches (its transcript is the instrument B is
/// triggered by).
const WATCH_PANE: &str = "p0";

/// The pane's own tick period (`sleep` in the ticker subshell).
const TICK_PERIOD_MS: u64 = 10;

/// Lines of filler each pane prints before it starts ticking.
///
/// **Why the panes carry a journal at all**: it is what makes C's moment wide
/// enough to hit. When the incoming daemon has received the last pane's master
/// it still has to *adopt* that pane — seed a ring from the manifest's
/// scrollback and feed the whole raw journal through a fresh state engine — and
/// the commit comes after that work. With an empty journal the adoption is a few
/// microseconds and the kill lands after the commit (measured: it did, and the
/// committed-but-killed incoming is the one state the design cannot repair —
/// ADR 0021 §2c accepts it, because the incoming owned the panes). A journal
/// makes the window hundreds of milliseconds, and the size is bounded on the
/// sending side by `MAX_MANIFEST_BYTES` (1 MiB for the whole manifest), so this
/// is 8 × ~90 KiB (a 30-byte line × this many).
const BURST_LINES: usize = 3000;

/// Silence on the attached client's transcript that means **the pumps are
/// paused**. A multiple of the tick period, so a scheduler hiccup or a slow
/// batch of ticks is not mistaken for the pause: at 10 ms per tick this is
/// six missed ticks.
const FREEZE_MS: u64 = 60;

/// Ticks the client must have streamed before the cut — enough that the stream
/// is established rather than still catching up on the ring.
const MIN_PRE_TICKS: u64 = 20;

/// How long a kill-point trigger may take before the point is called a failure.
const TRIGGER_DEADLINE: Duration = Duration::from_secs(20);

/// How long the retry may take. A is the slow one: the killed incoming never
/// connected to the transfer socket, so the outgoing daemon sits out its own
/// 10 s accept bound before it releases the one-handoff lock — and a retry
/// inside that window is refused as *busy* (exit 3, a deferred update), which
/// the retry loop retries rather than fails.
const RETRY_DEADLINE: Duration = Duration::from_secs(45);

/// The three moments, in the order the cut passes through them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum KillPoint {
    Before,
    Mid,
    After,
}

impl KillPoint {
    const ALL: [Self; 3] = [Self::Before, Self::Mid, Self::After];

    fn letter(self) -> &'static str {
        match self {
            Self::Before => "a",
            Self::Mid => "b",
            Self::After => "c",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Before => "A — before the transfer",
            Self::Mid => "B — mid-transfer (the panes paused)",
            Self::After => "C — after the transfer, before the commit",
        }
    }

    /// What the kill is triggered by, for the evidence header.
    fn trigger(self) -> &'static str {
        match self {
            Self::Before => "immediately after spawn(2) — the incoming cannot have run",
            Self::Mid => "the attached client's tick stream stops (the pumps are paused)",
            Self::After => "every pane's master has arrived at the incoming (/proc/<pid>/fd)",
        }
    }
}

pub fn run(rest: &[String]) -> ExitCode {
    let _ = rest; // evidence is always written; today there are no flags
    let root = workspace_root();
    let evidence_dir = root.join(".loop").join("evidence").join("T-0038");
    let (server_bin, cli_bin, _tui_bin) = bins();
    let mut report = Report::default();

    for bin in [&server_bin, &cli_bin] {
        if !bin.exists() {
            report.fail(format!("missing binary {} (build first)", bin.display()));
            return finish(&report);
        }
    }

    for point in KillPoint::ALL {
        let outcome = run_point(point, &server_bin, &cli_bin);
        report.absorb(&outcome);
        let _ = std::fs::create_dir_all(&evidence_dir);
        let path = evidence_dir.join(format!("stage4-{}.txt", point.letter()));
        let mut text = format!(
            "T-0038 stage 4 — kill point {}\n\
             slice: cargo xtask e2e --slice {SLICE}\n\
             cut: a real `arreo-server --handoff-from <socket>` (what `arreo update --server`\n\
             spawns), SIGKILLed at this point by the trigger: {}\n\
             machine: panes={PANES} tick={TICK_PERIOD_MS}ms watch={WATCH_PANE}\n\n",
            point.title(),
            point.trigger(),
        );
        text.push_str(&outcome.text());
        match std::fs::write(&path, &text) {
            Ok(()) => report.note(format!("evidence: {}", path.display())),
            Err(e) => report.fail(format!("cannot write evidence {}: {e}", path.display())),
        }
    }

    finish(&report)
}

/// Print the summary line and return the slice's exit code.
fn finish(report: &Report) -> ExitCode {
    if report.failures == 0 {
        println!(
            "[PASS] {SLICE}: three kill points, each leaving the old daemon whole ({} checks)",
            report.passes
        );
        ExitCode::SUCCESS
    } else {
        println!(
            "[FAIL] {SLICE}: {} failure(s), {} pass(es)",
            report.failures, report.passes
        );
        ExitCode::FAILURE
    }
}

/// The workspace root: xtask's manifest dir is one level below it.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

/// The slice-wide verdict: each point's checks roll up here, and the per-point
/// evidence files carry the detail.
#[derive(Default)]
struct Report {
    passes: usize,
    failures: usize,
}

impl Report {
    fn note(&mut self, line: impl Into<String>) {
        println!("{SLICE}> note: {}", line.into());
    }
    fn fail(&mut self, what: impl Into<String>) {
        println!("[FAIL] {SLICE}: {}", what.into());
        self.failures += 1;
    }
    fn absorb(&mut self, outcome: &PointOutcome) {
        self.passes += outcome.passes;
        self.failures += outcome.failures;
    }
}

/// Everything one kill point produced: its verdicts, its narrative, and the raw
/// material a human reads when a check fails.
struct PointOutcome {
    point: KillPoint,
    passes: usize,
    failures: usize,
    lines: Vec<String>,
    facts: Vec<String>,
    raw: Vec<String>,
}

impl PointOutcome {
    fn new(point: KillPoint) -> Self {
        Self {
            point,
            passes: 0,
            failures: 0,
            lines: vec![format!(
                "=== kill point {}: {} ===",
                point.letter(),
                point.title()
            )],
            facts: Vec::new(),
            raw: Vec::new(),
        }
    }

    fn pass(&mut self, what: impl Into<String>) {
        let what = what.into();
        println!("[PASS] {SLICE}: {}: {what}", self.point.letter());
        self.lines.push(format!("[PASS] {what}"));
        self.passes += 1;
    }

    fn fail(&mut self, what: impl Into<String>) {
        let what = what.into();
        println!("[FAIL] {SLICE}: {}: {what}", self.point.letter());
        self.lines.push(format!("[FAIL] {what}"));
        self.failures += 1;
    }

    fn check(&mut self, name: &str, ok: bool, detail: impl Into<String>) {
        let detail = detail.into();
        if ok {
            self.pass(format!("{name} ({detail})"));
        } else {
            self.fail(format!("{name}: {detail}"));
        }
    }

    fn note(&mut self, line: impl Into<String>) {
        let line = line.into();
        println!("{SLICE}> {}: {line}", self.point.letter());
        self.lines.push(format!("note: {line}"));
    }

    fn fact(&mut self, line: impl Into<String>) {
        self.facts.push(line.into());
    }

    fn text(&self) -> String {
        let mut out = self.lines.join("\n");
        out.push_str("\n\n--- what was observed ---\n");
        out.push_str(&self.facts.join("\n"));
        out.push_str("\n\n--- the run ---\n");
        out.push_str(&self.raw.join("\n"));
        out.push('\n');
        out
    }
}

/// The scratch directory, removed when it goes out of scope.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Result<Self, String> {
        let root = std::env::var_os("ARREO_E2E_SCRATCH")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("arreo-e2e-abort-{tag}-{}", std::process::id()));
        match std::fs::remove_dir_all(&root) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("clean {}: {e}", root.display())),
        }
        std::fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
        Ok(Self(root))
    }

    fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The environment every process in this slice gets (the daemon keys its SQLite
/// off the socket path, but its device-authority bootstrap follows
/// `XDG_DATA_HOME`), so the slice never touches the developer's own state.
#[derive(Clone)]
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(root: &Path) -> Result<Self, String> {
        for dir in ["home", "state", "data", "config", "run", "arreo-state"] {
            std::fs::create_dir_all(root.join(dir))
                .map_err(|e| format!("{}/{dir}: {e}", root.display()))?;
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("ARREO_STATE_DIR", self.root.join("arreo-state"));
        command
    }
}

/// A daemon this slice spawned: kill + reap on drop, with `kill9` as the
/// deliberate SIGKILL the abort points need.
///
/// **`graceful` decides what `Drop` does**, and the distinction is not cosmetic:
/// `arreo server stop` is a message to *the daemon serving that socket*, so
/// sending it from a candidate that was refused (or already exited) would stop
/// the daemon the slice is asserting about. Only the daemons this slice means to
/// leave serving — the outgoing one, and the retry — are stopped gracefully; a
/// candidate that is being tested for refusal is killed and reaped.
struct Daemon {
    child: Child,
    sandbox: Sandbox,
    cli: PathBuf,
    socket: PathBuf,
    log: PathBuf,
    graceful: bool,
    status: Option<std::process::ExitStatus>,
}

impl Daemon {
    /// A daemon this slice expects to leave serving (graceful stop on drop).
    fn spawn(
        sandbox: &Sandbox,
        server_bin: &Path,
        cli_bin: &Path,
        socket: &Path,
        tag: &str,
        args: &[&str],
    ) -> Result<Self, String> {
        Self::spawn_inner(sandbox, server_bin, cli_bin, socket, tag, args, true)
    }

    /// A daemon that must never be sent `server stop`: a candidate whose refusal
    /// is what is being asserted about the *serving* daemon.
    fn spawn_candidate(
        sandbox: &Sandbox,
        server_bin: &Path,
        cli_bin: &Path,
        socket: &Path,
        tag: &str,
        args: &[&str],
    ) -> Result<Self, String> {
        Self::spawn_inner(sandbox, server_bin, cli_bin, socket, tag, args, false)
    }

    fn spawn_inner(
        sandbox: &Sandbox,
        server_bin: &Path,
        cli_bin: &Path,
        socket: &Path,
        tag: &str,
        args: &[&str],
        graceful: bool,
    ) -> Result<Self, String> {
        let log = sandbox.root.join(format!("server-{tag}.log"));
        let out = std::fs::File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
        let err = out
            .try_clone()
            .map_err(|e| format!("{}: {e}", log.display()))?;
        let child = sandbox
            .command(server_bin)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|e| format!("{}: {e}", server_bin.display()))?;
        Ok(Self {
            child,
            sandbox: sandbox.clone(),
            cli: cli_bin.to_path_buf(),
            socket: socket.to_path_buf(),
            log,
            graceful,
            status: None,
        })
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn log_text(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }

    /// Poll the child: `Some(status)` once (then cached), `None` while it runs.
    fn poll(&mut self) -> Option<std::process::ExitStatus> {
        if self.status.is_none() {
            self.status = self.child.try_wait().ok().flatten();
        }
        self.status
    }

    fn alive(&mut self) -> bool {
        self.poll().is_none()
    }

    /// SIGKILL, then reap. This is the abort the slice exists to exercise.
    fn kill9(&mut self) -> Result<std::process::ExitStatus, String> {
        if let Some(status) = self.poll() {
            return Err(format!("the process had already exited ({status})"));
        }
        self.child
            .kill()
            .map_err(|e| format!("kill -9 {}: {e}", self.child.id()))?;
        let status = self
            .child
            .wait()
            .map_err(|e| format!("reap {}: {e}", self.child.id()))?;
        self.status = Some(status);
        Ok(status)
    }

    /// End this daemon: a graceful SIGTERM drain for one this slice leaves
    /// serving (the children go with it — the master descriptors close and the
    /// kernel hangs up their terminals), a plain kill + reap for a candidate
    /// whose refusal is what the slice is asserting about the *other* daemon.
    fn stop(&mut self) {
        if self.poll().is_some() {
            return;
        }
        if !self.graceful {
            let _ = self.child.kill();
            let _ = self.child.wait();
            self.status = self.child.try_wait().ok().flatten();
            return;
        }
        let socket = self.socket.display().to_string();
        let stopped = run_cli(
            &self.sandbox,
            &self.cli,
            &["server", "stop", "--socket", &socket],
            CLI_DEADLINE,
        );
        if !stopped.ok() && self.poll().is_none() {
            let _ = self.child.kill();
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.poll().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

/// One tick the attached client printed: the tick number, the pane's announced
/// pid, when the line arrived, and the raw line.
type ObservedTick = (u64, u32, Instant, String);

/// The `arreo attach` client, as a long-lived child. Its stdout is the pane's own
/// transcript, parsed as it arrives; **that arrival time is the instrument B is
/// triggered by** (a line cannot arrive before the daemon's pump pushed it), and
/// its stderr is where a reconnect would be announced — which is what makes
/// "the client never noticed" checkable rather than asserted.
struct AttachChild {
    child: Child,
    ticks: Arc<Mutex<Vec<ObservedTick>>>,
    stderr: Arc<Mutex<String>>,
    raw: Arc<Mutex<Vec<String>>>,
}

impl AttachChild {
    fn spawn(sandbox: &Sandbox, cli_bin: &Path, socket: &Path, pane: &str) -> Option<Self> {
        let mut child = sandbox
            .command(cli_bin)
            .arg("attach")
            .arg(pane)
            .arg("--socket")
            .arg(socket)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let stderr = child.stderr.take()?;
        let ticks = Arc::new(Mutex::new(Vec::new()));
        let raw = Arc::new(Mutex::new(Vec::new()));
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        {
            let ticks = Arc::clone(&ticks);
            let raw = Arc::clone(&raw);
            std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    if let Some((n, pid)) = parse_tick(&line) {
                        let ts = Instant::now();
                        if let Ok(mut ticks) = ticks.lock() {
                            ticks.push((n, pid, ts, line.clone()));
                        }
                    }
                    if let Ok(mut raw) = raw.lock() {
                        raw.push(line);
                    }
                }
            });
        }
        {
            let stderr_buf = Arc::clone(&stderr_buf);
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if let Ok(mut buf) = stderr_buf.lock() {
                        buf.push_str(&line);
                        buf.push('\n');
                    }
                }
            });
        }
        Some(Self {
            child,
            ticks,
            stderr: stderr_buf,
            raw,
        })
    }

    fn ticks(&self) -> Vec<ObservedTick> {
        self.ticks.lock().map(|t| t.clone()).unwrap_or_default()
    }

    fn newest(&self) -> Option<ObservedTick> {
        self.ticks().last().cloned()
    }

    fn stderr(&self) -> String {
        self.stderr.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn transcript(&self) -> Vec<String> {
        self.raw.lock().map(|r| r.clone()).unwrap_or_default()
    }

    fn exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for AttachChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One `arreo` invocation, bounded by a deadline (a command that runs past it is
/// killed and reported, never waited on for ever).
struct Run {
    code: Option<i32>,
    output: String,
}

impl Run {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

impl std::fmt::Display for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.output.trim())
    }
}

fn run_cli(sandbox: &Sandbox, bin: &Path, args: &[&str], deadline: Duration) -> Run {
    let start = Instant::now();
    let mut child = match sandbox
        .command(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return Run {
                code: None,
                output: format!("could not start {}: {e}", bin.display()),
            }
        }
    };
    // Both pipes are drained from their own threads: a child that fills a pipe
    // while this loop waits for it to exit would deadlock.
    let stdout_drain = drain(child.stdout.take().expect("stdout was piped"));
    let stderr_drain = drain(child.stderr.take().expect("stderr was piped"));
    let (code, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status.code(), false),
            Ok(None) if start.elapsed() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break (None, true);
            }
        }
    };
    let mut output = stdout_drain.join().unwrap_or_default();
    output.push_str(&stderr_drain.join().unwrap_or_default());
    if timed_out {
        output = format!("timed out after {deadline:?}; it had printed: {output:?}");
    }
    Run { code, output }
}

fn drain<R: Read + Send + 'static>(mut stream: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// Parse `tick-<n> pid=<pid>` lines. Whole-line prefix matching only — `tick-1`
/// is a prefix of `tick-10`.
fn parse_tick(line: &str) -> Option<(u64, u32)> {
    let rest = line.strip_prefix("tick-")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let n = digits.parse().ok()?;
    let pid = rest.split("pid=").nth(1)?.trim().parse().ok()?;
    Some((n, pid))
}

/// Poll `f` every 20 ms until true or `limit` elapses.
fn until(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Poll `f` every millisecond until true or `limit` elapses — for the triggers,
/// where 20 ms of jitter is a third of the window B has to hit.
fn until_fast(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    false
}

/// The distinct PTY masters a process holds, counted **semantically**: the
/// `tty-index` of every `/dev/ptmx` descriptor in `/proc/<pid>/fd`. Four
/// descriptors of one terminal answer the same index, so this is "how many pane
/// terminals this process holds" and not "how many descriptors it happens to
/// have open" — which is what makes it usable as evidence that the transfer
/// arrived (the outgoing daemon sends exactly one master per pane).
fn distinct_ptys(pid: u32) -> BTreeSet<i64> {
    let dir = PathBuf::from(format!("/proc/{pid}/fd"));
    let mut out = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let Ok(target) = std::fs::read_link(entry.path()) else {
            continue;
        };
        if target != Path::new("/dev/ptmx") {
            continue;
        }
        let info = PathBuf::from(format!(
            "/proc/{pid}/fdinfo/{}",
            entry.file_name().to_string_lossy()
        ));
        let Ok(text) = std::fs::read_to_string(&info) else {
            continue;
        };
        for line in text.lines() {
            if let Some(value) = line.strip_prefix("tty-index:") {
                if let Ok(index) = value.trim().parse::<i64>() {
                    out.insert(index);
                }
            }
        }
    }
    out
}

/// Is this process still there?
fn process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// The pane's `tick-N pid=<pid>` program: a burst (to give the ring a journal
/// big enough that C's window is wide — see [`BURST_LINES`]), then a monotonic
/// tick stream that carries the pane's own pid on every line.
fn ticker_script() -> String {
    format!(
        "stty -echo\n\
         yes \"arreo-journal-filler-xxxxxxxxx\" | head -n {BURST_LINES} > /dev/null\n\
         ( i=0; while true; do i=$((i+1)); echo \"tick-$i pid=$$\"; sleep {}; done ) &\n\
         while IFS= read -r line; do echo \"got:$line\"; done",
        TICK_PERIOD_MS as f64 / 1000.0
    )
}

/// The last tick each pane's ring holds, as `(pane, max tick, pid)`.
fn pane_tails(
    sandbox: &Sandbox,
    cli_bin: &Path,
    socket: &Path,
    note: &mut impl FnMut(String),
) -> Vec<(String, u64, u32)> {
    let mut out = Vec::new();
    let socket = socket.display().to_string();
    for n in 0..PANES {
        let id = format!("p{n}");
        let read = run_cli(
            sandbox,
            cli_bin,
            &["read", &id, "--socket", &socket],
            CLI_DEADLINE,
        );
        if !read.ok() {
            note(format!("read {id}: {read}"));
            continue;
        }
        let ticks: Vec<(u64, u32)> = read.output.lines().filter_map(parse_tick).collect();
        match ticks.last() {
            Some((max, pid)) => out.push((id, *max, *pid)),
            None => note(format!("read {id}: no ticker line yet")),
        }
    }
    out
}

/// Read that every pane's ticker is contiguous **within the window the ring
/// holds** (the ring keeps the last 512 lines, so this is the surviving suffix):
/// no gap, no duplicate, one pid.
fn contiguity_problem(lines: &[String]) -> Option<String> {
    let ticks: Vec<(u64, u32)> = lines.iter().filter_map(|l| parse_tick(l)).collect();
    if ticks.len() < 2 {
        return Some(format!("only {} ticker line(s)", ticks.len()));
    }
    let pid = ticks[0].1;
    for pair in ticks.windows(2) {
        if pair[1].1 != pid {
            return Some(format!(
                "the pane's pid changed mid-stream: {} -> {}",
                pair[0].1, pair[1].1
            ));
        }
        if pair[1].0 != pair[0].0 + 1 {
            return Some(format!(
                "the marker stream is not contiguous: tick {} followed by tick {}",
                pair[0].0, pair[1].0
            ));
        }
    }
    None
}

/// Run one kill point end to end and return everything it observed.
fn run_point(point: KillPoint, server_bin: &Path, cli_bin: &Path) -> PointOutcome {
    let mut out = PointOutcome::new(point);
    let scratch = match Scratch::new(point.letter()) {
        Ok(scratch) => scratch,
        Err(e) => {
            out.fail(format!("scratch: {e}"));
            return out;
        }
    };
    let sandbox = match Sandbox::new(scratch.root()) {
        Ok(sandbox) => sandbox,
        Err(e) => {
            out.fail(format!("sandbox: {e}"));
            return out;
        }
    };
    let socket = scratch.root().join("a.sock");
    let socket_arg = socket.display().to_string();

    // ---- the outgoing daemon and its panes --------------------------------
    let mut old = match Daemon::spawn(
        &sandbox,
        server_bin,
        cli_bin,
        &socket,
        "old",
        &["--socket", &socket_arg],
    ) {
        Ok(daemon) => daemon,
        Err(e) => {
            out.fail(format!("outgoing daemon: {e}"));
            return out;
        }
    };
    if !until(Duration::from_secs(15), || {
        std::os::unix::net::UnixStream::connect(&socket).is_ok()
    }) {
        out.fail(format!(
            "the outgoing daemon never bound {}; it said: {}",
            socket.display(),
            old.log_text()
        ));
        return out;
    }
    let old_pid = old.pid();
    out.pass(format!("outgoing daemon serves (pid {old_pid})"));

    let script = ticker_script();
    for n in 0..PANES {
        let id = format!("p{n}");
        let spawned = run_cli(
            &sandbox,
            cli_bin,
            &[
                "spawn",
                &id,
                "/bin/sh",
                "-c",
                &script,
                "--socket",
                &socket_arg,
            ],
            CLI_DEADLINE,
        );
        if !spawned.ok() {
            out.fail(format!("spawn {id}: {spawned}"));
            return out;
        }
    }
    let mut notes: Vec<String> = Vec::new();
    let producing = until(Duration::from_secs(20), || {
        pane_tails(&sandbox, cli_bin, &socket, &mut |_| {}).len() == PANES
    });
    if !producing {
        out.fail("the panes never produced ticks before the cut");
        return out;
    }
    let pre = pane_tails(&sandbox, cli_bin, &socket, &mut |n| notes.push(n));
    out.pass(format!(
        "{PANES} panes tick before the cut ({})",
        pre.iter()
            .map(|(id, tick, pid)| format!("{id}:tick-{tick}/pid={pid}"))
            .collect::<Vec<_>>()
            .join(" ")
    ));

    // ---- the client attached throughout -----------------------------------
    let Some(mut attach) = AttachChild::spawn(&sandbox, cli_bin, &socket, WATCH_PANE) else {
        out.fail("cannot start `arreo attach`");
        return out;
    };
    if !until(Duration::from_secs(20), || {
        attach
            .newest()
            .is_some_and(|(n, _, _, _)| n >= MIN_PRE_TICKS)
    }) {
        out.fail(format!(
            "the client never streamed {MIN_PRE_TICKS} ticks; its stderr: {:?}",
            attach.stderr()
        ));
        return out;
    }
    out.pass(format!(
        "a client is attached to {WATCH_PANE} and streaming ({} ticks)",
        attach.ticks().len()
    ));

    // ---- the cut -----------------------------------------------------------
    let t_cut = Instant::now();
    let mut incoming = match Daemon::spawn_candidate(
        &sandbox,
        server_bin,
        cli_bin,
        &socket,
        "incoming",
        &[
            "--handoff-from",
            &socket_arg,
            "--handoff-timeout-secs",
            "30",
        ],
    ) {
        Ok(daemon) => daemon,
        Err(e) => {
            out.fail(format!("incoming daemon: {e}"));
            return out;
        }
    };
    let incoming_pid = incoming.pid();

    let kill_detail = match point {
        KillPoint::Before => {
            // Immediately: the process cannot have run its handshake yet (the
            // binary is still loading). Its death is the whole event.
            match incoming.kill9() {
                Ok(status) => format!(
                    "killed {:.1}ms after spawn ({status})",
                    t_cut.elapsed().as_secs_f64() * 1000.0
                ),
                Err(e) => {
                    out.fail(format!("kill -9 before the transfer: {e}"));
                    return out;
                }
            }
        }
        KillPoint::Mid => {
            let froze = until_fast(TRIGGER_DEADLINE, || {
                attach
                    .newest()
                    .is_some_and(|(_, _, ts, _)| ts.elapsed() >= Duration::from_millis(FREEZE_MS))
            });
            if !froze {
                out.fail(format!(
                    "the panes never paused: no {FREEZE_MS}ms silence arrived on the client's \
                     transcript within {TRIGGER_DEADLINE:?} (the cut may have completed)"
                ));
                return out;
            }
            // The premise is the *pause*, not the transfer socket. The `.handoff`
            // path exists only for the milliseconds of the descriptor transfer,
            // so requiring it here raced the cut and failed the slice's own
            // premise on a healthy run (measured). What is load-bearing: the
            // panes are still paused — the outgoing daemon pauses only while a
            // handoff is in flight — the incoming is alive, and no commit has
            // happened. That is "mid-handoff" whether the fds have arrived yet
            // or not, and the assertion bundle below holds either way.
            let still_paused = attach
                .newest()
                .is_some_and(|(_, _, ts, _)| ts.elapsed() >= Duration::from_millis(FREEZE_MS));
            if !still_paused || !incoming.alive() {
                out.fail(format!(
                    "the panes were not still paused with the incoming daemon alive \
                     (still paused: {still_paused}, incoming alive: {})",
                    incoming.alive()
                ));
                return out;
            }
            let fds = distinct_ptys(incoming_pid).len();
            match incoming.kill9() {
                Ok(status) => format!(
                    "killed {:.1}ms into the pause ({status}); {fds}/{PANES} pane terminal(s) had \
                     arrived at the incoming daemon",
                    t_cut.elapsed().as_secs_f64() * 1000.0
                ),
                Err(e) => {
                    out.fail(format!("kill -9 mid-transfer: {e}"));
                    return out;
                }
            }
        }
        KillPoint::After => {
            let arrived = until_fast(TRIGGER_DEADLINE, || {
                distinct_ptys(incoming_pid).len() >= PANES
            });
            let fds = distinct_ptys(incoming_pid).len();
            if !arrived {
                out.fail(format!(
                    "only {fds}/{PANES} pane terminals arrived at the incoming daemon within \
                     {TRIGGER_DEADLINE:?}"
                ));
                return out;
            }
            // **The commit has not happened, checked positively**: the outgoing
            // daemon paused the pumps before it sent anything and only the
            // incoming's commit can end this cut, so a still-frozen client
            // stream is the outgoing daemon still being the one serving panes.
            // Without this the point can land *after* the commit — measured, on
            // the first run of this slice — and a committed-then-killed incoming
            // is the one state the design cannot repair (ADR 0021 §2c: the byte
            // is authorisation, and the panes are the incoming's by then).
            let still_paused = attach
                .newest()
                .is_some_and(|(_, _, ts, _)| ts.elapsed() >= Duration::from_millis(FREEZE_MS));
            if !incoming.alive() {
                out.fail("the incoming daemon exited before the kill: the cut committed");
                return out;
            }
            if old.poll().is_some() {
                out.fail("the outgoing daemon exited before the kill: the cut committed");
                return out;
            }
            if !still_paused {
                out.fail(
                    "the panes were not paused at the kill, so the commit had already been \
                     sent — the window this point exists for was missed",
                );
                return out;
            }
            match incoming.kill9() {
                Ok(status) => format!(
                    "killed {:.1}ms after the cut began, with {fds}/{PANES} pane terminals and \
                     the manifest arrived ({status}); the panes were still paused and the \
                     outgoing daemon was still serving, so the commit was withheld",
                    t_cut.elapsed().as_secs_f64() * 1000.0
                ),
                Err(e) => {
                    out.fail(format!("kill -9 after the transfer: {e}"));
                    return out;
                }
            }
        }
    };
    out.note(kill_detail.clone());
    out.fact(format!("kill: {kill_detail}"));

    // ---- the bundle --------------------------------------------------------
    // 1. No cut was recorded: the commit is evidence, and it was withheld.
    let commit_rows = audit_rows(&sandbox, cli_bin, &socket, "handoff");
    let commit_count = commit_rows.trim().lines().filter(|l| !l.is_empty()).count();
    out.check(
        "no cut was recorded (no `handoff` commit row)",
        commit_count == 0,
        if commit_count == 0 {
            "0 rows".to_string()
        } else {
            format!("{commit_count} row(s): {}", commit_rows.trim())
        },
    );
    // 2. The old daemon is still there and still serving.
    let old_alive = old.alive();
    let served = run_cli(
        &sandbox,
        cli_bin,
        &["panes", "--socket", &socket_arg],
        CLI_DEADLINE,
    );
    let served_ok = served.ok() && (0..PANES).all(|n| served.output.contains(&format!("p{n}")));
    out.check(
        "the old daemon is still serving (a client's request is answered)",
        old_alive && served_ok,
        format!(
            "pid {old_pid} alive: {old_alive}; `arreo panes` lists all {PANES} panes: {served_ok}"
        ),
    );
    if !old_alive {
        out.fact(
            "the outgoing daemon is gone; the rest of the bundle cannot be judged".to_string(),
        );
        out.raw
            .push(format!("--- old daemon log ---\n{}", old.log_text()));
        attach.kill();
        return out;
    }
    // 3. Every pane is alive and still producing, contiguously.
    let post = pane_tails(&sandbox, cli_bin, &socket, &mut |n| notes.push(n));
    let mut pane_problems: Vec<String> = Vec::new();
    for (id, tick, pid) in &post {
        if !process_alive(*pid) {
            pane_problems.push(format!("{id}: pid {pid} is gone"));
            continue;
        }
        let pre_tick = pre
            .iter()
            .find(|(p, _, _)| p == id)
            .map(|(_, t, _)| *t)
            .unwrap_or(0);
        if *tick <= pre_tick {
            pane_problems.push(format!(
                "{id}: stalled at tick-{tick} (was tick-{pre_tick})"
            ));
        }
    }
    if post.len() != PANES {
        pane_problems.push(format!("only {}/{} panes answered", post.len(), PANES));
    }
    let pre_pids: Vec<u32> = pre.iter().map(|(_, _, pid)| *pid).collect();
    let post_pids: Vec<u32> = post.iter().map(|(_, _, pid)| *pid).collect();
    if pre_pids != post_pids {
        pane_problems.push(format!(
            "a pane's pid changed across the abort: {pre_pids:?} -> {post_pids:?}"
        ));
    }
    out.check(
        "every pane is alive and still producing",
        pane_problems.is_empty(),
        if pane_problems.is_empty() {
            format!(
                "{} panes, pids {:?}, ticking past their pre-abort marks",
                post.len(),
                post_pids
            )
        } else {
            pane_problems.join("; ")
        },
    );
    // ...and contiguous, per pane, over the window the ring holds.
    let mut contiguity_problems: Vec<String> = Vec::new();
    for n in 0..PANES {
        let id = format!("p{n}");
        let read = run_cli(
            &sandbox,
            cli_bin,
            &["read", &id, "--socket", &socket_arg],
            CLI_DEADLINE,
        );
        if !read.ok() {
            contiguity_problems.push(format!("{id}: {read}"));
            continue;
        }
        let lines: Vec<String> = read.output.lines().map(str::to_string).collect();
        if let Some(problem) = contiguity_problem(&lines) {
            contiguity_problems.push(format!("{id}: {problem}"));
        }
    }
    out.check(
        "every pane's marker stream is contiguous (no loss, no duplicate, no restart)",
        contiguity_problems.is_empty(),
        if contiguity_problems.is_empty() {
            format!("{PANES} panes, one pid each, every tick +1")
        } else {
            contiguity_problems.join("; ")
        },
    );
    // 4. The client never noticed, beyond the pause the abort itself caused.
    let ticks = attach.ticks();
    let first_pid = ticks.first().map(|(_, pid, _, _)| *pid);
    let one_pid = ticks.iter().all(|(_, pid, _, _)| Some(*pid) == first_pid);
    let contiguous = ticks.windows(2).all(|w| w[1].0 == w[0].0 + 1);
    let stderr_so_far = attach.stderr();
    let noticed_nothing = !stderr_so_far.contains("connection lost")
        && !stderr_so_far.contains("reconnected")
        && !attach.exited();
    out.check(
        "the attached client saw nothing beyond the abort's own pause",
        one_pid && contiguous && noticed_nothing,
        format!(
            "{} lines, one pid ({first_pid:?}), contiguous: {contiguous}, reconnect notices: {}, \
             client alive: {}",
            ticks.len(),
            stderr_so_far.lines().count(),
            !attach.exited()
        ),
    );
    // The pause itself, measured from the client's own arrival times: the gap
    // between the last tick before the kill and the first one after it.
    let (gap, gap_ticks) = tick_gap(&ticks, t_cut);
    out.fact(format!(
        "the client's own clock: the abort cost it {:.1}ms between tick {} and tick {} \
         (the pause, then the resume)",
        gap.as_secs_f64() * 1000.0,
        gap_ticks.0,
        gap_ticks.1
    ));
    // 5. The abort is on the record — where the outgoing daemon could know it.
    let abort_rows = audit_rows(&sandbox, cli_bin, &socket, "handoff.abort");
    let abort_seen = !abort_rows.trim().is_empty();
    match point {
        // A kills the process before it asked for anything, so there is nothing
        // for the outgoing daemon to record: the assertion is that no *cut* was
        // recorded (check 1), not that an abort was.
        KillPoint::Before => out.note(format!(
            "abort rows after A: {} (A dies before it can ask, so the outgoing daemon has \
             nothing to record)",
            if abort_seen {
                abort_rows.trim()
            } else {
                "none"
            }
        )),
        // B and C die inside a transfer the outgoing daemon is running, so the
        // abort must be recorded by the side that knows.
        _ => out.check(
            "the aborted cut is on the audit trail (`handoff.abort`)",
            abort_seen,
            if abort_seen {
                abort_rows.trim().lines().next().unwrap_or("").to_string()
            } else {
                "no rows".to_string()
            },
        ),
    }

    // ---- the retry ---------------------------------------------------------
    // The invariant that makes a retry meaningful: the abort left a daemon
    // serving. Everything below fails loudly if it did not.
    if old.poll().is_some() {
        out.fail(
            "the outgoing daemon exited before the retry could start: the abort left the machine \
             with no serving daemon",
        );
        return out;
    }
    let t_retry = Instant::now();
    let mut attempts: Vec<String> = Vec::new();
    let mut retry_daemon: Option<Daemon> = None;
    while Instant::now() < t_retry + RETRY_DEADLINE {
        let mut candidate = match Daemon::spawn_candidate(
            &sandbox,
            server_bin,
            cli_bin,
            &socket,
            "retry",
            &[
                "--handoff-from",
                &socket_arg,
                "--handoff-timeout-secs",
                "30",
            ],
        ) {
            Ok(daemon) => daemon,
            Err(e) => {
                out.fail(format!("retry daemon: {e}"));
                return out;
            }
        };
        let attempt_deadline = Instant::now() + Duration::from_secs(30);
        let mut refused: Option<std::process::ExitStatus> = None;
        let mut committed = false;
        while Instant::now() < attempt_deadline {
            if old.poll().is_some() {
                committed = true;
                break;
            }
            if let Some(status) = candidate.poll() {
                refused = Some(status);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        if committed {
            retry_daemon = Some(candidate);
            break;
        }
        match refused {
            // A is the slow one here: the killed incoming never connected to the
            // transfer socket, so the outgoing daemon is still inside its own
            // 10 s accept bound. Retrying while it holds the one-handoff lock is
            // answered exit 3 — the deferred update the project's exit-code
            // contract defines — so it is retried, not failed.
            Some(status) if status.code() == Some(3) => attempts.push(format!(
                "refused as busy ({status}) — the aborted handoff still held the one-handoff \
                 lock; retrying"
            )),
            Some(status) => {
                attempts.push(format!("refused ({status})"));
                out.fail(format!(
                    "the retry was refused with {status}, not the busy deferral (exit 3); \
                     attempts: {}",
                    attempts.join(" | ")
                ));
                return out;
            }
            None => {
                out.fail(format!(
                    "the retry never settled within 30s (t={:.1}s)",
                    t_retry.elapsed().as_secs_f64()
                ));
                return out;
            }
        }
    }
    let Some(mut new) = retry_daemon else {
        out.fail(format!(
            "no retry succeeded within {RETRY_DEADLINE:?}; attempts: {}",
            attempts.join(" | ")
        ));
        return out;
    };
    let status = old
        .poll()
        .expect("the outgoing daemon exited (checked above)");
    let new_pid = new.pid();
    // The retry daemon must actually be the one serving: alive, answering
    // `panes` with every pane, and saying so in its own log (the stage-1
    // criterion's observable — there is no `status` verb naming the daemon pid).
    let retry_serves = new.alive()
        && until(Duration::from_secs(15), || {
            let panes = run_cli(
                &sandbox,
                cli_bin,
                &["panes", "--socket", &socket_arg],
                CLI_DEADLINE,
            );
            panes.ok() && (0..PANES).all(|n| panes.output.contains(&format!("p{n}")))
        })
        && new.log_text().contains(&format!("pid {new_pid}"));
    out.check(
        "the retry commits and the outgoing daemon exits 0",
        status.success() && retry_serves,
        format!(
            "retry took {:.2}s after {} attempt(s) ({}); outgoing daemon exit: {status}; the \
             retry daemon (pid {new_pid}) is serving: {retry_serves}",
            t_retry.elapsed().as_secs_f64(),
            attempts.len() + 1,
            if attempts.is_empty() {
                "accepted first time".to_string()
            } else {
                attempts.join(" | ")
            }
        ),
    );
    // The panes travelled: same pids, still producing, and the client's stream
    // is still the same pane (stage 3's reattach is what makes this continuous).
    let after = pane_tails(&sandbox, cli_bin, &socket, &mut |n| notes.push(n));
    let after_pids: Vec<u32> = after.iter().map(|(_, _, pid)| *pid).collect();
    if !until(Duration::from_secs(20), || {
        attach.newest().is_some_and(|(n, _, ts, _)| {
            ts.elapsed() < Duration::from_millis(500) && n > MIN_PRE_TICKS
        }) && attach.stderr().contains("reconnected")
    }) {
        out.note(format!(
            "the client's post-retry stream: {} ticks, stderr: {:?}",
            attach.ticks().len(),
            attach.stderr()
        ));
    }
    let final_ticks = attach.ticks();
    let final_contiguous = final_ticks.windows(2).all(|w| w[1].0 == w[0].0 + 1);
    let final_one_pid = final_ticks
        .iter()
        .all(|(_, pid, _, _)| Some(*pid) == final_ticks.first().map(|(_, p, _, _)| *p));
    out.check(
        "the retry carries the panes (same pids, still ticking)",
        after_pids == pre_pids && after.len() == PANES,
        format!("{:?} -> {:?} ({} panes)", pre_pids, after_pids, after.len()),
    );
    out.check(
        "the client's transcript is one pane, one pid, contiguous across the abort and the retry",
        final_one_pid && final_contiguous,
        format!(
            "{} lines, {} reconnect(s)",
            final_ticks.len(),
            attach.stderr().matches("reconnected").count()
        ),
    );
    // ---- exactly one daemon ------------------------------------------------
    // The retry daemon is serving (its own log says so, naming its pid), the
    // outgoing daemon is gone, the killed incoming is gone, and the flock
    // invariant refuses a third.
    let new_log = new.log_text();
    let serving_line = new_log
        .lines()
        .find(|l| l.contains("handoff complete"))
        .unwrap_or("")
        .to_string();
    out.check(
        "the retry daemon is the one serving",
        serving_line.contains(&format!("pid {new_pid}")),
        if serving_line.is_empty() {
            "the retry daemon logged no `handoff complete` line".to_string()
        } else {
            serving_line.clone()
        },
    );
    let mut third = match Daemon::spawn_candidate(
        &sandbox,
        server_bin,
        cli_bin,
        &socket,
        "third",
        &["--socket", &socket_arg],
    ) {
        Ok(daemon) => daemon,
        Err(e) => {
            out.fail(format!("third daemon: {e}"));
            return out;
        }
    };
    let refused = until(Duration::from_secs(15), || third.poll().is_some());
    let third_status = third.poll();
    let still_accepts = std::os::unix::net::UnixStream::connect(&socket).is_ok();
    out.check(
        "exactly one daemon serves the socket (a third is refused)",
        refused && third_status.is_some_and(|s| !s.success()) && still_accepts,
        format!(
            "the killed incoming (pid {incoming_pid}) is gone, the outgoing (pid {old_pid}) \
             exited, the retry (pid {new_pid}) serves; a third daemon exited {} and the socket \
             still accepts connects: {still_accepts}",
            third_status.map(|s| s.to_string()).unwrap_or_else(|| {
                "never (it is still running — two daemons on one socket)".to_string()
            })
        ),
    );

    // ---- the raw material --------------------------------------------------
    let ticks = attach.ticks();
    let transcript = attach.transcript();
    let (gap, gap_ticks) = tick_gap(&ticks, t_cut);
    out.raw.push(format!(
        "--- the client's transcript ({} lines; the pane's own words) ---\n\
         first: {}\n…\nlast: {}",
        transcript.len(),
        transcript.first().cloned().unwrap_or_default(),
        transcript.last().cloned().unwrap_or_default()
    ));
    out.raw.push(format!(
        "--- the abort's gap in the client's own clock ---\n\
         tick {} arrived {:.1}ms before tick {}; that interval is the pause the abort caused \
         (nothing else interrupted the stream: {} ticks, {} pid(s), contiguous: {})",
        gap_ticks.0,
        gap.as_secs_f64() * 1000.0,
        gap_ticks.1,
        ticks.len(),
        ticks
            .iter()
            .map(|(_, pid, _, _)| *pid)
            .collect::<BTreeSet<_>>()
            .len(),
        ticks.windows(2).all(|w| w[1].0 == w[0].0 + 1),
    ));
    out.raw.push(format!(
        "--- the client's stderr (any notice here belongs to the *retry* below: the assertions \
         above are taken before it runs, and the abort itself must not have broken the \
         connection) ---\n{}",
        attach.stderr()
    ));
    out.raw.push(format!(
        "--- the outgoing daemon's log (tail) ---\n{}",
        tail(&old.log_text(), 12)
    ));
    out.raw.push(format!(
        "--- the retry daemon's log (tail) ---\n{}",
        tail(&new.log_text(), 12)
    ));
    for note in &notes {
        out.raw.push(format!("note: {note}"));
    }
    attach.kill();
    new.stop();
    out
}

/// The gap the abort cost the attached client: the largest interval between two
/// consecutive ticks that straddles `t_cut`.
fn tick_gap(ticks: &[ObservedTick], t_cut: Instant) -> (Duration, (u64, u64)) {
    let mut best: Option<(Duration, (u64, u64))> = None;
    for pair in ticks.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        if a.2 < t_cut && b.2 >= t_cut {
            let gap = b.2.duration_since(a.2);
            if best.as_ref().is_none_or(|(g, _)| gap > *g) {
                best = Some((gap, (a.0, b.0)));
            }
        }
    }
    best.unwrap_or((Duration::ZERO, (0, 0)))
}

/// The last `n` lines of a log, as evidence.
fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(n);
    lines[from..].join("\n")
}

/// The audit rows for one action, through the CLI's own export (which reads the
/// store the daemon writes). Empty output means no rows: `render_export` of an
/// empty window is zero bytes.
fn audit_rows(sandbox: &Sandbox, cli_bin: &Path, socket: &Path, action: &str) -> String {
    let run = run_cli(
        sandbox,
        cli_bin,
        &[
            "audit",
            "export",
            "--action",
            action,
            "--format",
            "jsonl",
            "--out",
            "-",
            "--socket",
            &socket.display().to_string(),
        ],
        CLI_DEADLINE,
    );
    if run.ok() {
        run.output
    } else {
        format!("(audit export failed: {run})")
    }
}
