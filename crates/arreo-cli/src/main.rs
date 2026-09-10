//! `arreo` CLI binary. Verbs: `record` (T-0011), `metrics --pid` (T-0006),
//! daemon verbs `serve`-side client: `panes`, `spawn`, `attach`, `send` (T-0005),
//! lifecycle: `service`, `server` (T-0012).

use arreo_core::proto::codec;
use arreo_core::proto::{AgentState, Message, VERSION};
use std::future::Future;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

fn usage() -> ExitCode {
    eprintln!("usage:");
    eprintln!("  arreo --version");
    eprintln!("  arreo record <command> [args...] -o <fixture.pty> [--timeout-secs N]");
    eprintln!("  arreo replay <fixture.pty> [--speed N]");
    eprintln!("  arreo metrics --pid <PID> [--samples N]");
    eprintln!("  arreo panes [--socket PATH]");
    eprintln!("  arreo spawn <id> <program> [args...] [--socket PATH]");
    eprintln!("  arreo attach <id> [--socket PATH]   (stream output; Ctrl-C detaches, pane keeps running)");
    eprintln!("  arreo send <id> <text...> [--socket PATH]");
    eprintln!("  arreo read <id> [--from N] [--socket PATH]   (one-shot snapshot)");
    eprintln!("  arreo wait <id> --state <state> [--timeout 5m] [--socket PATH]");
    eprintln!("  arreo split <id> <new-id> [--socket PATH]");
    eprintln!("  arreo metrics <id> [--socket PATH]   (pane query; --pid <PID> samples locally)");
    eprintln!("  arreo service install|uninstall|status [--socket PATH]");
    eprintln!("  arreo server stop [--socket PATH]   (graceful: drain + exit 0)");
    eprintln!("  arreo audit [--limit N] [--socket PATH]   (append-only log, secrets redacted)");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    // NOTE: only argv[1] counts (chaos-found, T-0017: a global `.any()`
    // swallowed child args, so `arreo record X --version` printed OUR version
    // instead of recording the child's `--version` run).
    if args.get(1).is_some_and(|a| a == "--version" || a == "-V") {
        println!("arreo {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let verb = args.get(1).map(String::as_str);
    match verb {
        Some("record") => cmd_record(&args[2..]),
        Some("replay") => cmd_replay(&args[2..]),
        Some("metrics") => {
            if args[2..].iter().any(|a| a == "--pid") {
                cmd_metrics(&args[2..])
            } else {
                rt::block_on(cmd_pane_metrics(&args[2..]))
            }
        }
        Some("read") => rt::block_on(cmd_read(&args[2..])),
        Some("wait") => rt::block_on(cmd_wait(&args[2..])),
        Some("split") => rt::block_on(cmd_split(&args[2..])),
        Some("panes") => rt::block_on(cmd_panes(&args[2..])),
        Some("spawn") => rt::block_on(cmd_spawn(&args[2..])),
        Some("attach") => rt::block_on(cmd_attach(&args[2..])),
        Some("send") => rt::block_on(cmd_send(&args[2..])),
        Some("service") => cmd_service(&args[2..]),
        Some("server") => rt::block_on(cmd_server(&args[2..])),
        Some("audit") => cmd_audit(&args[2..]),
        _ => usage(),
    }
}

/// Minimal block_on (current-thread runtime: no extra threads for a CLI).
mod rt {
    use super::Future;
    use std::process::ExitCode;
    pub fn block_on<F: Future<Output = ExitCode>>(future: F) -> ExitCode {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map(|rt| rt.block_on(future))
            .unwrap_or_else(|e| {
                eprintln!("arreo: runtime failed: {e}");
                ExitCode::FAILURE
            })
    }
}

fn default_socket() -> PathBuf {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime).join("arreo.sock");
    }
    std::env::temp_dir().join(format!("arreo-{}.sock", unix_uid()))
}

#[cfg(unix)]
fn unix_uid() -> u32 {
    unsafe {
        extern "C" {
            fn getuid() -> u32;
        }
        getuid()
    }
}

#[cfg(not(unix))]
fn unix_uid() -> u32 {
    0
}

fn take_socket(rest: &[String]) -> (PathBuf, Vec<String>) {
    let mut socket = default_socket();
    let mut kept = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        if rest[i] == "--socket" && i + 1 < rest.len() {
            socket = PathBuf::from(&rest[i + 1]);
            i += 2;
        } else {
            kept.push(rest[i].clone());
            i += 1;
        }
    }
    (socket, kept)
}

fn cmd_record(rest: &[String]) -> ExitCode {
    // Parse: arreo record <cmd...> -o <path> [--timeout-secs N] [--allow-secrets]
    let mut output: Option<PathBuf> = None;
    let mut timeout_secs = 30u64;
    let mut allow_secrets = false;
    let mut command: Vec<String> = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "-o" | "--output" => {
                i += 1;
                if i >= rest.len() {
                    eprintln!("record: -o needs a path");
                    return ExitCode::from(2);
                }
                output = Some(PathBuf::from(&rest[i]));
            }
            "--timeout-secs" => {
                i += 1;
                timeout_secs = rest.get(i).and_then(|s| s.parse().ok()).unwrap_or(30);
            }
            "--allow-secrets" => allow_secrets = true,
            // `sh -c '...'` passthrough: -c is the child's flag, not ours.
            // Users writing `arreo record /bin/sh -c '...'` mean the shell's
            // -c; anything after the command position is child argv. Detect:
            // if we already have a command word, treat -c as child argv.
            flag if flag.starts_with('-') && command.is_empty() => {
                eprintln!("record: unknown flag {flag}");
                return ExitCode::from(2);
            }
            flag if flag.starts_with('-') => {
                command.push(flag.to_string());
            }
            "--" => {
                command.extend_from_slice(&rest[i + 1..]);
                break;
            }
            _ => command.push(rest[i].clone()),
        }
        i += 1;
    }
    let Some(path) = output else {
        eprintln!("record: missing -o <fixture.pty>");
        return ExitCode::from(2);
    };
    if command.is_empty() {
        eprintln!("record: missing <command>");
        return ExitCode::from(2);
    }
    let argv: Vec<&str> = command.iter().map(String::as_str).collect();
    let fixture =
        match arreo_core::fixtures::Fixture::record(&argv, Duration::from_secs(timeout_secs)) {
            Ok(fixture) => fixture,
            Err(e) => {
                eprintln!("record: {e}");
                return ExitCode::FAILURE;
            }
        };
    let findings = arreo_core::fixtures::scan_secrets(&fixture.text());
    if !findings.is_empty() && !allow_secrets {
        eprintln!("record: refusing to save — possible secrets detected:");
        for finding in &findings {
            eprintln!("  {finding}");
        }
        eprintln!("re-run with --allow-secrets to override (never commit secrets).");
        return ExitCode::FAILURE;
    }
    match fixture.save(&path) {
        Ok(()) => {
            println!(
                "recorded {} events ({} bytes) -> {}",
                fixture.events.len(),
                fixture.replay_accelerated().len(),
                path.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("record: save failed: {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_replay(rest: &[String]) -> ExitCode {
    let mut speed = f64::INFINITY;
    let mut path: Option<PathBuf> = None;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--speed" => {
                i += 1;
                speed = rest
                    .get(i)
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(f64::INFINITY);
            }
            flag if flag.starts_with('-') => {
                eprintln!("replay: unknown flag {flag}");
                return ExitCode::from(2);
            }
            _ => path = Some(PathBuf::from(&rest[i])),
        }
        i += 1;
    }
    let Some(path) = path else {
        eprintln!("replay: missing <fixture.pty>");
        return ExitCode::from(2);
    };
    let fixture = match arreo_core::fixtures::Fixture::load(&path) {
        Ok(fixture) => fixture,
        Err(e) => {
            eprintln!("replay: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Timed replay to stdout (demos); tests use replay_accelerated directly.
    let mut last = 0u64;
    for event in &fixture.events {
        if speed.is_finite() {
            let wait_ms = (event.t_ms.saturating_sub(last) as f64 / speed) as u64;
            if wait_ms > 0 {
                std::thread::sleep(Duration::from_millis(wait_ms));
            }
        }
        last = event.t_ms;
        if let Err(e) = std::io::Write::write_all(&mut std::io::stdout(), &event.bytes) {
            eprintln!("replay: {e}");
            return ExitCode::FAILURE;
        }
    }
    ExitCode::SUCCESS
}

/// `arreo metrics --pid <PID> [--samples N]`: live per-tree table.
/// The daemon-backed `arreo metrics <pane>` (socket query) lands with
/// T-0005/T-0012; this verb proves the sampler over real PIDs today.
fn cmd_metrics(rest: &[String]) -> ExitCode {
    let mut pid: Option<u32> = None;
    let mut samples = 3u32;
    let mut i = 0;
    while i < rest.len() {
        match rest[i].as_str() {
            "--pid" => {
                i += 1;
                pid = rest.get(i).and_then(|s| s.parse().ok());
            }
            "--samples" => {
                i += 1;
                samples = rest.get(i).and_then(|s| s.parse().ok()).unwrap_or(3).max(1);
            }
            flag => {
                eprintln!("metrics: unknown flag {flag} (want --pid <PID> [--samples N])");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }
    let Some(pid) = pid else {
        eprintln!("metrics: missing --pid <PID>");
        return ExitCode::from(2);
    };
    let mut sampler = arreo_core::metrics::Sampler::new();
    println!(
        "{:>8} {:>12} {:>8} {:>6}  CGROUP",
        "PID", "RSS", "CPU%", "PIDS"
    );
    for _ in 0..samples {
        match sampler.sample_tree(pid) {
            Ok(sample) => {
                let cpu = sample
                    .cpu_percent
                    .map(|c| format!("{c:.1}"))
                    .unwrap_or_else(|| "—".to_string());
                let cgroup = sample
                    .cgroup_bytes
                    .map(|b| format!("{}M", b / 1_048_576))
                    .unwrap_or_else(|| "—".to_string());
                println!(
                    "{pid:>8} {:>10}KiB {cpu:>8} {:>6}  {cgroup}",
                    sample.rss_bytes / 1024,
                    sample.pids.len(),
                );
            }
            Err(e) => {
                eprintln!("metrics: {e}");
                return ExitCode::FAILURE;
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    ExitCode::SUCCESS
}

/// Framed MessagePack connection with Hello→Welcome handshake.
struct Connection {
    reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    buf: Vec<u8>,
}

async fn open_connection(socket: &PathBuf) -> Result<Connection, String> {
    use tokio::io::AsyncWriteExt;
    let stream = tokio::net::UnixStream::connect(socket).await.map_err(|e| {
        format!(
            "cannot connect to {}: {e} (is arreo-server running?)",
            socket.display()
        )
    })?;
    let (reader, writer) = stream.into_split();
    let mut conn = Connection {
        reader: tokio::io::BufReader::new(reader),
        writer,
        buf: Vec::new(),
    };
    // Handshake: Hello → Welcome (or loud Error on version mismatch).
    let hello = Message::Hello {
        v: VERSION,
        client: "arreo-cli".to_string(),
        wants: vec![VERSION],
    };
    let frame = codec::encode_frame(&hello).map_err(|e| format!("encode: {e}"))?;
    conn.writer
        .write_all(&frame)
        .await
        .map_err(|e| format!("write: {e}"))?;
    conn.writer
        .flush()
        .await
        .map_err(|e| format!("flush: {e}"))?;
    match conn.recv().await? {
        Message::Welcome { .. } => Ok(conn),
        Message::Error { message, .. } => Err(format!("handshake: {message}")),
        other => Err(format!("handshake: unexpected {other:?}")),
    }
}

impl Connection {
    async fn send(&mut self, message: &Message) -> Result<(), String> {
        use tokio::io::AsyncWriteExt;
        let frame = codec::encode_frame(message).map_err(|e| format!("encode: {e}"))?;
        self.writer
            .write_all(&frame)
            .await
            .map_err(|e| format!("write: {e}"))?;
        self.writer.flush().await.map_err(|e| format!("flush: {e}"))
    }

    async fn recv(&mut self) -> Result<Message, String> {
        use tokio::io::AsyncReadExt;
        loop {
            if let Ok((message, consumed)) = codec::decode_frame(&self.buf) {
                self.buf.drain(..consumed);
                return Ok(message);
            }
            let mut chunk = [0u8; 8192];
            let n = self
                .reader
                .read(&mut chunk)
                .await
                .map_err(|e| format!("read: {e}"))?;
            if n == 0 {
                return Err("server closed connection".to_string());
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

async fn request(socket: &PathBuf, req: &Message) -> Result<Message, String> {
    let mut conn = open_connection(socket).await?;
    conn.send(req).await?;
    conn.recv().await
}

async fn cmd_panes(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    if !kept.is_empty() {
        eprintln!("panes: unexpected args {kept:?}");
        return ExitCode::from(2);
    }
    match request(
        &socket,
        &Message::Panes {
            v: VERSION,
            panes: vec![],
        },
    )
    .await
    {
        Ok(Message::Panes { panes, .. }) => {
            println!("{:>16}  STATE", "ID");
            for pane in panes {
                println!(
                    "{:>16}  {}",
                    pane.id,
                    if pane.alive { "alive" } else { "exited" }
                );
            }
            ExitCode::SUCCESS
        }
        Ok(Message::Error { message, .. }) => {
            eprintln!("panes: {message}");
            ExitCode::FAILURE
        }
        Ok(other) => {
            eprintln!("panes: unexpected {other:?}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("panes: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn cmd_spawn(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    if kept.len() < 2 {
        eprintln!("usage: arreo spawn <id> <program> [args...] [--socket PATH]");
        return ExitCode::from(2);
    }
    let req = Message::Spawn {
        v: VERSION,
        id: kept[0].clone(),
        program: kept[1].clone(),
        args: kept[2..].to_vec(),
        cols: 80,
        rows: 24,
        memory_max: None,
        pids_max: None,
        kill_on_breach: false,
    };
    match request(&socket, &req).await {
        Ok(Message::Ok { .. }) => {
            println!("spawned {}", kept[0]);
            ExitCode::SUCCESS
        }
        Ok(Message::Error { message, .. }) => {
            eprintln!("spawn: {message}");
            ExitCode::FAILURE
        }
        Ok(other) => {
            eprintln!("spawn: unexpected {other:?}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("spawn: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn cmd_send(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    if kept.len() < 2 {
        eprintln!("usage: arreo send <id> <text...> [--socket PATH]");
        return ExitCode::from(2);
    }
    let req = Message::Send {
        v: VERSION,
        id: kept[0].clone(),
        data: kept[1..].join(" "),
    };
    match request(&socket, &req).await {
        Ok(Message::Ok { .. }) => ExitCode::SUCCESS,
        Ok(Message::Error { message, .. }) => {
            eprintln!("send: {message}");
            ExitCode::FAILURE
        }
        Ok(other) => {
            eprintln!("send: unexpected {other:?}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("send: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Attach: stream append-deltas to stdout until the pane exits (then exit 0)
/// or the connection breaks. Ctrl-C detaches (exit 0) — the pane keeps
/// running on the daemon. No full repaints: only NEW lines print (v0 delta).
async fn cmd_attach(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    if kept.len() != 1 {
        eprintln!("usage: arreo attach <id> [--socket PATH]");
        return ExitCode::from(2);
    }
    let id = kept[0].clone();
    let mut conn = match open_connection(&socket).await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("attach: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = conn
        .send(&Message::Attach {
            v: VERSION,
            id: id.clone(),
            from_line: 0,
        })
        .await
    {
        eprintln!("attach: {e}");
        return ExitCode::FAILURE;
    }
    loop {
        match conn.recv().await {
            Ok(Message::Delta { lines, .. }) | Ok(Message::Snapshot { lines, .. }) => {
                for text in lines {
                    println!("{text}");
                }
            }
            Ok(Message::Exited { code, .. }) => {
                eprintln!("attach: pane exited (code {code:?})");
                return ExitCode::SUCCESS;
            }
            Ok(Message::Error { message, .. }) => {
                eprintln!("attach: {message}");
                return ExitCode::FAILURE;
            }
            Ok(other) => {
                eprintln!("attach: unexpected {other:?}");
                return ExitCode::FAILURE;
            }
            Err(e) => {
                eprintln!("attach: connection lost: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
}

/// `arreo service install|uninstall|status`: manage the OS service unit.
/// Install writes the unit file for the native manager and enables it;
/// uninstall reverses fully; status reports manager + unit state.
fn cmd_service(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    let action = kept.first().map(String::as_str).unwrap_or("");
    let kind = arreo_core::lifecycle::ServiceKind::native();
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("arreo-server")))
        .unwrap_or_else(|| PathBuf::from("arreo-server"));
    match action {
        "install" => {
            let unit = arreo_core::lifecycle::unit_file(kind, &exe, &socket);
            match arreo_core::lifecycle::unit_path(kind) {
                Some(path) => {
                    if let Some(parent) = path.parent() {
                        if let Err(e) = std::fs::create_dir_all(parent) {
                            eprintln!("service install: {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                    if let Err(e) = std::fs::write(&path, &unit) {
                        eprintln!("service install: {e}");
                        return ExitCode::FAILURE;
                    }
                    println!("wrote {}", path.display());
                    enable_service(kind);
                    ExitCode::SUCCESS
                }
                None => {
                    println!("{unit}");
                    eprintln!("service install: no unit path on this OS — run the printed script as Administrator");
                    ExitCode::SUCCESS
                }
            }
        }
        "uninstall" => match arreo_core::lifecycle::unit_path(kind) {
            Some(path) => {
                disable_service(kind);
                match std::fs::remove_file(&path) {
                    Ok(()) => {
                        println!("removed {}", path.display());
                        ExitCode::SUCCESS
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        println!("not installed");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("service uninstall: {e}");
                        ExitCode::FAILURE
                    }
                }
            }
            None => {
                eprintln!("service uninstall: manual on this OS (see install output)");
                ExitCode::from(2)
            }
        },
        "status" => {
            let path = arreo_core::lifecycle::unit_path(kind);
            let installed = path.as_ref().is_some_and(|p| p.exists());
            println!("manager: {kind:?}");
            println!(
                "unit: {}",
                path.map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(manual)".to_string())
            );
            println!("installed: {installed}");
            println!(
                "socket: {} ({})",
                socket.display(),
                if socket.exists() { "present" } else { "absent" }
            );
            ExitCode::SUCCESS
        }
        _ => {
            eprintln!("usage: arreo service install|uninstall|status [--socket PATH]");
            ExitCode::from(2)
        }
    }
}

fn enable_service(kind: arreo_core::lifecycle::ServiceKind) {
    use arreo_core::lifecycle::ServiceKind as Kind;
    let result = match kind {
        Kind::SystemdUser => std::process::Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output()
            .and_then(|_| {
                std::process::Command::new("systemctl")
                    .args(["--user", "enable", "--now", "arreo.service"])
                    .output()
            })
            .map(|_| ()),
        Kind::Launchd => {
            eprintln!("service install: plist written — load with: launchctl load -w <path>");
            Ok(())
        }
        Kind::WindowsService => {
            eprintln!("service install: script printed — run as Administrator (see above)");
            Ok(())
        }
    };
    match result {
        Ok(()) => println!("service enabled"),
        Err(e) => {
            eprintln!("service install: unit written but enable failed: {e} (enable manually)")
        }
    }
}

fn disable_service(kind: arreo_core::lifecycle::ServiceKind) {
    use arreo_core::lifecycle::ServiceKind as Kind;
    if let Kind::SystemdUser = kind {
        let _ = std::process::Command::new("systemctl")
            .args(["--user", "disable", "--now", "arreo.service"])
            .output();
    }
    // launchd/Windows: unloading needs the exact path/user context — the
    // unit file removal above is the reversal; document, don't guess.
}

/// `arreo server stop`: graceful shutdown via SIGTERM (the daemon drains +
/// exits 0). Finds the daemon by socket presence; refuses when absent.
async fn cmd_server(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    if kept.first().map(String::as_str) != Some("stop") {
        eprintln!("usage: arreo server stop [--socket PATH]");
        return ExitCode::from(2);
    }
    if !socket.exists() {
        eprintln!(
            "server stop: no socket at {} (daemon not running?)",
            socket.display()
        );
        return ExitCode::FAILURE;
    }
    // SIGTERM the daemon: resolve its PID via `panes` liveness, else fall
    // back to pkill by socket path. Simplest robust path: find arreo-server
    // processes whose command line names our socket.
    let pid = find_daemon_pid(&socket).await;
    match pid {
        Some(pid) => {
            unsafe {
                extern "C" {
                    fn kill(pid: u32, sig: i32) -> i32;
                }
                if kill(pid, 15) != 0 {
                    eprintln!("server stop: SIGTERM to {pid} failed");
                    return ExitCode::FAILURE;
                }
            }
            // Wait for the socket to close (drain + exit), up to 10 s.
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while socket.exists() && std::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            // Socket file removal is the daemon's last act; absence proves it.
            // (A stale file with no listener also passes — serve() treats it
            // the same on next start.)
            println!("server stopped (pid {pid})");
            ExitCode::SUCCESS
        }
        None => {
            eprintln!("server stop: socket exists but no live daemon found (stale file?)");
            ExitCode::FAILURE
        }
    }
}

/// Find the daemon PID by probing: connect + ask `panes` — any answer proves
/// liveness; the PID itself comes from a `server pid` lookup via /proc scan
/// for `arreo-server --socket <path>`.
async fn find_daemon_pid(socket: &PathBuf) -> Option<u32> {
    let want = socket.to_string_lossy().to_string();
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.filter_map(|e| e.ok()) {
        // NOTE: `continue`, never `?` — /proc holds non-numeric entries and
        // unreadable PIDs; either must skip, not abort the whole scan
        // (chaos-found: `?` here returned None on the first non-PID entry).
        let pid: u32 = match entry.file_name().to_string_lossy().parse() {
            Ok(pid) => pid,
            Err(_) => continue,
        };
        let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(cmdline) => cmdline,
            Err(_) => continue,
        };
        let parts: Vec<&str> = cmdline
            .split(|b| *b == 0)
            .filter_map(|s| std::str::from_utf8(s).ok())
            .collect();
        if parts.iter().any(|p| p.ends_with("arreo-server")) && parts.iter().any(|p| *p == want) {
            // Confirm it answers (not a zombie holding the path).
            if tokio::net::UnixStream::connect(socket).await.is_ok() {
                return Some(pid);
            }
        }
    }
    None
}

/// `arreo read <id> [--from N]`: one-shot snapshot of current pane text.
async fn cmd_read(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    let mut from_line = 0usize;
    let mut id: Option<String> = None;
    let mut i = 0;
    while i < kept.len() {
        if kept[i] == "--from" && i + 1 < kept.len() {
            from_line = kept[i + 1].parse().unwrap_or(0);
            i += 2;
        } else if id.is_none() {
            id = Some(kept[i].clone());
            i += 1;
        } else {
            eprintln!("usage: arreo read <id> [--from N] [--socket PATH]");
            return ExitCode::from(2);
        }
    }
    let Some(id) = id else {
        eprintln!("usage: arreo read <id> [--from N] [--socket PATH]");
        return ExitCode::from(2);
    };
    match request(
        &socket,
        &Message::Read {
            v: VERSION,
            id,
            from_line,
        },
    )
    .await
    {
        Ok(Message::Delta { lines, .. }) | Ok(Message::Snapshot { lines, .. }) => {
            for text in lines {
                println!("{text}");
            }
            ExitCode::SUCCESS
        }
        Ok(Message::Error { message, .. }) => {
            eprintln!("read: {message}");
            ExitCode::FAILURE
        }
        Ok(other) => {
            eprintln!("read: unexpected {other:?}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("read: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `arreo wait <id> --state <state> [--timeout <dur>]`: block until the pane
/// reaches a state. Duration syntax: `500ms`, `30s`, `5m` (default `5m`).
/// Exit 0 on match (prints the StateEvent), 1 on timeout/error.
async fn cmd_wait(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    let mut id: Option<String> = None;
    let mut state: Option<String> = None;
    let mut timeout_ms = 5 * 60 * 1000u64;
    let mut i = 0;
    while i < kept.len() {
        match kept[i].as_str() {
            "--state" if i + 1 < kept.len() => {
                state = Some(kept[i + 1].clone());
                i += 2;
            }
            "--timeout" if i + 1 < kept.len() => {
                timeout_ms = match parse_duration_ms(&kept[i + 1]) {
                    Some(ms) => ms,
                    None => {
                        eprintln!(
                            "wait: bad --timeout {:?} (want 500ms, 30s, 5m)",
                            kept[i + 1]
                        );
                        return ExitCode::from(2);
                    }
                };
                i += 2;
            }
            other if id.is_none() => {
                id = Some(other.to_string());
                i += 1;
            }
            _ => {
                eprintln!("usage: arreo wait <id> --state <state> [--timeout 5m] [--socket PATH]");
                return ExitCode::from(2);
            }
        }
    }
    let (Some(id), Some(state)) = (id, state) else {
        eprintln!("usage: arreo wait <id> --state <state> [--timeout 5m] [--socket PATH]");
        return ExitCode::from(2);
    };
    let want = match state.to_lowercase().as_str() {
        "working" => AgentState::Working,
        "idle" => AgentState::Idle,
        "question" => AgentState::Question,
        "blocked" => AgentState::Blocked,
        "done" => AgentState::Done,
        "unknown" => AgentState::Unknown,
        _ => {
            eprintln!(
                "wait: bad state {state:?} (want working|idle|question|blocked|done|unknown)"
            );
            return ExitCode::from(2);
        }
    };
    match request(
        &socket,
        &Message::Wait {
            v: VERSION,
            id,
            state: want,
            timeout_ms,
        },
    )
    .await
    {
        Ok(Message::StateEvent {
            state,
            confidence,
            matched_pattern,
            ..
        }) => {
            println!("state={state:?} confidence={confidence} pattern={matched_pattern:?}");
            ExitCode::SUCCESS
        }
        Ok(Message::Error { message, .. }) => {
            eprintln!("wait: {message}");
            ExitCode::FAILURE
        }
        Ok(other) => {
            eprintln!("wait: unexpected {other:?}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("wait: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse_duration_ms(text: &str) -> Option<u64> {
    if let Some(ms) = text.strip_suffix("ms") {
        return ms.parse().ok();
    }
    if let Some(s) = text.strip_suffix('s') {
        return s.parse::<u64>().ok().map(|s| s * 1000);
    }
    if let Some(m) = text.strip_suffix('m') {
        return m.parse::<u64>().ok().map(|m| m * 60 * 1000);
    }
    None
}

/// `arreo split <id> <new-id>`: spawn a sibling pane with the same program.
async fn cmd_split(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    if kept.len() != 2 {
        eprintln!("usage: arreo split <id> <new-id> [--socket PATH]");
        return ExitCode::from(2);
    }
    match request(
        &socket,
        &Message::Split {
            v: VERSION,
            id: kept[0].clone(),
            new_id: kept[1].clone(),
            cols: 80,
            rows: 24,
        },
    )
    .await
    {
        Ok(Message::Ok { .. }) => {
            println!("split {} -> {}", kept[0], kept[1]);
            ExitCode::SUCCESS
        }
        Ok(Message::Error { message, .. }) => {
            eprintln!("split: {message}");
            ExitCode::FAILURE
        }
        Ok(other) => {
            eprintln!("split: unexpected {other:?}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("split: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `arreo metrics <id>`: resource truth for one pane's tree (daemon query;
/// the local `--pid` sampler from T-0006 stays for pid-level inspection).
async fn cmd_pane_metrics(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    if kept.len() != 1 {
        eprintln!("usage: arreo metrics <id> [--socket PATH]  (pane query; use --pid <PID> for local sampling)");
        return ExitCode::from(2);
    }
    // `--pid` still routes to the local sampler (T-0006 verb preserved).
    if kept[0] == "--pid" {
        eprintln!("usage: arreo metrics --pid <PID> [--samples N]  (local sampler)");
        return ExitCode::from(2);
    }
    match request(
        &socket,
        &Message::MetricsReq {
            v: VERSION,
            id: kept[0].clone(),
        },
    )
    .await
    {
        Ok(Message::Metrics {
            rss_bytes,
            cpu_percent,
            pids,
            ..
        }) => {
            println!(
                "rss={}KiB cpu={} pids={}",
                rss_bytes / 1024,
                cpu_percent
                    .map(|c| format!("{c:.1}%"))
                    .unwrap_or_else(|| "—".to_string()),
                pids
            );
            ExitCode::SUCCESS
        }
        Ok(Message::Error { message, .. }) => {
            eprintln!("metrics: {message}");
            ExitCode::FAILURE
        }
        Ok(other) => {
            eprintln!("metrics: unexpected {other:?}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("metrics: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `arreo audit [--limit N]`: print the append-only audit log (newest last).
/// Reads the sidecar DB directly (no daemon round-trip — the log outlives
/// the daemon by design). Secrets are already redacted at write time.
fn cmd_audit(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    let mut limit = 50usize;
    let mut i = 0;
    while i < kept.len() {
        match kept[i].as_str() {
            "--limit" if i + 1 < kept.len() => {
                limit = kept[i + 1].parse().unwrap_or(50).max(1);
                i += 2;
            }
            _ => {
                eprintln!("usage: arreo audit [--limit N] [--socket PATH]");
                return ExitCode::from(2);
            }
        }
    }
    let mut db = socket.into_os_string();
    db.push(".db");
    let db = PathBuf::from(db);
    if !db.exists() {
        eprintln!("audit: no log yet (no prompts sent through this daemon)");
        return ExitCode::SUCCESS;
    }
    let store = match arreo_core::store::SessionStore::open(&db) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("audit: {e}");
            return ExitCode::FAILURE;
        }
    };
    match store.audit_recent(limit) {
        Ok(events) => {
            for event in events.iter().rev() {
                println!(
                    "{} {} {} {}{}",
                    event.ts_ms,
                    event.device,
                    event.agent,
                    if event.redacted { "[redacted] " } else { "" },
                    event.prompt.lines().next().unwrap_or("")
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("audit: {e}");
            ExitCode::FAILURE
        }
    }
}
