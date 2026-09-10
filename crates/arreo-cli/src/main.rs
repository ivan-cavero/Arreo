//! `arreo` CLI binary. Verbs: `record` (T-0011), `metrics --pid` (T-0006),
//! daemon verbs `serve`-side client: `panes`, `spawn`, `attach`, `send` (T-0005).

use arreo_core::proto::{Request, Response};
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
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("arreo {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let verb = args.get(1).map(String::as_str);
    match verb {
        Some("record") => cmd_record(&args[2..]),
        Some("replay") => cmd_replay(&args[2..]),
        Some("metrics") => cmd_metrics(&args[2..]),
        Some("panes") => rt::block_on(cmd_panes(&args[2..])),
        Some("spawn") => rt::block_on(cmd_spawn(&args[2..])),
        Some("attach") => rt::block_on(cmd_attach(&args[2..])),
        Some("send") => rt::block_on(cmd_send(&args[2..])),
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

async fn open_connection(
    socket: &PathBuf,
) -> Result<
    (
        tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
        tokio::net::unix::OwnedWriteHalf,
    ),
    String,
> {
    let stream = tokio::net::UnixStream::connect(socket).await.map_err(|e| {
        format!(
            "cannot connect to {}: {e} (is arreo-server running?)",
            socket.display()
        )
    })?;
    let (reader, writer) = stream.into_split();
    Ok((tokio::io::BufReader::new(reader), writer))
}

async fn request(socket: &PathBuf, req: &Request) -> Result<Response, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let (mut reader, mut writer) = open_connection(socket).await?;
    let mut line = serde_json::to_string(req).map_err(|e| format!("encode: {e}"))?;
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("write: {e}"))?;
    writer.flush().await.map_err(|e| format!("flush: {e}"))?;
    let mut out = String::new();
    reader
        .read_line(&mut out)
        .await
        .map_err(|e| format!("read: {e}"))?;
    serde_json::from_str(&out).map_err(|e| format!("decode: {e}"))
}

async fn cmd_panes(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    if !kept.is_empty() {
        eprintln!("panes: unexpected args {kept:?}");
        return ExitCode::from(2);
    }
    match request(&socket, &Request::List { v: 0 }).await {
        Ok(Response::Panes { panes, .. }) => {
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
        Ok(Response::Error { message, .. }) => {
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
    let req = Request::Spawn {
        v: 0,
        id: kept[0].clone(),
        program: kept[1].clone(),
        args: kept[2..].to_vec(),
        cols: 80,
        rows: 24,
    };
    match request(&socket, &req).await {
        Ok(Response::Ok { .. }) => {
            println!("spawned {}", kept[0]);
            ExitCode::SUCCESS
        }
        Ok(Response::Error { message, .. }) => {
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
    let req = Request::Send {
        v: 0,
        id: kept[0].clone(),
        data: kept[1..].join(" "),
    };
    match request(&socket, &req).await {
        Ok(Response::Ok { .. }) => ExitCode::SUCCESS,
        Ok(Response::Error { message, .. }) => {
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
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let (socket, kept) = take_socket(rest);
    if kept.len() != 1 {
        eprintln!("usage: arreo attach <id> [--socket PATH]");
        return ExitCode::from(2);
    }
    let id = kept[0].clone();
    let (mut reader, mut writer) = match open_connection(&socket).await {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("attach: {e}");
            return ExitCode::FAILURE;
        }
    };
    let req = Request::Attach {
        v: 0,
        id: id.clone(),
        from_line: 0,
    };
    let mut line = serde_json::to_string(&req).unwrap();
    line.push('\n');
    if let Err(e) = writer.write_all(line.as_bytes()).await {
        eprintln!("attach: {e}");
        return ExitCode::FAILURE;
    }
    let _ = writer.flush().await;
    loop {
        let mut out = String::new();
        match reader.read_line(&mut out).await {
            Ok(0) => return ExitCode::SUCCESS,
            Ok(_) => {}
            Err(e) => {
                eprintln!("attach: connection lost: {e}");
                return ExitCode::FAILURE;
            }
        }
        let response: Response = match serde_json::from_str(&out) {
            Ok(response) => response,
            Err(e) => {
                eprintln!("attach: bad frame: {e}");
                return ExitCode::FAILURE;
            }
        };
        match response {
            Response::Output { lines, .. } => {
                for text in lines {
                    println!("{text}");
                }
            }
            Response::Exited { code, .. } => {
                eprintln!("attach: pane exited (code {code:?})");
                return ExitCode::SUCCESS;
            }
            Response::Error { message, .. } => {
                eprintln!("attach: {message}");
                return ExitCode::FAILURE;
            }
            Response::Ok { .. } | Response::Panes { .. } => {}
        }
    }
}
