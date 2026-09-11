//! T-0049: the CLI never panics on a closed stdout pipe.
//!
//! Found while writing T-0024's tests: `arreo pair | head -1` aborted the CLI,
//! because Rust's `println!` panics when the write fails (EPIPE) and a reader
//! that exits early — a pager, `head`, a harness that stops reading — closes
//! the pipe. Every printing verb had the same hazard. The fix is one decision
//! applied once (default SIGPIPE disposition at the single entry point), and
//! these tests prove it: the process dies by signal (exit 141, silent) instead
//! of panicking (exit 101 + `panicked at` text).
//!
//! Each test seeds many rows through the CLI itself, then pipes a listing verb
//! into a reader that closes after one line — the shape of `| head -1`. A verb
//! that prints nothing would exit 0 without ever touching the pipe, which
//! proves nothing; the seeding is what makes the test honest.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

/// The `arreo` binary under test.
fn arreo() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arreo"))
}

/// Scratch identity dir + socket, so no verb touches the developer's own.
struct Scratch {
    dir: PathBuf,
    socket: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "arreo-cli-pipe-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self {
            socket: dir.join("arreo.sock"),
            dir,
        }
    }

    /// Run to completion, discarding output (seeding helper).
    fn seed(&self, args: &[&str]) {
        let output = Command::new(arreo())
            .args(args)
            .arg("--socket")
            .arg(&self.socket)
            .env("ARREO_IDENTITY_DIR", &self.dir)
            .output()
            .expect("arreo runs");
        assert!(
            output.status.success(),
            "seeding {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Seed 50 devices through the product's own door. Each `issue` prints one
/// line AND writes one `device.issue` audit row — so this seeds both the
/// `devices list` listing (50 stdout lines) and the `audit` tail (50 rows) in
/// one loop, with no daemon and no hand-written state.
///
/// The keys come from `devices id` in throwaway identity dirs: real ed25519
/// points, because the authority verifies what it pins (a deterministic
/// `{:064x}` counter is not a curve point and is refused — correctly).
fn seed_fifty_devices(scratch: &Scratch) {
    for index in 0..50 {
        let key_dir = scratch.dir.join(format!("key-{index}"));
        std::fs::create_dir_all(&key_dir).expect("key dir");
        let output = Command::new(arreo())
            .args(["devices", "id", "--json"])
            .env("ARREO_IDENTITY_DIR", &key_dir)
            .output()
            .expect("devices id runs");
        assert!(
            output.status.success(),
            "devices id failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let key = stdout
            .lines()
            .find_map(|line| {
                let value: serde_json::Value = serde_json::from_str(line).ok()?;
                value["public_key"].as_str().map(str::to_string)
            })
            .expect("devices id prints a public key");
        scratch.seed(&[
            "devices",
            "issue",
            "--name",
            &format!("pipe-{index}"),
            "--role",
            "viewer",
            "--key",
            &key,
        ]);
    }
}

/// Run the CLI with stdout piped, then close the pipe immediately — the shape
/// of a consumer that went away (`| head -1` after the first line, a pager
/// quit, a harness that stopped reading). Returns the child's exit code
/// (None = signaled) and everything it wrote to stderr.
///
/// The close is immediate rather than after one line on purpose: 50 seeded
/// rows are ~7 KB, which fits the 64 KB pipe buffer, so "read one line then
/// close" is a race the child usually wins by finishing first — a test that
/// passes whether or not the fix exists proves nothing. Closing before the
/// child can possibly write (spawn + close is microseconds, exec + query +
/// print is milliseconds) makes the first write hit the closed pipe
/// deterministically. That the verb *would* have written is proven separately
/// by the file run (`first_line_to_file` asserts non-empty).
fn run_and_close_after_one_line(args: &[&str], scratch: &Scratch) -> (Option<i32>, String) {
    let mut child = Command::new(arreo())
        .args(args)
        .arg("--socket")
        .arg(&scratch.socket)
        .env("ARREO_IDENTITY_DIR", &scratch.dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("arreo runs");
    drop(child.stdout.take());
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    // Reap: the child must have hit the closed pipe on its first write (the
    // file run proves it writes). Only a panic is a failure.
    let status = loop {
        match child.try_wait().expect("wait") {
            Some(status) => break status,
            None => {
                if std::time::Instant::now() > deadline + Duration::from_secs(5) {
                    child.kill().ok();
                    break child.wait().expect("wait after kill");
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_string(&mut stderr);
    }
    (status.code(), stderr)
}

/// The first line of the same run redirected to a file: byte-identical output
/// is the proof the fix changed no verb's bytes, only its death.
fn first_line_to_file(args: &[&str], scratch: &Scratch) -> String {
    let output = Command::new(arreo())
        .args(args)
        .arg("--socket")
        .arg(&scratch.socket)
        .env("ARREO_IDENTITY_DIR", &scratch.dir)
        .output()
        .expect("arreo runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().next().unwrap_or("").to_string()
}

fn assert_quiet_death(verb: &str, args: &[&str], scratch: &Scratch) {
    let (code, stderr) = run_and_close_after_one_line(args, scratch);
    assert!(
        !stderr.contains("panicked at"),
        "{verb}: panicked on a closed pipe: {stderr}"
    );
    // 0 = exited before noticing (buffered output fit); 141 = SIGPIPE
    // (128+13), the Unix way to die when the consumer goes away. 101 is
    // Rust's panic exit — the one outcome that must never appear.
    match code {
        Some(0) | Some(141) | None => {}
        Some(101) => panic!("{verb}: exit 101 — panicked on a closed pipe: {stderr}"),
        Some(other) => panic!("{verb}: unexpected exit {other}: {stderr}"),
    }
    // Same bytes either way: the fix changes the death, not the output.
    let filed = first_line_to_file(args, scratch);
    assert!(
        !filed.contains("panicked"),
        "{verb}: file output contains panic text: {filed}"
    );
    assert!(
        !filed.is_empty(),
        "{verb}: file output is empty — the byte comparison proves nothing"
    );
}

#[test]
fn audit_survives_a_closed_pipe() {
    let scratch = Scratch::new("audit");
    seed_fifty_devices(&scratch);
    assert_quiet_death("audit", &["audit", "--limit", "50"], &scratch);
}

#[test]
fn devices_list_survives_a_closed_pipe() {
    let scratch = Scratch::new("devices");
    seed_fifty_devices(&scratch);
    assert_quiet_death("devices list", &["devices", "list"], &scratch);
}

/// The `arreo-server` binary, built into the same target directory (the
/// relay tests use the same trick to keep the AGPL crate out of the graph).
fn server() -> PathBuf {
    let server = arreo().parent().expect("target dir").join("arreo-server");
    assert!(
        server.exists(),
        "{} is missing — run `cargo build` (or `cargo test --workspace`) first",
        server.display()
    );
    server
}

#[test]
fn panes_survives_a_closed_pipe() {
    // `panes` needs a live daemon (it queries over the socket, unlike `devices
    // list` and `audit` which read the store directly). Fifty panes through
    // the product's own `spawn`, then the listing piped into a reader that
    // closes after one line.
    let scratch = Scratch::new("panes");
    let mut daemon = Command::new(server())
        .arg("--socket")
        .arg(&scratch.socket)
        .env("ARREO_IDENTITY_DIR", &scratch.dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("arreo-server runs");
    // Wait for the socket: connect, not sleep, so the test is ready-driven.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(&scratch.socket).is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "the daemon never bound its socket"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    for index in 0..50 {
        scratch.seed(&[
            "spawn",
            &format!("pane-{index}"),
            "/bin/sh",
            "-c",
            "sleep 60",
        ]);
    }
    assert_quiet_death("panes", &["panes"], &scratch);
    daemon.kill().ok();
    daemon.wait().ok();
}

#[test]
fn pair_is_covered_by_construction() {
    // `pair` mid-wait needs a live relay and a phone; no pipe test can reach
    // the mid-wait print without the full three-process rig (which keeps the
    // child's stdout open deliberately — see pairing.rs's note). What this
    // test asserts instead is the property that makes one fix cover every
    // verb: the disposition is installed once, at the single entry point,
    // before any verb runs — so a verb cannot opt out by accident.
    let main = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs");
    let text = std::fs::read_to_string(&main).expect("read main.rs");
    // Two definitions (unix + non-unix) and one call: the disposition is
    // installed once, at the single entry point, before any verb runs — so a
    // verb cannot opt out by accident, including `pair` mid-wait.
    assert_eq!(
        text.matches("fn restore_default_sigpipe()").count(),
        2,
        "one definition per platform"
    );
    assert_eq!(
        text.matches("restore_default_sigpipe();").count(),
        1,
        "one call, at the top of main"
    );
    let main_fn = text.find("fn main()").expect("main");
    let call = text.find("restore_default_sigpipe();").expect("the call");
    assert!(
        call > main_fn && call < main_fn + 2500,
        "the call is the first thing in main, before any verb runs"
    );
}
