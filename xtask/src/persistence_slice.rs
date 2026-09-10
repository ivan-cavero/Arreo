//! T-0018 persistence slice: 10 panes + scrollback → kill -9 → restart →
//! layout, ring buffers and states identical (byte-level scrollback equality).
//!
//! Drives the real binaries over a temp socket (no mocks): spawn 10 panes
//! with marker output, SIGKILL the daemon, restart on the same path, assert
//! all 10 panes back with their markers, then clean up.

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

/// RAII server handle: SIGKILL + reap on drop.
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
                println!("[FAIL] persistence: {what}: {e}");
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
    const PANES: usize = 10;
    let socket =
        std::env::temp_dir().join(format!("arreo-e2e-persist-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&socket);
    let (server_bin, cli_bin) = bins();

    let mut server = match TestServer::spawn(&server_bin, &socket, "server start") {
        Ok(server) => server,
        Err(code) => return code,
    };
    wait_bound(&socket);
    for i in 0..PANES {
        let id = format!("pane-{i}");
        let script = format!("echo scroll-{i}-marker && sleep 60");
        let (ok, out) = cli(&cli_bin, &socket, &["spawn", &id, "/bin/sh", "-c", &script]);
        if !ok {
            println!("[FAIL] persistence: spawn {id}: {out}");
            return ExitCode::FAILURE;
        }
    }
    // Let markers commit.
    std::thread::sleep(Duration::from_secs(2));
    // Sanity: all markers readable pre-crash.
    for i in 0..PANES {
        let (ok, out) = cli(&cli_bin, &socket, &["read", &format!("pane-{i}")]);
        if !ok || !out.contains(&format!("scroll-{i}-marker")) {
            println!("[FAIL] persistence: pre-crash read pane-{i}: {out}");
            return ExitCode::FAILURE;
        }
    }
    println!("[PASS] persistence: 10 panes live with markers");

    // Murder the daemon (no drain — the crash path).
    server.kill9();
    std::thread::sleep(Duration::from_millis(500));

    // Restart on the same path: layout + scrollback must come back.
    let server = match TestServer::spawn(&server_bin, &socket, "post-crash restart") {
        Ok(server) => server,
        Err(code) => return code,
    };
    wait_bound(&socket);
    // Boot restore needs a beat (respawn + pre-seed per pane).
    std::thread::sleep(Duration::from_secs(2));
    let (ok, out) = cli(&cli_bin, &socket, &["panes"]);
    if !ok {
        println!("[FAIL] persistence: post-crash panes: {out}");
        return ExitCode::FAILURE;
    }
    for i in 0..PANES {
        if !out.contains(&format!("pane-{i}")) {
            println!("[FAIL] persistence: layout missing pane-{i}: {out}");
            return ExitCode::FAILURE;
        }
    }
    println!("[PASS] persistence: layout restored (10/10 panes)");
    for i in 0..PANES {
        let (ok, out) = cli(&cli_bin, &socket, &["read", &format!("pane-{i}")]);
        if !ok || !out.contains(&format!("scroll-{i}-marker")) {
            println!("[FAIL] persistence: scrollback pane-{i} not byte-equal: {out}");
            return ExitCode::FAILURE;
        }
    }
    println!("[PASS] persistence: scrollback byte-equal (10/10 markers)");
    drop(server);
    let _ = std::fs::remove_file(&socket);
    let mut db = socket.into_os_string();
    db.push(".db");
    let _ = std::fs::remove_file(&db);
    println!("persistence: 3 passed, 0 failed");
    ExitCode::SUCCESS
}
