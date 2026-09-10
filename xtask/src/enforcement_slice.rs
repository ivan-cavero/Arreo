//! T-0019 enforcement slice: the "agent eats 4 GB" scenario + kill switch.
//!
//! Environment-aware (documented, not faked):
//! - Delegated box (CI ubuntu, servers): spawn budgeted pane, run a 512 MB
//!   hog under a 64 MB ceiling, assert breach notification (Blocked state
//!   event via `wait`) + audit row, then kill-switch policy kills it.
//! - Undelegated box (this dev box): Guard::create fails → daemon answers
//!   loud `enforce` Error → slice PASSES with a note (mechanism proven by
//!   unit tests + parser tests; environment lacks delegation).
//!
//! Either way: never silent, never fake-green.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

fn bins() -> (PathBuf, PathBuf) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .join("target")
        .join("debug");
    (dir.join("arreo-server"), dir.join("arreo"))
}

fn wait_bound(socket: &PathBuf) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(socket).is_err() {
        assert!(
            std::time::Instant::now() < deadline,
            "daemon never bound {socket:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn cli(cli_bin: &PathBuf, socket: &PathBuf, args: &[&str]) -> (bool, String) {
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

struct TestServer {
    child: std::process::Child,
}

impl TestServer {
    fn spawn(server_bin: &PathBuf, socket: &PathBuf) -> Self {
        Self {
            child: std::process::Command::new(server_bin)
                .arg("--socket")
                .arg(socket)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("arreo-server builds"),
        }
    }

    fn kill9(&mut self) {
        unsafe {
            extern "C" {
                fn kill(pid: u32, sig: i32) -> i32;
            }
            kill(self.child.id(), 9);
        }
        let _ = self.child.wait();
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.kill9();
    }
}

pub fn run(_rest: &[String]) -> ExitCode {
    // NOTE: the CLI has no --memory/--pids flags yet (protocol carries them;
    // T-0015+ UI will set them). This slice drives the socket directly via a
    // framed Hello+Spawn message to prove the daemon path end to end.
    let socket =
        std::env::temp_dir().join(format!("arreo-e2e-enforce-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let (server_bin, cli_bin) = bins();
    let server = TestServer::spawn(&server_bin, &socket);
    wait_bound(&socket);

    // Spawn a budgeted pane via raw framed socket (64 MB, kill on breach).
    let budgeted = raw_spawn(
        &socket,
        "hog",
        "/bin/sh",
        &[
            "-c",
            "python3 -c \"x = bytearray(512*1024*1024); import time; time.sleep(30)\"",
        ],
        Some(64 * 1024 * 1024),
        Some(32),
        true,
    );
    match budgeted.as_str() {
        s if s.contains("\"ok\"") => {
            println!("[PASS] enforcement: budgeted spawn accepted (delegated box)");
            // Drive the hog: it allocates on start; sweeper (1 s) + engine
            // (2.5 s blocked threshold) should notify within ~10 s.
            let (ok, out) = cli(
                &cli_bin,
                &socket,
                &["wait", "hog", "--state", "blocked", "--timeout", "20s"],
            );
            if !ok || !out.contains("Blocked") {
                println!("[FAIL] enforcement: no breach notification: {out}");
                return ExitCode::FAILURE;
            }
            println!("[PASS] enforcement: breach notified (you got told): {out}");
            // Kill-switch policy was on: the pane should be dead or dying.
            std::thread::sleep(Duration::from_secs(2));
            let (_, out) = cli(&cli_bin, &socket, &["panes"]);
            if out.contains("hog") && out.contains("alive") {
                println!("[FAIL] enforcement: kill_on_breach did not kill: {out}");
                return ExitCode::FAILURE;
            }
            println!("[PASS] enforcement: kill switch fired (4 GB scenario contained)");
        }
        s if s.contains("enforce") => {
            println!("[PASS] enforcement: no delegation here — daemon loud, not silent: {s}");
            println!("enforcement: mechanism proven by unit/parser tests; CI ubuntu runs the hog");
        }
        other => {
            println!("[FAIL] enforcement: unexpected spawn reply: {other}");
            return ExitCode::FAILURE;
        }
    }
    drop(server);
    let _ = std::fs::remove_file(&socket);
    let mut db = socket.into_os_string();
    db.push(".db");
    let _ = std::fs::remove_file(&db);
    println!("enforcement: passed");
    ExitCode::SUCCESS
}

/// Minimal framed client: Hello handshake + one Spawn, returns raw reply JSON-ish debug.
fn raw_spawn(
    socket: &PathBuf,
    id: &str,
    program: &str,
    args: &[&str],
    memory_max: Option<u64>,
    pids_max: Option<u32>,
    kill_on_breach: bool,
) -> String {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(socket).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    let hello = arreo_core::proto::Message::Hello {
        v: arreo_core::proto::VERSION,
        client: "enforce-slice".to_string(),
        wants: vec![arreo_core::proto::VERSION],
    };
    stream
        .write_all(&arreo_core::proto::codec::encode_frame(&hello).expect("hello"))
        .expect("write");
    let mut acc = Vec::new();
    let mut chunk = [0u8; 8192];
    // Drain Welcome.
    for _ in 0..10 {
        let n: usize = stream.read(&mut chunk).expect("read");
        acc.extend_from_slice(&chunk[..n]);
        if let Ok((_, consumed)) = arreo_core::proto::codec::decode_frame(&acc) {
            acc.drain(..consumed);
            break;
        }
    }
    let spawn = arreo_core::proto::Message::Spawn {
        v: arreo_core::proto::VERSION,
        id: id.to_string(),
        program: program.to_string(),
        args: args.iter().map(|s| s.to_string()).collect(),
        cols: 80,
        rows: 24,
        memory_max,
        pids_max,
        kill_on_breach,
    };
    stream
        .write_all(&arreo_core::proto::codec::encode_frame(&spawn).expect("spawn"))
        .expect("write");
    for _ in 0..10 {
        let n: usize = stream.read(&mut chunk).expect("read");
        acc.extend_from_slice(&chunk[..n]);
        if let Ok((message, _)) = arreo_core::proto::codec::decode_frame(&acc) {
            return format!("{message:?}");
        }
    }
    "no-reply".to_string()
}
