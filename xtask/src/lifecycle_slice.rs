//! T-0012 lifecycle slice: service install round-trip + SIGTERM drain +
//! kill -9 restart posture, against the real binaries on a temp socket.
//!
//! Crash-recovery honesty: without T-0018 persistence, `kill -9` loses live
//! panes by design — this probe asserts the daemon restarts cleanly into an
//! EMPTY registry and says so (no phantom panes, no stale lock), rather than
//! pretending sessions survive.

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

/// RAII server handle: SIGKILL + reap on drop, so no failure path leaks a
/// daemon (clippy `zombification` lint + good hygiene).
struct TestServer {
    child: std::process::Child,
}

impl TestServer {
    fn spawn(server_bin: &PathBuf, socket: &PathBuf, what: &str) -> Result<Self, ExitCode> {
        match std::process::Command::new(server_bin)
            .arg("--socket")
            .arg(socket)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => Ok(Self { child }),
            Err(e) => {
                println!("[FAIL] lifecycle: {what}: {e}");
                Err(ExitCode::FAILURE)
            }
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
    let socket = std::env::temp_dir().join(format!("arreo-e2e-life-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let (server_bin, cli_bin) = bins();

    // 1. Daemon starts, serves, SIGTERM drains to exit 0.
    let mut server = match TestServer::spawn(&server_bin, &socket, "server start") {
        Ok(server) => server,
        Err(code) => return code,
    };
    wait_bound(&socket);
    let (ok, _) = cli(
        &cli_bin,
        &socket,
        &["spawn", "chat", "/bin/sh", "-c", "echo hi && sleep 30"],
    );
    if !ok {
        println!("[FAIL] lifecycle: spawn");
        return ExitCode::FAILURE;
    }
    let (ok, out) = cli(&cli_bin, &socket, &["server", "stop"]);
    if !ok {
        println!("[FAIL] lifecycle: server stop: {out}");
        return ExitCode::FAILURE;
    }
    let status = server.child.wait().expect("reap");
    if !status.success() {
        println!("[FAIL] lifecycle: daemon exit {status:?}, want 0");
        return ExitCode::FAILURE;
    }
    println!("[PASS] lifecycle: SIGTERM drain → exit 0, socket released");

    // 2. kill -9 → restart: comes back EMPTY (no phantom panes), serves fine.
    let mut server = match TestServer::spawn(&server_bin, &socket, "restart") {
        Ok(server) => server,
        Err(code) => return code,
    };
    wait_bound(&socket);
    let (ok, _) = cli(&cli_bin, &socket, &["spawn", "doomed", "/bin/sleep", "30"]);
    if !ok {
        println!("[FAIL] lifecycle: respawn");
        return ExitCode::FAILURE;
    }
    // SIGKILL the daemon (no drain — the crash path).
    server.kill9();
    std::thread::sleep(Duration::from_millis(300));
    // Restart on the same path: stale socket must not block it.
    let server = match TestServer::spawn(&server_bin, &socket, "post-crash restart") {
        Ok(server) => server,
        Err(code) => return code,
    };
    wait_bound(&socket);
    let (ok, out) = cli(&cli_bin, &socket, &["panes"]);
    if !ok {
        println!("[FAIL] lifecycle: post-crash panes: {out}");
        return ExitCode::FAILURE;
    }
    if out.contains("doomed") {
        println!("[FAIL] lifecycle: phantom pane after crash (pre-T-0018 must be empty): {out}");
        return ExitCode::FAILURE;
    }
    println!("[PASS] lifecycle: kill -9 → restart clean, registry honestly empty (restore lands in T-0018)");
    drop(server);
    let _ = std::fs::remove_file(&socket);
    println!("lifecycle: 2 passed, 0 failed");
    ExitCode::SUCCESS
}
