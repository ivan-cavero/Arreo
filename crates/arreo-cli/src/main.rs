//! `arreo` CLI binary. Verbs: `record` (T-0011), `metrics --pid` (T-0006),
//! daemon verbs `serve`-side client: `panes`, `spawn`, `attach`, `send` (T-0005),
//! lifecycle: `service`, `server` (T-0012).

use arreo_core::proto::codec;
use arreo_core::proto::{AgentState, Message, VERSION};
use arreo_core::store::{audit_json, AuditQuery, ExportFormat, SessionStore, StoredAudit};
use std::future::Future;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

/// Restore the default SIGPIPE disposition (T-0049).
///
/// Rust ignores SIGPIPE process-wide, so a write to a closed pipe fails with
/// EPIPE — and `println!` panics on that (exit 101 + `panicked at` text).
/// Every Unix tool a pipeline composes (`head`, `less`, a harness that stops
/// reading) closes the pipe, so the CLI must die by signal (exit 141, silent)
/// instead. `unsafe` is confined here: `signal(2)` with `SIG_DFL` cannot
/// violate memory safety (it installs no handler, runs no code), and the
/// alternative — a `libc` dependency for one call — is scaffolding. No-op on
/// non-Unix (Windows has no SIGPIPE; `println!` there fails closed already).
#[cfg(unix)]
fn restore_default_sigpipe() {
    unsafe {
        const SIGPIPE: i32 = 13;
        const SIG_DFL: usize = 0;
        extern "C" {
            fn signal(signum: i32, handler: usize) -> usize;
        }
        signal(SIGPIPE, SIG_DFL);
    }
}

#[cfg(not(unix))]
fn restore_default_sigpipe() {}

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
    eprintln!("  arreo metrics history <pane> --since 6h --step 1m [--json] [--socket PATH]");
    eprintln!("  arreo service install|uninstall|status [--socket PATH]");
    eprintln!("  arreo server stop [--socket PATH]   (graceful: drain + exit 0)");
    eprintln!(
        "  arreo audit [--limit N] [--json] [--socket PATH]   (append-only log, secrets redacted)"
    );
    eprintln!(
        "      columns: ts_ms action outcome device agent peer detail prompt   (tail, newest last)"
    );
    eprintln!("  arreo audit export [--format jsonl|json] [--since MS] [--until MS] [--action NAME] [--out PATH|-]");
    eprintln!("      MS is Unix milliseconds; --out - (the default) is stdout");
    eprintln!("  arreo audit prune --before MS   (never automatic; says how many rows went)");
    eprintln!("  arreo devices <id|list|issue|rotate|revoke|authorize> [--json] [--socket PATH]");
    eprintln!("      list --revoked|--all   (live devices by default; tombstones with --revoked)");
    eprintln!("      revoke <name|id>       (idempotent; the audit row names who and when)");
    eprintln!("  arreo pair [--role owner|viewer] [--ttl-secs N] [--mailbox ADDR] [--config PATH] [--json]");
    eprintln!(
        "      show a code; pins the device that types it. With a [relay] section configured, the"
    );
    eprintln!("      invite also names the account and relay, so `arreo machines add` can register the new");
    eprintln!("  arreo pair --join \"four words\" --uri arreo://pair?... [--name N] [--json]   (this device joins)");
    eprintln!("  arreo machines list [--json] [--all] [--offline] [--config PATH]");
    eprintln!("  arreo machines status [<name>] [--json] [--offline]   (0 ok, 2 usage, 3 unknown machine, 4 relay unreachable, 5 conflict)");
    eprintln!(
        "      --json is the script contract (schema 1); the human table is not one and may change"
    );
    eprintln!("      authorize --verb <read|send|...>   (the transport's own decision path)");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    // Broken-pipe discipline (T-0049), decided once here rather than at 245
    // `println!` sites: restore the default SIGPIPE disposition so the process
    // dies the way every other Unix tool does when its consumer goes away
    // (`head -1` → SIGPIPE → exit 141, no panic text), instead of Rust's
    // default of ignoring SIGPIPE and failing the write with EPIPE — which
    // `println!` turns into `panicked at 'failed printing to stdout'` + exit
    // 101. Why this over a Result-routing writer: one `unsafe` block at the
    // single entry point versus touching every print site and auditing that no
    // future `println!` reintroduces the panic; the disposition is
    // process-wide, which is exactly the scope of the problem (every verb
    // prints). No new dependency (`libc` for one call is scaffolding); the
    // raw `signal(2)` binding is three lines with its contract stated.
    restore_default_sigpipe();
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
            if args.get(2).map(String::as_str) == Some("history") {
                rt::block_on(cmd_metrics_history(&args[3..]))
            } else if args[2..].iter().any(|a| a == "--pid") {
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
        Some("devices") => cmd_devices(&args[2..]),
        Some("pair") => cmd_pair(&args[2..]),
        Some("machines") => machines::run(&args[2..]),
        _ => usage(),
    }
}

mod machines;

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
            // Attention first (T-0041): alerting panes sort ahead of merely
            // working ones, so a script can page on the listing. The alert
            // column is empty when no episode fired — absent, never a lie.
            let mut panes = panes;
            panes.sort_by(|a, b| {
                a.alert
                    .is_none()
                    .cmp(&b.alert.is_none())
                    .then(a.id.cmp(&b.id))
            });
            println!("{:>16}  {:<7}  ALERT", "ID", "STATE");
            for pane in panes {
                println!(
                    "{:>16}  {:<7}  {}",
                    pane.id,
                    if pane.alive { "alive" } else { "exited" },
                    pane.alert.as_deref().unwrap_or(""),
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

/// `arreo metrics history <pane> --since 6h --step 1m [--json]`: the durable
/// series (T-0040), not the live sample.
///
/// Durations accept `s`/`m`/`h`/`d` suffixes (`6h`, `30d`, `90s`) or bare
/// milliseconds. `--step` asks a tier; the server downshifts to the nearest
/// real step when the ask is finer than available and says so, so `--step 1s`
/// over 6 h reports 10 s rows with a note rather than an empty series. An
/// unknown pane gives an empty series plus a clear message, not an error.
async fn cmd_metrics_history(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    let mut pane: Option<String> = None;
    let mut since_ms: Option<u64> = None;
    let mut step_ms: u64 = 0;
    let mut json = false;
    let mut i = 0;
    while i < kept.len() {
        match kept[i].as_str() {
            "--since" => {
                i += 1;
                match kept.get(i).map(|s| parse_history_arg("--since", s)) {
                    Some(Ok(ms)) => since_ms = Some(ms),
                    Some(Err(code)) => return code,
                    None => {
                        eprintln!("metrics history: --since wants a duration (e.g. 6h)");
                        return ExitCode::from(2);
                    }
                }
            }
            "--step" => {
                i += 1;
                match kept.get(i).map(|s| parse_history_arg("--step", s)) {
                    Some(Ok(ms)) => step_ms = ms,
                    Some(Err(code)) => return code,
                    None => {
                        eprintln!("metrics history: --step wants a duration (e.g. 1m)");
                        return ExitCode::from(2);
                    }
                }
            }
            "--json" => json = true,
            other if !other.starts_with("--") && pane.is_none() => pane = Some(other.to_string()),
            other => {
                eprintln!("metrics history: unknown argument {other:?}");
                eprintln!("usage: arreo metrics history <pane> --since 6h --step 1m [--json] [--socket PATH]");
                return ExitCode::from(2);
            }
        }
        i += 1;
    }
    let Some(pane) = pane else {
        eprintln!(
            "usage: arreo metrics history <pane> --since 6h --step 1m [--json] [--socket PATH]"
        );
        return ExitCode::from(2);
    };
    let Some(since) = since_ms else {
        eprintln!("metrics history: --since is required (e.g. --since 6h)");
        return ExitCode::from(2);
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    match request(
        &socket,
        &Message::MetricsHistory {
            v: VERSION,
            id: pane.clone(),
            since_ms: now_ms.saturating_sub(since),
            until_ms: u64::MAX,
            step_ms,
        },
    )
    .await
    {
        Ok(Message::MetricsSeries {
            step_ms: got,
            downshifted,
            rows,
            ..
        }) => {
            if rows.is_empty() {
                println!("no history for {pane:?} in this window");
                return ExitCode::SUCCESS;
            }
            if downshifted {
                eprintln!(
                    "note: showing {} rows (nearest real step to the ask)",
                    render_duration(got)
                );
            }
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "pane": pane,
                        "step_ms": got,
                        "downshifted": downshifted,
                        "rows": rows.iter().map(|row| serde_json::json!({
                            "ts_ms": row.ts_ms,
                            "rss_avg": row.rss_avg,
                            "rss_peak": row.rss_peak,
                            "cpu_avg": row.cpu_avg,
                            "cpu_peak": row.cpu_peak,
                            "pids": row.pids,
                        })).collect::<Vec<_>>(),
                    })
                );
            } else {
                println!("ts_ms rss_avg rss_peak cpu_avg cpu_peak pids");
                for row in &rows {
                    println!(
                        "{} {} {} {:.1} {:.1} {}",
                        row.ts_ms,
                        row.rss_avg / 1024,
                        row.rss_peak / 1024,
                        row.cpu_avg,
                        row.cpu_peak,
                        row.pids
                    );
                }
            }
            ExitCode::SUCCESS
        }
        Ok(Message::Error { message, .. }) => {
            eprintln!("metrics history: {message}");
            ExitCode::FAILURE
        }
        Ok(other) => {
            eprintln!("metrics history: unexpected {other:?}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("metrics history: {e}");
            ExitCode::FAILURE
        }
    }
}

/// A duration for `--since`/`--step`: `90s`, `6h`, `30d`, or bare milliseconds.
/// Non-numeric is a usage error, never a silent default — a typo'd window that
/// silently means "everything" is how a query quietly stops being the window
/// someone asked for.
fn parse_history_arg(flag: &str, value: &str) -> Result<u64, ExitCode> {
    let (digits, factor) = match value.strip_suffix(['s', 'm', 'h', 'd']) {
        Some(_) if value.len() > 1 => {
            let (num, suffix) = value.split_at(value.len() - 1);
            let factor = match suffix {
                "s" => 1_000u64,
                "m" => 60_000,
                "h" => 3_600_000,
                "d" => 86_400_000,
                _ => unreachable!("stripped above"),
            };
            (num, factor)
        }
        _ => (value, 1),
    };
    match digits.parse::<u64>() {
        Ok(n) => Ok(n.saturating_mul(factor)),
        Err(_) => {
            eprintln!("metrics history: {flag} wants a duration (e.g. 6h), got {value:?}");
            Err(ExitCode::from(2))
        }
    }
}

/// Render a step back into the duration spelling the CLI accepts.
fn render_duration(step_ms: u64) -> String {
    if step_ms.is_multiple_of(86_400_000) {
        format!("{}d", step_ms / 86_400_000)
    } else if step_ms.is_multiple_of(3_600_000) {
        format!("{}h", step_ms / 3_600_000)
    } else if step_ms.is_multiple_of(60_000) {
        format!("{}m", step_ms / 60_000)
    } else if step_ms.is_multiple_of(1_000) {
        format!("{}s", step_ms / 1_000)
    } else {
        format!("{step_ms}ms")
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

/// `arreo audit [--limit N] [--json]`: print the append-only audit log (newest
/// last). Reads the sidecar DB directly (no daemon round-trip — the log outlives
/// the daemon by design). Secrets are already redacted at write time.
///
/// Subcommands (T-0033):
///   `export` — a filtered window as jsonl/json, to stdout or a file
///   `prune`  — delete rows older than an explicit bound (never automatic)
fn cmd_audit(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    let mut limit = 50usize;
    let mut json = false;
    let mut i = 0;
    while i < kept.len() {
        match kept[i].as_str() {
            "--limit" if i + 1 < kept.len() => {
                limit = match kept[i + 1].parse() {
                    Ok(n) => n,
                    Err(_) => {
                        eprintln!("audit: --limit wants a number, got {:?}", kept[i + 1]);
                        return ExitCode::from(2);
                    }
                };
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            // A subcommand reads the same store, so it is dispatched only after
            // `--socket` has been peeled off above.
            "export" => return audit::cmd_export(socket, &kept[i + 1..]),
            "prune" => return audit::cmd_prune(socket, &kept[i + 1..]),
            other => {
                eprintln!("audit: unknown argument {other:?}");
                eprintln!("usage: arreo audit [--limit N] [--json] [--socket PATH]");
                eprintln!("       arreo audit export [--format jsonl|json] [--since MS] [--until MS] [--action NAME] [--out PATH|-] [--socket PATH]");
                eprintln!("       arreo audit prune --before MS [--socket PATH]");
                return ExitCode::from(2);
            }
        }
    }
    let store = match audit::open(&socket) {
        Ok(Some(store)) => store,
        Ok(None) => {
            // No log yet is an empty log, not a broken one: the message is on
            // stderr, and `--json` still answers in the shape it promises.
            if json {
                println!("{}", audit::json_object(&[]));
            }
            return ExitCode::SUCCESS;
        }
        Err(code) => return code,
    };
    if json {
        return audit::print_json(&store, limit);
    }
    match store.audit_recent(limit) {
        Ok(events) => {
            for event in events.iter().rev() {
                // The *action* is what an operator greps for (`device.revoke`),
                // and it replaced the coarser `kind` column here (T-0033): a
                // revocation printed as "device_change" is how T-0024 lost the
                // event kind and how T-0026 lost the action — a row that is
                // written but not rendered is a row nobody can act on.
                println!(
                    "{} {:<22} {:<8} {:<16} {:<12} {:<18} {:<24} {}{}",
                    event.ts_ms,
                    event.action,
                    event.outcome.as_str(),
                    event.device,
                    event.agent,
                    event.peer.as_deref().unwrap_or("-"),
                    event.detail.as_deref().unwrap_or("-"),
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

/// The `arreo audit` subcommands that read or trim the same sidecar log.
mod audit {
    use super::{audit_json, AuditQuery, ExportFormat, SessionStore, StoredAudit};
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    /// The store behind the daemon socket, or `None` when nothing has ever been
    /// logged there.
    ///
    /// A missing DB is not an error: "no prompts have gone through this daemon
    /// yet" is a valid answer to "show me the log". Each verb decides how to say
    /// so — the tail explains, `--json` and `export` still print an empty result
    /// a script can parse. The file is never created here: `SessionStore::open`
    /// would create and migrate it, and a read must not write.
    pub(super) fn open(socket: &Path) -> Result<Option<SessionStore>, ExitCode> {
        let mut db = socket.to_path_buf().into_os_string();
        db.push(".db");
        let db = PathBuf::from(db);
        if !db.exists() {
            eprintln!("audit: no log yet (no prompts sent through this daemon)");
            return Ok(None);
        }
        SessionStore::open(&db).map(Some).map_err(|e| {
            eprintln!("audit: {e}");
            ExitCode::FAILURE
        })
    }

    /// A Unix-millisecond bound. Non-numeric is a usage error: a typo'd filter
    /// that silently means "no filter" is how an export quietly stops being the
    /// window someone asked for.
    fn parse_ms(flag: &str, value: &str) -> Result<u64, ExitCode> {
        value.parse::<u64>().map_err(|_| {
            eprintln!("audit: {flag} wants Unix milliseconds, got {value:?}");
            ExitCode::from(2)
        })
    }

    /// `arreo audit [--json]`: the tail as one JSON object, each row in the
    /// export's own shape so the two cannot disagree.
    ///
    /// Same rows as the human table — the newest `limit`, oldest first — rather
    /// than `audit_query`'s first `limit` rows of all history: `--limit` on this
    /// verb has always meant "the tail", and a `--json` that silently showed a
    /// different window than the table next to it would be a trap.
    pub(super) fn print_json(store: &SessionStore, limit: usize) -> ExitCode {
        match store.audit_recent(limit) {
            Ok(mut events) => {
                events.reverse();
                println!("{}", json_object(&events));
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("audit: {e}");
                ExitCode::FAILURE
            }
        }
    }

    /// `{"count":N,"rows":[…]}` — the shape `--json` prints, so a row read here
    /// and a row read from an export are the same object.
    pub(super) fn json_object(events: &[StoredAudit]) -> serde_json::Value {
        let rows: Vec<serde_json::Value> = events.iter().map(audit_json).collect();
        serde_json::json!({
            "count": rows.len(),
            "rows": rows,
        })
    }

    /// What an export of zero rows looks like, in each format. Mirrors the
    /// store's own rendering of an empty window (what `audit_export` returns for
    /// a filter that matches nothing), so a log that does not exist yet and a
    /// window with no rows produce the same bytes.
    fn empty_export(format: ExportFormat) -> String {
        match format {
            ExportFormat::Jsonl => String::new(),
            ExportFormat::Json => "[]\n".to_string(),
        }
    }

    /// Write the export where `--out` asked: stdout for `-` (and for no flag at
    /// all), a file otherwise — and a file write names the path it wrote, because
    /// a silent success on a typo'd path is an export the operator does not have.
    fn emit(out: Option<&str>, text: &str, format: ExportFormat) -> ExitCode {
        match out {
            None | Some("-") => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Some(path) => match std::fs::write(path, text) {
                Ok(()) => {
                    println!("exported {} ({})", path, format.as_str());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("audit export: {path}: {e}");
                    ExitCode::FAILURE
                }
            },
        }
    }

    /// The value that follows a flag, or a usage error naming the flag. A
    /// trailing `--out` with nothing after it is a mistake, and saying so beats
    /// reporting the flag the user *did* mean as an unknown argument.
    fn value<'a>(rest: &'a [String], i: usize, flag: &str) -> Result<&'a str, ExitCode> {
        rest.get(i).map(String::as_str).ok_or_else(|| {
            eprintln!("audit: {flag} needs a value");
            ExitCode::from(2)
        })
    }

    /// `arreo audit export [--format jsonl|json] [--since MS] [--until MS]
    /// [--action NAME] [--out PATH|-]`.
    pub(super) fn cmd_export(socket: PathBuf, rest: &[String]) -> ExitCode {
        let mut format = ExportFormat::Jsonl;
        let mut since: Option<u64> = None;
        let mut until: Option<u64> = None;
        let mut action: Option<String> = None;
        let mut out: Option<String> = None;
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--format" => {
                    let raw = match value(rest, i + 1, "--format") {
                        Ok(raw) => raw,
                        Err(code) => return code,
                    };
                    match ExportFormat::parse(raw) {
                        Some(parsed) => format = parsed,
                        None => {
                            eprintln!("audit export: --format wants jsonl or json, got {raw:?}");
                            return ExitCode::from(2);
                        }
                    }
                    i += 2;
                }
                "--since" => {
                    let raw = match value(rest, i + 1, "--since") {
                        Ok(raw) => raw,
                        Err(code) => return code,
                    };
                    match parse_ms("--since", raw) {
                        Ok(ms) => since = Some(ms),
                        Err(code) => return code,
                    }
                    i += 2;
                }
                "--until" => {
                    let raw = match value(rest, i + 1, "--until") {
                        Ok(raw) => raw,
                        Err(code) => return code,
                    };
                    match parse_ms("--until", raw) {
                        Ok(ms) => until = Some(ms),
                        Err(code) => return code,
                    }
                    i += 2;
                }
                "--action" => {
                    action = match value(rest, i + 1, "--action") {
                        Ok(raw) => Some(raw.to_string()),
                        Err(code) => return code,
                    };
                    i += 2;
                }
                "--out" => {
                    out = match value(rest, i + 1, "--out") {
                        Ok(raw) => Some(raw.to_string()),
                        Err(code) => return code,
                    };
                    i += 2;
                }
                other => {
                    eprintln!("audit export: unknown argument {other:?}");
                    eprintln!("usage: arreo audit export [--format jsonl|json] [--since MS] [--until MS] [--action NAME] [--out PATH|-] [--socket PATH]");
                    return ExitCode::from(2);
                }
            }
        }
        let store = match open(&socket) {
            Ok(Some(store)) => store,
            // A log that does not exist holds no rows in the window: emit the
            // empty export rather than nothing, so `--format json | jq` works on
            // a machine that has never logged anything.
            Ok(None) => return emit(out.as_deref(), &empty_export(format), format),
            Err(code) => return code,
        };
        // The filters are passed through as given: the export is a *view* of the
        // log, so the same window twice must be byte-identical, and anything the
        // CLI added on top would break that. The limit is a real bound, not
        // `usize::MAX`: an export means the whole window, and `i64::MAX` says so
        // without leaning on SQLite's negative-LIMIT-means-unlimited quirk.
        let query = AuditQuery {
            since_ms: since,
            until_ms: until,
            action,
            limit: i64::MAX as usize,
        };
        let text = match store.audit_export(&query, format) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("audit export: {e}");
                return ExitCode::FAILURE;
            }
        };
        emit(out.as_deref(), &text, format)
    }

    /// `arreo audit prune --before MS`: delete rows older than the bound.
    ///
    /// `--before` is required. Nothing prunes the log on a timer — an
    /// append-only log that quietly deletes itself is not an audit log — so a
    /// prune with no bound is a mistake, and this refuses to guess one.
    pub(super) fn cmd_prune(socket: PathBuf, rest: &[String]) -> ExitCode {
        // `--socket` was already peeled off by `cmd_audit`, so `rest` is just
        // this subcommand's own flags.
        let mut before: Option<u64> = None;
        let mut i = 0;
        while i < rest.len() {
            match rest[i].as_str() {
                "--before" => {
                    let raw = match value(rest, i + 1, "--before") {
                        Ok(raw) => raw,
                        Err(code) => return code,
                    };
                    match parse_ms("--before", raw) {
                        Ok(ms) => before = Some(ms),
                        Err(code) => return code,
                    }
                    i += 2;
                }
                other => {
                    eprintln!("audit prune: unknown argument {other:?}");
                    eprintln!("usage: arreo audit prune --before MS [--socket PATH]");
                    return ExitCode::from(2);
                }
            }
        }
        let Some(before_ms) = before else {
            eprintln!(
                "audit prune: --before MS is required (nothing prunes the log automatically)"
            );
            eprintln!("usage: arreo audit prune --before MS [--socket PATH]");
            return ExitCode::from(2);
        };
        let store = match open(&socket) {
            Ok(Some(store)) => store,
            // Nothing was ever logged, so nothing was older than the bound. The
            // count still prints: a script reads this line, and "the log does not
            // exist" and "the log had nothing to drop" are the same answer.
            Ok(None) => {
                println!("pruned 0 row(s) older than {before_ms}");
                return ExitCode::SUCCESS;
            }
            Err(code) => return code,
        };
        let now_ms = arreo_core::identity::authority::now_ms().max(0) as u64;
        match store.audit_prune(before_ms, now_ms) {
            Ok(removed) => {
                println!("pruned {removed} row(s) older than {before_ms}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("audit prune: {e}");
                ExitCode::FAILURE
            }
        }
    }
}

/// `arreo devices …` (T-0025): the server-host operator's view of device
/// identity. Reads and writes the same files and store the daemon uses, so it
/// works whether or not the daemon is running (WAL SQLite tolerates both).
///
/// Subcommands:
///   `id`        — this machine's own device key + id (creates it if absent)
///   `list`      — every pinned device (add `--json` for scripts)
///   `issue`     — sign a certificate for a device public key
///   `rotate`    — move a device onto a new key (the old key stops working)
///   `revoke`    — refuse a device from now on (durable, audited)
///   `authorize` — ask the authority about a key (exit 0 = allowed)
/// Which devices a listing shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceFilter {
    Live,
    Revoked,
    All,
}

fn cmd_devices(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    let mut json = false;
    let mut args: Vec<String> = Vec::new();
    for arg in kept {
        if arg == "--json" {
            json = true;
        } else {
            args.push(arg);
        }
    }
    let Some(sub) = args.first().map(String::as_str) else {
        eprintln!("usage: arreo devices <id|list|issue|rotate|revoke|authorize> [options] [--socket PATH]");
        return ExitCode::from(2);
    };
    match sub {
        "id" => devices_id(json),
        // `--revoked` shows the tombstones; `--all` shows both. The default is
        // the live set, because that is the question "who can reach this machine
        // right now" — and a revoked device appearing in it would be a lie.
        "list" => {
            let show = if args.iter().any(|arg| arg == "--revoked") {
                DeviceFilter::Revoked
            } else if args.iter().any(|arg| arg == "--all") {
                DeviceFilter::All
            } else {
                DeviceFilter::Live
            };
            devices_list(&socket, json, show)
        }
        "issue" => devices_issue(&socket, &args[1..], json),
        "rotate" => devices_rotate(&socket, &args[1..], json),
        "revoke" => devices_revoke(&socket, &args[1..], json),
        "authorize" => devices_authorize(&socket, &args[1..], json),
        other => {
            eprintln!("devices: unknown subcommand {other:?}");
            eprintln!("usage: arreo devices <id|list|issue|rotate|revoke|authorize> [options] [--socket PATH]");
            ExitCode::from(2)
        }
    }
}

fn open_authority(
    socket: &std::path::Path,
) -> Result<arreo_core::identity::authority::DeviceAuthority, ExitCode> {
    arreo_core::identity::authority::DeviceAuthority::load(
        arreo_core::identity::authority::Layout::for_socket(socket),
    )
    .map_err(|e| {
        eprintln!("devices: {e}");
        ExitCode::FAILURE
    })
}

/// This machine's client key: printed as id + public key, so the *server*
/// operator can pin it. Creating it here means "my identity" is one command.
fn devices_id(json: bool) -> ExitCode {
    let path = arreo_core::identity::authority::client_key_path();
    let key = match arreo_core::identity::authority::client_key() {
        Ok(key) => key,
        Err(e) => {
            eprintln!("devices: {e}");
            return ExitCode::FAILURE;
        }
    };
    let id = arreo_core::identity::DeviceId::from_key(&key.public());
    if json {
        println!(
            "{}",
            serde_json::json!({
                "device": id.display_id(),
                "public_key": key.public_hex(),
                "key_file": path.display().to_string(),
            })
        );
    } else {
        println!("device {} key {}", id.display_id(), key.public_hex());
        println!("(key file: {})", path.display());
        println!("pin it on the server with: arreo devices issue --name <name> --role <owner|viewer> --key {}", key.public_hex());
    }
    ExitCode::SUCCESS
}

fn devices_list(socket: &std::path::Path, json: bool, show: DeviceFilter) -> ExitCode {
    let authority = match open_authority(socket) {
        Ok(authority) => authority,
        Err(code) => return code,
    };
    let all = authority.devices();
    let devices: Vec<_> = match show {
        // Tombstones are the point of `--revoked`: a revoked device keeps its
        // row, so the operator can see what was cut off (and by whom) without
        // reading the audit log.
        DeviceFilter::Revoked => all.iter().filter(|d| d.revoked).cloned().collect(),
        DeviceFilter::Live => all.iter().filter(|d| !d.revoked).cloned().collect(),
        DeviceFilter::All => all,
    };
    if json {
        let rows: Vec<serde_json::Value> = devices
            .iter()
            .map(|device| {
                serde_json::json!({
                    "id": device.id.display_id(),
                    "name": device.name,
                    "role": device.role.as_str(),
                    "public_key": device.public_hex(),
                    "serial": device.serial,
                    "issued_at_ms": device.issued_at_ms,
                    "last_seen_ms": device.last_seen_ms,
                    "revoked": device.revoked,
                    "revoked_at_ms": device.revoked_at_ms,
                    "revoked_by": device.revoked_by,
                    "retired_to": device.retired_to.as_ref().map(arreo_core::identity::DeviceId::display_id),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "root": authority.root_fingerprint(),
                "devices": rows,
            })
        );
        return ExitCode::SUCCESS;
    }
    println!("root {}…", &authority.root_fingerprint()[..16]);
    if devices.is_empty() {
        println!(
            "{}",
            match show {
                DeviceFilter::Revoked => "no revoked devices".to_string(),
                _ => "no devices paired yet (pair one, or issue from a public key)".to_string(),
            }
        );
        return ExitCode::SUCCESS;
    }
    println!(
        "{:<36} {:<16} {:<7} {:>6}  STATUS",
        "DEVICE", "NAME", "ROLE", "SERIAL"
    );
    for device in devices {
        let status = if device.revoked {
            // Who and when, in the place an operator looks first — the table.
            match (&device.revoked_by, device.revoked_at_ms) {
                (Some(by), Some(at)) => format!("revoked by {by} at {at}"),
                (Some(by), None) => format!("revoked by {by}"),
                _ => "revoked".to_string(),
            }
        } else if let Some(replacement) = &device.retired_to {
            format!("rotated → {}", replacement.display_id())
        } else if device.last_seen_ms.is_some() {
            "active".to_string()
        } else {
            "never seen".to_string()
        };
        println!(
            "{:<36} {:<16} {:<7} {:>6}  {}",
            device.id.display_id(),
            device.name,
            device.role.as_str(),
            device.serial,
            status
        );
    }
    ExitCode::SUCCESS
}

/// Parse `--name`, `--role`, `--key <hex>` (or `--key-file <path>`).
fn parse_issue_args(
    args: &[String],
) -> Result<(String, arreo_core::identity::Role, String), String> {
    let mut name: Option<String> = None;
    let mut role: Option<arreo_core::identity::Role> = None;
    let mut key: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--name" if i + 1 < args.len() => {
                name = Some(args[i + 1].clone());
                i += 2;
            }
            "--role" if i + 1 < args.len() => {
                role = arreo_core::identity::Role::parse(&args[i + 1]).ok();
                if role.is_none() {
                    return Err(format!("unknown role {:?} (owner|viewer)", args[i + 1]));
                }
                i += 2;
            }
            "--key" if i + 1 < args.len() => {
                key = Some(args[i + 1].clone());
                i += 2;
            }
            "--key-file" if i + 1 < args.len() => {
                key = Some(
                    std::fs::read_to_string(&args[i + 1])
                        .map_err(|e| format!("{}: {e}", args[i + 1]))?
                        .trim()
                        .to_string(),
                );
                i += 2;
            }
            other => return Err(format!("unexpected argument {other:?}")),
        }
    }
    let name = name.ok_or("missing --name")?;
    let role = role.ok_or("missing --role")?;
    let key = key.ok_or("missing --key <hex> or --key-file <path>")?;
    Ok((name, role, key))
}

/// The one hex-key parser (`arreo_core::identity::verifying_key_from_hex`),
/// with this command's error phrasing kept for its own output.
fn parse_public_key_hex(hex: &str) -> Result<arreo_core::identity::VerifyingKey, String> {
    arreo_core::identity::verifying_key_from_hex(hex).map_err(|e| e.to_string())
}

fn devices_issue(socket: &std::path::Path, args: &[String], json: bool) -> ExitCode {
    let (name, role, key_hex) = match parse_issue_args(args) {
        Ok(parsed) => parsed,
        Err(e) => {
            eprintln!("devices issue: {e}");
            return ExitCode::from(2);
        }
    };
    let key = match parse_public_key_hex(&key_hex) {
        Ok(key) => key,
        Err(e) => {
            eprintln!("devices issue: {e}");
            return ExitCode::from(2);
        }
    };
    let mut authority = match open_authority(socket) {
        Ok(authority) => authority,
        Err(code) => return code,
    };
    match authority.issue(&name, role, &key) {
        Ok(cert) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "device": cert.device().display_id(),
                        "name": cert.name(),
                        "role": cert.role().as_str(),
                        "serial": cert.serial(),
                        "issued_at_ms": cert.payload.issued_at_ms,
                    })
                );
            } else {
                println!(
                    "issued {} ({}) for {} as {} — serial {}",
                    cert.device().display_id(),
                    cert.name(),
                    cert.role().as_str(),
                    role,
                    cert.serial()
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("devices issue: {e}");
            ExitCode::FAILURE
        }
    }
}

fn devices_rotate(socket: &std::path::Path, args: &[String], json: bool) -> ExitCode {
    let mut device: Option<arreo_core::identity::DeviceId> = None;
    let mut name: Option<String> = None;
    let mut role: Option<arreo_core::identity::Role> = None;
    let mut key: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--device" if i + 1 < args.len() => {
                device = arreo_core::identity::DeviceId::parse(&args[i + 1]).ok();
                if device.is_none() {
                    eprintln!("devices rotate: not a device id: {:?}", args[i + 1]);
                    return ExitCode::from(2);
                }
                i += 2;
            }
            "--name" if i + 1 < args.len() => {
                name = Some(args[i + 1].clone());
                i += 2;
            }
            "--role" if i + 1 < args.len() => {
                role = arreo_core::identity::Role::parse(&args[i + 1]).ok();
                if role.is_none() {
                    eprintln!(
                        "devices rotate: unknown role {:?} (owner|viewer)",
                        args[i + 1]
                    );
                    return ExitCode::from(2);
                }
                i += 2;
            }
            "--key" if i + 1 < args.len() => {
                key = Some(args[i + 1].clone());
                i += 2;
            }
            "--key-file" if i + 1 < args.len() => {
                key = std::fs::read_to_string(&args[i + 1])
                    .map(|text| text.trim().to_string())
                    .ok();
                i += 2;
            }
            other => {
                eprintln!("devices rotate: unexpected argument {other:?}");
                return ExitCode::from(2);
            }
        }
    }
    let (Some(device), Some(key_hex)) = (device, key) else {
        eprintln!("usage: arreo devices rotate --device <id> [--name N] [--role R] --key <hex>");
        return ExitCode::from(2);
    };
    let key = match parse_public_key_hex(&key_hex) {
        Ok(key) => key,
        Err(e) => {
            eprintln!("devices rotate: {e}");
            return ExitCode::from(2);
        }
    };
    let mut authority = match open_authority(socket) {
        Ok(authority) => authority,
        Err(code) => return code,
    };
    // The role and name default to the device's current ones: rotation is
    // about the key, not about changing what the device may do.
    let previous = authority.devices().into_iter().find(|r| r.id == device);
    let Some(previous) = previous else {
        eprintln!("devices rotate: no device {}", device.display_id());
        return ExitCode::FAILURE;
    };
    let role = role.unwrap_or(previous.role);
    let name = name.unwrap_or(previous.name);
    match authority.rotate(&device, &name, role, &key) {
        Ok(cert) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "device": cert.device().display_id(),
                        "replaced": device.display_id(),
                        "serial": cert.serial(),
                    })
                );
            } else {
                println!(
                    "rotated {} → {} (serial {}); the old key no longer authorizes",
                    device.display_id(),
                    cert.device().display_id(),
                    cert.serial()
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("devices rotate: {e}");
            ExitCode::FAILURE
        }
    }
}

fn devices_revoke(socket: &std::path::Path, args: &[String], json: bool) -> ExitCode {
    let Some(raw) = args.first() else {
        eprintln!("usage: arreo devices revoke <name|id> [--json]");
        return ExitCode::from(2);
    };
    let mut authority = match open_authority(socket) {
        Ok(authority) => authority,
        Err(code) => return code,
    };
    // A name or an id: an operator holding a phone says "the pixel", and one
    // reading a log says the fingerprint. Both should work, and an ambiguous
    // name must be refused rather than resolved to a guess.
    let device = match parse_device_reference(&authority, raw) {
        Ok(device) => device,
        Err(e) => {
            eprintln!("devices revoke: {e}");
            return ExitCode::from(3);
        }
    };
    // Who made the call: a device id when the operator runs this through a
    // device session, or `local-cli` for the machine's own socket, which is the
    // case this command serves today.
    let revoker = arreo_core::identity::revocation::LOCAL_CLI;
    let now = arreo_core::identity::authority::now_ms();
    match authority.revoke(&device, revoker, now) {
        Ok(arreo_core::identity::authority::Revocation::Revoked) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "revoked": true,
                        "already": false,
                        "device": device.display_id(),
                        "by": revoker,
                        "at_ms": now,
                    })
                );
            } else {
                println!("revoked {}", device.display_id());
            }
            ExitCode::SUCCESS
        }
        Ok(arreo_core::identity::authority::Revocation::AlreadyRevoked) => {
            // Idempotent, and honest about it: exit 0 because the desired state
            // holds, but say "already" so a script can tell the difference.
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "revoked": true,
                        "already": true,
                        "device": device.display_id(),
                    })
                );
            } else {
                println!("{} was already revoked", device.display_id());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("devices revoke: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Resolve a `<name|id>` reference to one device.
///
/// A name that matches more than one device is an error rather than a choice:
/// revoking the wrong device is not a mistake an operator should be able to make
/// by having two phones called "pixel".
fn parse_device_reference(
    authority: &arreo_core::identity::DeviceAuthority,
    raw: &str,
) -> Result<arreo_core::identity::DeviceId, String> {
    if let Ok(id) = arreo_core::identity::DeviceId::parse(raw) {
        return Ok(id);
    }
    let matches: Vec<_> = authority
        .devices()
        .into_iter()
        .filter(|device| device.name == raw)
        .collect();
    match matches.len() {
        1 => Ok(matches[0].id.clone()),
        0 => Err(format!(
            "no device is named {raw:?} (try `arreo devices list`)"
        )),
        n => Err(format!(
            "{n} devices are named {raw:?}: revoke one by id ({})",
            matches
                .iter()
                .map(|device| device.id.display_id())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// Ask the authority about a key — the same call the remote transport makes.
/// Exit 0 = this key may connect (and, with `--verb`, may do that verb).
///
/// With no key argument it uses this machine's own client key, which is the
/// server-host case: the operator's own machine is a device like any other.
/// A peer's key is passed explicitly (that is what the transport does with the
/// key a connecting device proves possession of).
fn devices_authorize(socket: &std::path::Path, args: &[String], json: bool) -> ExitCode {
    let mut verb: Option<arreo_core::identity::role::Verb> = None;
    let mut key_arg: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--verb" if i + 1 < args.len() => {
                match parse_verb(&args[i + 1]) {
                    Some(parsed) => verb = Some(parsed),
                    None => {
                        eprintln!(
                            "devices authorize: unknown verb {:?} \
                             (read|attach|wait|metrics|panes|send|spawn|split|kill)",
                            args[i + 1]
                        );
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            other if !other.starts_with('-') && key_arg.is_none() => {
                key_arg = Some(other.to_string());
                i += 1;
            }
            other => {
                eprintln!("devices authorize: unexpected argument {other:?}");
                return ExitCode::from(2);
            }
        }
    }
    let key_hex = match key_arg {
        Some(key) => key,
        None => match arreo_core::identity::authority::client_key() {
            Ok(key) => key.public_hex(),
            Err(e) => {
                eprintln!("devices authorize: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    let key = match parse_public_key_hex(&key_hex) {
        Ok(key) => key,
        Err(e) => {
            eprintln!("devices authorize: {e}");
            return ExitCode::from(2);
        }
    };
    let mut authority = match open_authority(socket) {
        Ok(authority) => authority,
        Err(code) => return code,
    };
    // With `--verb`, this is the transport's exact decision path:
    // authenticate, then enforce the role policy in one call.
    if let Some(verb) = verb {
        let device = arreo_core::identity::DeviceId::from_key(&key);
        let role = authority.role_of(&device);
        return match authority.check_verb(&key, verb) {
            Ok(()) => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "allowed": true,
                            "device": device.display_id(),
                            "role": role.map(|r| r.as_str()),
                            "verb": format!("{verb:?}").to_lowercase(),
                        })
                    );
                } else {
                    println!(
                        "allowed {} to {verb:?} (role {})",
                        device.display_id(),
                        role.map(|r| r.as_str()).unwrap_or("?")
                    );
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({ "allowed": false, "reason": e.to_string() })
                    );
                } else {
                    eprintln!("denied: {e}");
                }
                ExitCode::FAILURE
            }
        };
    }
    match authority.authorize(&key) {
        Ok(record) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "allowed": true,
                        "device": record.id.display_id(),
                        "role": record.role.as_str(),
                        "name": record.name,
                    })
                );
            } else {
                println!(
                    "allowed {} ({}) as {}",
                    record.id.display_id(),
                    record.name,
                    record.role
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({ "allowed": false, "reason": e.to_string() })
                );
            } else {
                eprintln!("refused: {e}");
            }
            ExitCode::FAILURE
        }
    }
}

/// The verbs a device can be checked against, spelled as in `role::Verb`.
fn parse_verb(text: &str) -> Option<arreo_core::identity::role::Verb> {
    use arreo_core::identity::role::Verb;
    match text.to_ascii_lowercase().as_str() {
        "read" => Some(Verb::Read),
        "attach" => Some(Verb::Attach),
        "wait" => Some(Verb::Wait),
        "metrics" => Some(Verb::Metrics),
        "panes" => Some(Verb::Panes),
        "send" => Some(Verb::Send),
        "spawn" => Some(Verb::Spawn),
        "split" => Some(Verb::Split),
        "kill" => Some(Verb::Kill),
        "admin" => Some(Verb::Admin),
        "hello" => Some(Verb::Hello),
        _ => None,
    }
}

/// `arreo pair` (T-0024): pin a device using a four-word code.
///
/// Server side (on the machine running the daemon):
///   `arreo pair [--name <label>] [--role owner|viewer] [--ttl-secs 300]
///               [--mailbox <path|host:port>] [--socket PATH] [--json]`
/// prints the code and the invite URI, waits, and issues a certificate.
///
/// Phone side (on the device being paired):
///   `arreo pair --join "<four words>" --uri <arreo://pair?...> [--name <label>] [--json]`
/// generates a keypair (in memory), proves the code, and stores the
/// certificate the server signs.
///
/// The code is the only shared secret, it never travels, and a wrong guess
/// burns the session — see `arreo_core::pairing` for the exact argument.
fn cmd_pair(rest: &[String]) -> ExitCode {
    let (socket, kept) = take_socket(rest);
    let mut join_code: Option<String> = None;
    let mut uri: Option<String> = None;
    let mut name: Option<String> = None;
    let mut role: Option<arreo_core::identity::Role> = None;
    let mut ttl_secs: u64 = arreo_core::pairing::flow::DEFAULT_TTL.as_secs();
    let mut mailbox: Option<String> = None;
    let mut json = false;
    let mut config: Option<PathBuf> = None;

    let mut i = 0;
    while i < kept.len() {
        match kept[i].as_str() {
            "--config" if i + 1 < kept.len() => {
                config = Some(PathBuf::from(&kept[i + 1]));
                i += 2;
            }
            "--join" if i + 1 < kept.len() => {
                join_code = Some(kept[i + 1].clone());
                i += 2;
            }
            "--uri" if i + 1 < kept.len() => {
                uri = Some(kept[i + 1].clone());
                i += 2;
            }
            "--name" if i + 1 < kept.len() => {
                name = Some(kept[i + 1].clone());
                i += 2;
            }
            "--role" if i + 1 < kept.len() => {
                match arreo_core::identity::Role::parse(&kept[i + 1]) {
                    Ok(parsed) => role = Some(parsed),
                    Err(e) => {
                        eprintln!("pair: {e}");
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--ttl-secs" if i + 1 < kept.len() => {
                match kept[i + 1].parse::<u64>() {
                    Ok(secs) if secs >= 1 => ttl_secs = secs,
                    _ => {
                        eprintln!("pair: --ttl-secs wants a positive number of seconds");
                        return ExitCode::from(2);
                    }
                }
                i += 2;
            }
            "--mailbox" if i + 1 < kept.len() => {
                mailbox = Some(kept[i + 1].clone());
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            other => {
                eprintln!("pair: unexpected argument {other:?}");
                eprintln!("usage: arreo pair [--name N] [--role owner|viewer] [--ttl-secs N] [--mailbox ADDR] [--json]");
                eprintln!("       arreo pair --join \"four words\" --uri arreo://pair?... [--name N] [--json]");
                return ExitCode::from(2);
            }
        }
    }

    match (join_code, uri) {
        (Some(_), None) => {
            eprintln!(
                "pair: --join also needs --uri (it carries the mailbox, session and server key)"
            );
            ExitCode::from(2)
        }
        (None, Some(_)) => {
            eprintln!("pair: --uri without --join: the code is typed by the human, never carried in the invite");
            ExitCode::from(2)
        }
        (Some(code), Some(uri)) => cmd_pair_phone(&code, &uri, name, json),
        (None, None) => cmd_pair_server(&socket, name, role, ttl_secs, mailbox, json, config),
    }
}

/// Default mailbox when the server does not name one: the same runtime dir the
/// daemon's socket lives in, so a local relay is found without configuration.
fn default_mailbox() -> std::path::PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return std::path::PathBuf::from(runtime).join("arreo-relay.sock");
    }
    std::path::PathBuf::from("/tmp/arreo-relay.sock")
}

fn cmd_pair_server(
    socket: &std::path::Path,
    name: Option<String>,
    role: Option<arreo_core::identity::Role>,
    ttl_secs: u64,
    mailbox: Option<String>,
    json: bool,
    config: Option<PathBuf>,
) -> ExitCode {
    use arreo_core::pairing::flow::PairingServer;
    use arreo_core::pairing::{MailboxAddr, PairingError};

    let addr = match mailbox {
        Some(text) => match MailboxAddr::parse(&text) {
            Ok(addr) => addr,
            Err(e) => {
                eprintln!("pair: {e}");
                return ExitCode::from(2);
            }
        },
        None => MailboxAddr::Unix(default_mailbox()),
    };
    let mut authority = match open_authority(socket) {
        Ok(authority) => authority,
        Err(code) => return code,
    };
    let ttl = std::time::Duration::from_secs(ttl_secs);

    // The server's identity key signs the certificate *and* is the SPAKE2
    // identity, so the code authenticates exactly the machine the phone will
    // trust.
    let root = match arreo_core::identity::RootKey::load_or_generate(&root_key_path()) {
        Ok(root) => root,
        Err(e) => {
            eprintln!("pair: cannot load the server identity key: {e}");
            return ExitCode::FAILURE;
        }
    };
    // The directory hint (T-0058): this machine's account and relay, so the
    // machine it admits knows where to register itself. Absent when this machine
    // is not on a relay — an ordinary pairing, unchanged.
    //
    // A configuration that is present but *incomplete* is a loud error rather
    // than a silent `None`: an operator who enabled the relay and then admitted a
    // machine that joins nothing would have a bug they cannot see.
    let directory = match directory_hint(config.as_deref()) {
        Ok(hint) => hint,
        Err(message) => {
            eprintln!("pair: {message}");
            return ExitCode::from(2);
        }
    };
    let mut server = match PairingServer::begin(&root, addr, ttl, directory) {
        Ok(server) => server,
        Err(e) => {
            eprintln!("pair: {e}");
            return ExitCode::FAILURE;
        }
    };

    let invite = server.invite().clone();
    let code = server.code().phrase();
    if json {
        println!(
            "{}",
            serde_json::json!({
                "role": "server",
                "code": code,
                "uri": invite.uri(),
                "session": invite.session,
                "mailbox": invite.mailbox.as_str(),
                "server_key": invite.server_key,
                "expires_in_secs": ttl_secs,
            })
        );
    } else {
        println!("pair this device (expires in {}s):", ttl_secs);
        println!();
        println!("  code: {code}");
        println!("  uri:  {}", invite.uri());
        println!();
        println!("On the device being paired:");
        println!("  arreo pair --join \"{code}\" --uri '{}'", invite.uri());
    }
    // Flush both streams so a caller reading our stdout sees the code before we
    // block waiting for the phone.
    use std::io::Write as _;
    let _ = std::io::stdout().flush();

    let request = match server.receive() {
        Ok(request) => request,
        Err(e) => return pair_failed(&authority, &invite.session, &e, json),
    };
    // The phone proposes a name; the server may override it. Either way it is
    // untrusted text: trimmed, length-capped, and control characters dropped so
    // it cannot corrupt a terminal or a log line.
    let label = name.unwrap_or_else(|| request.name.clone());
    let Some(label) = sanitize_device_name(&label) else {
        let error = PairingError::BadInvite("the device name is empty".into());
        return pair_failed(&authority, &invite.session, &error, json);
    };
    let role = role.unwrap_or(arreo_core::identity::Role::Viewer);
    let cert = match authority.issue(&label, role, &request.public_key) {
        Ok(cert) => cert,
        Err(e) => {
            let error = PairingError::BadInvite(e.to_string());
            return pair_failed(&authority, &invite.session, &error, json);
        }
    };
    if let Err(e) = server.complete(&cert) {
        eprintln!("pair: the device was pinned but its certificate could not be delivered: {e}");
        eprintln!("pair: re-run `arreo pair` — the device did not store a certificate");
        return ExitCode::FAILURE;
    }
    if json {
        println!(
            "{}",
            serde_json::json!({
                "paired": true,
                "device": cert.device().display_id(),
                "name": label,
                "role": role.as_str(),
                "serial": cert.serial(),
            })
        );
    } else {
        println!("paired {} ({label}) as {role}", cert.device().display_id());
    }
    ExitCode::SUCCESS
}

/// One failure path for the server: audited, reported, never silent.
fn pair_failed(
    authority: &arreo_core::identity::authority::DeviceAuthority,
    session: &str,
    error: &arreo_core::pairing::PairingError,
    json: bool,
) -> ExitCode {
    // The audit trail is the operator's record of "someone tried"; the reason
    // goes in `prompt` and the session id in `agent`.
    let _ = authority.audit_pairing_failure(session, &error.to_string());
    if json {
        println!(
            "{}",
            serde_json::json!({ "paired": false, "reason": error.to_string(), "session": session })
        );
    } else {
        eprintln!("pairing failed: {error}");
    }
    ExitCode::FAILURE
}

/// Why a phone half of a pairing did not finish, split by what the caller can
/// do about it: a usage error is the human's flag, a failure is the exchange.
enum PairError {
    Usage(String),
    Failed(String),
}

/// The phone half of a pairing, start to finish: parse, exchange, and persist
/// what came back.
///
/// **Extracted for T-0058.** `arreo pair --join` and `arreo machines add` are the
/// same exchange — the second one just keeps going afterwards and asserts a
/// directory row. Two copies of this flow would be two places for the
/// persist-only-on-success rule (and the key-reuse rule, and the pinned-server
/// rule) to drift, and this is security-critical code where a drift is a hole.
fn join_pairing(
    code_text: &str,
    uri: &str,
    name: Option<String>,
) -> Result<
    (
        arreo_core::pairing::flow::PairedDevice,
        arreo_core::pairing::Invite,
    ),
    PairError,
> {
    use arreo_core::pairing::flow::PairingPhone;
    use arreo_core::pairing::{Code, Invite};

    let code = Code::parse(code_text).map_err(|e| PairError::Usage(e.to_string()))?;
    let invite = Invite::parse_uri(uri).map_err(|e| PairError::Usage(e.to_string()))?;
    // The dev box default is the machine's own hostname; a phone passes
    // --name. Either way the server may override it.
    let label = name.unwrap_or_else(default_device_name);
    let Some(label) = sanitize_device_name(&label) else {
        return Err(PairError::Usage("the device name is empty".into()));
    };

    // An existing keypair is reused (re-pairing the same device keeps its
    // identity); a fresh one is generated in memory and only written once the
    // certificate arrives — a failed pairing must leave nothing behind.
    let key_path = arreo_core::identity::identity_root().join("device.key");
    let (key, generated) = match arreo_core::identity::DeviceKey::load(&key_path) {
        Ok(key) => (key, false),
        Err(arreo_core::identity::KeyError::Missing { .. }) => {
            match arreo_core::identity::DeviceKey::generate() {
                Ok(key) => (key, true),
                Err(e) => return Err(PairError::Failed(e.to_string())),
            }
        }
        Err(e) => {
            return Err(PairError::Failed(format!(
                "cannot read {}: {e}",
                key_path.display()
            )))
        }
    };

    let phone = PairingPhone::join(&invite, &code, key, &label)
        .map_err(|e| PairError::Failed(e.to_string()))?;
    let paired = phone
        .await_cert()
        .map_err(|e| PairError::Failed(e.to_string()))?;

    // Success is the only moment anything is written.
    if generated {
        paired.key.save(&key_path).map_err(|e| {
            PairError::Failed(format!("paired, but cannot save the device key: {e}"))
        })?;
    }
    let cert = &paired.cert;
    let cert_dir = arreo_core::identity::identity_root().join("devices");
    let cert_path = cert
        .save(&cert_dir)
        .map_err(|e| PairError::Failed(format!("paired, but cannot save the certificate: {e}")))?;
    // Pin the server's key so later connections can verify its certificates
    // without the invite (public material — no secret here).
    let server_key_path = arreo_core::identity::identity_root().join("server.key");
    let pinned = arreo_core::identity::identity_root();
    arreo_core::identity::keys::create_private_dir(&pinned)
        .and_then(|()| {
            std::fs::write(&server_key_path, format!("{}\n", invite.server_key)).map_err(|e| {
                arreo_core::identity::KeyError::Io {
                    path: server_key_path.clone(),
                    detail: e.to_string(),
                }
            })
        })
        .map_err(|e| PairError::Failed(format!("paired, but cannot save the server key: {e}")))?;

    // The path is part of the result: a caller that prints it is telling the
    // operator which file to keep, and one that cannot must not print it.
    PAIRED_CERT_PATH.with(|cell| cell.set(Some(cert_path)));
    Ok((paired, invite))
}

thread_local! {
    /// Where the certificate of the last successful pairing went. A
    /// thread-local because the pairing path is synchronous and single-threaded
    /// (this binary's `rt::block_on` and this flow are both blocking); it exists
    /// so the *success message* can name the file without the flow function
    /// growing a second return value that every caller has to carry.
    static PAIRED_CERT_PATH: std::cell::Cell<Option<std::path::PathBuf>> =
        const { std::cell::Cell::new(None) };
}

fn cmd_pair_phone(code_text: &str, uri: &str, name: Option<String>, json: bool) -> ExitCode {
    let (paired, invite) = match join_pairing(code_text, uri, name) {
        Ok(pair) => pair,
        Err(PairError::Usage(message)) => {
            eprintln!("pair: {message}");
            return ExitCode::from(2);
        }
        Err(PairError::Failed(message)) => {
            eprintln!("pair: {message}");
            return ExitCode::FAILURE;
        }
    };
    let cert = &paired.cert;
    let cert_path = PAIRED_CERT_PATH
        .with(|cell| cell.take())
        .unwrap_or_default();
    if json {
        println!(
            "{}",
            serde_json::json!({
                "paired": true,
                "device": cert.device().display_id(),
                "name": cert.name(),
                "role": cert.role().as_str(),
                "cert_file": cert_path.display().to_string(),
                "server_key": invite.server_key,
            })
        );
    } else {
        println!(
            "paired with this server as {} ({})",
            cert.role(),
            cert.device().display_id()
        );
        println!("certificate: {}", cert_path.display());
    }
    ExitCode::SUCCESS
}

/// The account and relay a joining machine should register itself with
/// (T-0058), read from this machine's `[relay]` configuration.
///
/// `Ok(None)` is "this machine is not on a relay" (no path given, no file, or
/// the section disables it) — an ordinary pairing. `Err` is a configuration that
/// was given and is wrong: the operator must see that, because the alternative
/// is admitting a machine that silently joins nothing.
fn directory_hint(
    config: Option<&std::path::Path>,
) -> Result<Option<arreo_core::pairing::flow::DirectoryHint>, String> {
    let Some(path) = config
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("ARREO_CONFIG").map(PathBuf::from))
    else {
        return Ok(None);
    };
    match arreo_core::relay::config::load_config(&path) {
        Ok(Some(settings)) => Ok(Some(arreo_core::pairing::flow::DirectoryHint {
            account: settings.account,
            relay: settings.addr.to_string(),
        })),
        Ok(None) => Ok(None),
        Err(e) => Err(format!(
            "the configuration at {} cannot be used, so the invite cannot name an account: {e}",
            path.display()
        )),
    }
}

/// The server's identity key path (its root key — one identity per server).
fn root_key_path() -> std::path::PathBuf {
    arreo_core::identity::identity_root().join("root.key")
}

/// Trim, cap and de-control an untrusted device name.
fn sanitize_device_name(raw: &str) -> Option<String> {
    let cleaned: String = raw
        .trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    let cleaned = cleaned.trim().to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// This machine's default device name: its hostname, or a stable fallback.
///
/// The rule lives in `arreo-core` because the daemon needs the same default for
/// its directory row (T-0056): one answer to "what is this machine called",
/// not two that drift.
fn default_device_name() -> String {
    arreo_core::mesh::default_machine_name()
}
