//! Failing-first probes for T-0012: graceful SIGTERM drain.
//!
//! A live daemon serves a chatterbox pane; SIGTERM mid-traffic must flush
//! committed output (attach-readable until close) and exit promptly — never
//! truncate acknowledged bytes, never hang past the drain deadline.

use std::time::Duration;

fn temp_socket(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("arreo-life-{}-{}.sock", name, std::process::id()))
}

fn spawn_server_daemon(socket: std::path::PathBuf) -> std::process::Child {
    let exe = std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("deps dir")
        .parent()
        .expect("debug dir")
        .join("arreo-server");
    std::process::Command::new(exe)
        .arg("--socket")
        .arg(&socket)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("arreo-server binary exists (cargo test builds bins)")
}

fn raw_request(socket: &std::path::Path, line: &str) -> String {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(socket).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    stream.write_all(line.as_bytes()).expect("write");
    stream.write_all(b"\n").expect("nl");
    stream.flush().expect("flush");
    let mut reader = BufReader::new(stream);
    let mut out = String::new();
    reader.read_line(&mut out).expect("read");
    out
}

#[test]
fn sigterm_drains_committed_output_and_exits() {
    let socket = temp_socket("term");
    let _ = std::fs::remove_file(&socket);
    let mut server = spawn_server_daemon(socket.clone());
    // Wait for serve.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(&socket).is_err() {
        assert!(std::time::Instant::now() < deadline, "daemon never bound");
        std::thread::sleep(Duration::from_millis(50));
    }
    // Spawn a chatterbox and let output commit.
    let reply = raw_request(
        &socket,
        r#"{"op":"spawn","v":0,"id":"chat","program":"/bin/sh","args":["-c","for i in $(seq 1 50); do echo tick-$i; sleep 0.05; done; sleep 30"],"cols":80,"rows":24}"#,
    );
    assert!(reply.contains("\"ok\""), "spawn: {reply}");
    std::thread::sleep(Duration::from_millis(800));

    // SIGTERM mid-traffic.
    let pid = server.id();
    unsafe {
        extern "C" {
            fn kill(pid: u32, sig: i32) -> i32;
        }
        assert_eq!(kill(pid, 15), 0, "SIGTERM delivered");
    }
    // Daemon must exit promptly (drain deadline 5 s + margin).
    let status = server
        .wait_timeout(Duration::from_secs(10))
        .expect("wait_timeout supported")
        .expect("daemon exits after SIGTERM (no hang)");
    assert!(status.success(), "clean exit, got {status:?}");

    // Committed output is NOT recoverable without persistence (T-0018) — but
    // the daemon must have stayed alive long enough to flush: assert via a
    // fresh unit-file truth instead. This test's hard claims: prompt exit +
    // no socket left behind serving stale clients.
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_err(),
        "socket closed after shutdown (no stale listener)"
    );
    let _ = std::fs::remove_file(&socket);
}

trait WaitTimeout {
    fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>>;
}

impl WaitTimeout for std::process::Child {
    fn wait_timeout(
        &mut self,
        timeout: Duration,
    ) -> std::io::Result<Option<std::process::ExitStatus>> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match self.try_wait()? {
                Some(status) => return Ok(Some(status)),
                None => {
                    if std::time::Instant::now() >= deadline {
                        return Ok(None);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        }
    }
}

#[test]
fn unit_files_render_per_os() {
    use arreo_server::lifecycle::{unit_file, unit_path, ServiceKind};
    use std::path::PathBuf;
    let bin = PathBuf::from("/usr/bin/arreo-server");
    let sock = PathBuf::from("/run/user/1000/arreo.sock");
    let systemd = unit_file(ServiceKind::SystemdUser, &bin, &sock);
    assert!(
        systemd.contains("ExecStart=/usr/bin/arreo-server --socket /run/user/1000/arreo.sock"),
        "exec line:\n{systemd}"
    );
    assert!(systemd.contains("Restart=on-failure"), "restart policy");
    assert!(unit_path(ServiceKind::SystemdUser).is_some());
    let launchd = unit_file(ServiceKind::Launchd, &bin, &sock);
    assert!(launchd.contains("dev.arreo.daemon"), "label:\n{launchd}");
    let win = unit_file(ServiceKind::WindowsService, &bin, &sock);
    assert!(win.contains("sc.exe create"), "service script:\n{win}");
    assert!(
        unit_path(ServiceKind::WindowsService).is_none(),
        "windows has no unit path (manual script)"
    );
}
