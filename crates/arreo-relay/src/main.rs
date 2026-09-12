//! `arreo-relay` binary: the pairing mailbox (T-0024) and the router (T-0029).
//!
//! Usage:
//!   `arreo-relay serve --listen 127.0.0.1:8787 --state-dir /var/lib/arreo`   (router)
//!   `arreo-relay serve --listen ... --state-dir ... --pairing-tcp 0.0.0.0:8770`
//!   `arreo-relay account add --state-dir DIR --account ID --root-key HEX`
//!   `arreo-relay --pairing-socket /run/arreo/relay.sock`                      (mailbox only)
//!
//! Three doors, one binary, because a self-hosted relay is one thing to run
//! (ROADMAP §3.4): the **router** moves opaque envelopes between the account's
//! devices, the **pairing mailbox** carries SPAKE2 flights for a device that has
//! no certificate yet, and `account add` is the operator's hand on the account
//! registry — the one thing that must happen before any device can connect.
//!
//! `serve` needs a state directory (the router's SQLite file lives there and
//! must survive a restart); the legacy `--pairing-*` form needs no state at all,
//! because the mailbox is deliberately in-memory (T-0024's honest gap).

use std::path::{Path, PathBuf};
use std::sync::Arc;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("serve") => serve(&args[1..]),
        Some("account") => account(&args[1..]),
        Some("audit") => audit(&args[1..]),
        // Everything else is the T-0024 pairing-only form: bare flags, no
        // arguments, or a typo — which that path already refuses by name, so its
        // behavior (and its tests) stay exactly as they were.
        _ => pairing(&args),
    }
}

/// The original pairing-mailbox-only CLI (T-0024), unchanged.
fn pairing(args: &[String]) {
    let mut socket: Option<PathBuf> = None;
    let mut tcp: Option<String> = None;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--pairing-socket" => socket = args.next().map(PathBuf::from),
            "--pairing-tcp" => tcp = args.next().cloned(),
            "--help" | "-h" => {
                println!("usage: arreo-relay serve [--listen ADDR] --state-dir DIR [--pairing-tcp ADDR] [--pairing-socket PATH]");
                println!(
                    "       arreo-relay account add --state-dir DIR --account ID --root-key HEX"
                );
                println!(
                    "       arreo-relay audit export --state-dir DIR [--format jsonl|json] \
                     [--since MS] [--until MS] [--action NAME] [--out PATH|-]"
                );
                println!("       arreo-relay audit prune --state-dir DIR --before MS");
                println!("       arreo-relay [--pairing-socket PATH] [--pairing-tcp HOST:PORT]");
                println!("  `serve` runs the router (needs --state-dir); the bare flags run the");
                println!("  pairing mailbox alone, which needs no state and keeps nothing.");
                return;
            }
            other => {
                eprintln!("arreo-relay: unknown flag {other}");
                std::process::exit(2);
            }
        }
    }
    if socket.is_none() && tcp.is_none() {
        eprintln!("arreo-relay: nothing to serve — pass --pairing-socket and/or --pairing-tcp");
        std::process::exit(2);
    }

    let mailbox = Arc::new(arreo_relay::Mailbox::new());
    let mut handles = Vec::new();
    if let Some(path) = socket {
        spawn_unix_mailbox(&path, &mailbox, &mut handles);
    }
    if let Some(addr) = tcp {
        let mailbox = Arc::clone(&mailbox);
        eprintln!("arreo-relay: pairing mailbox on tcp://{addr}");
        handles.push(std::thread::spawn(move || {
            if let Err(e) = arreo_relay::pairing::serve_tcp(&addr, mailbox) {
                eprintln!("arreo-relay: tcp listener died: {e}");
                std::process::exit(1);
            }
        }));
    }
    for handle in handles {
        let _ = handle.join();
    }
}

/// Start the mailbox on a unix socket, where unix sockets exist.
///
/// On Windows there are none (T-0062), so `--pairing-socket` is **refused by
/// name** rather than compiled out silently: a flag that is accepted and then
/// ignored is the worst of the three behaviors, because the operator sees a
/// listener that is not there. The message names the alternative that does work
/// on every platform, so the refusal is actionable.
#[cfg(unix)]
fn spawn_unix_mailbox(
    path: &Path,
    mailbox: &Arc<arreo_relay::Mailbox>,
    threads: &mut Vec<std::thread::JoinHandle<()>>,
) {
    let mailbox = Arc::clone(mailbox);
    let path = path.to_path_buf();
    eprintln!("arreo-relay: pairing mailbox on unix://{}", path.display());
    threads.push(std::thread::spawn(move || {
        if let Err(e) = arreo_relay::pairing::serve_unix(&path, mailbox) {
            eprintln!("arreo-relay: unix listener died: {e}");
            std::process::exit(1);
        }
    }));
}

#[cfg(not(unix))]
fn spawn_unix_mailbox(
    path: &Path,
    _mailbox: &Arc<arreo_relay::Mailbox>,
    _threads: &mut Vec<std::thread::JoinHandle<()>>,
) {
    eprintln!(
        "arreo-relay: --pairing-socket {} is not available on this platform (there are no unix \
         sockets on Windows). Use --pairing-tcp HOST:PORT, which serves the same mailbox on \
         every platform",
        path.display()
    );
    std::process::exit(2);
}

/// The router: QUIC on `--listen`, plus any pairing listeners asked for.
fn serve(args: &[String]) {
    let mut listen = arreo_relay::router::DEFAULT_LISTEN.to_string();
    let mut state_dir: Option<PathBuf> = None;
    let mut pairing_socket: Option<PathBuf> = None;
    let mut pairing_tcp: Option<String> = None;
    let mut ttl_days = arreo_relay::DEFAULT_TTL_DAYS;
    let mut max_messages = arreo_relay::DEFAULT_MAX_MESSAGES;
    let mut max_mb = arreo_relay::DEFAULT_MAX_MB;

    let mut args = args.iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--listen" => match args.next() {
                Some(value) => listen = value.clone(),
                None => usage_error("--listen needs an address"),
            },
            "--state-dir" => state_dir = args.next().map(PathBuf::from),
            "--pairing-socket" => pairing_socket = args.next().map(PathBuf::from),
            "--pairing-tcp" => pairing_tcp = args.next().cloned(),
            "--inbox-ttl-days" => ttl_days = number(args.next(), "--inbox-ttl-days"),
            "--inbox-max-messages" => max_messages = number(args.next(), "--inbox-max-messages"),
            "--inbox-max-mb" => max_mb = number(args.next(), "--inbox-max-mb"),
            other => usage_error(&format!("unknown flag {other}")),
        }
    }
    let Some(state_dir) = state_dir else {
        usage_error("serve needs --state-dir (the router's SQLite file lives there)");
    };
    let addr: std::net::SocketAddr = match listen.parse() {
        Ok(addr) => addr,
        Err(e) => usage_error(&format!(
            "--listen {listen:?} is not an IP:PORT address: {e}"
        )),
    };
    if let Err(e) = std::fs::create_dir_all(&state_dir) {
        eprintln!("arreo-relay: cannot create {}: {e}", state_dir.display());
        std::process::exit(1);
    }
    let db = state_dir.join("relay.db");
    let store = match arreo_relay::RelayStore::open(&db) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("arreo-relay: cannot open {}: {e}", db.display());
            std::process::exit(1);
        }
    };

    eprintln!("arreo-relay: state {}", db.display());

    // The pairing mailbox, if asked for, runs on its own blocking threads: it is
    // deliberately independent of the router (a device pairs before it has a
    // certificate to route with).
    let mailbox = Arc::new(arreo_relay::Mailbox::new());
    let mut pairing_threads = Vec::new();
    if let Some(path) = pairing_socket {
        spawn_unix_mailbox(&path, &mailbox, &mut pairing_threads);
    }
    if let Some(addr) = pairing_tcp {
        let mailbox = Arc::clone(&mailbox);
        eprintln!("arreo-relay: pairing mailbox on tcp://{addr}");
        pairing_threads.push(std::thread::spawn(move || {
            if let Err(e) = arreo_relay::pairing::serve_tcp(&addr, mailbox) {
                eprintln!("arreo-relay: tcp listener died: {e}");
                std::process::exit(1);
            }
        }));
    }

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("arreo-relay: cannot start the runtime: {e}");
            std::process::exit(1);
        }
    };
    let limits = match arreo_relay::InboxLimits::from_options(ttl_days, max_messages, max_mb) {
        Ok(limits) => limits,
        Err(e) => usage_error(&e.to_string()),
    };
    eprintln!(
        "arreo-relay: inbox retention {} day(s), bounds {} message(s) / {} MiB per device",
        ttl_days, max_messages, max_mb
    );
    let router = Arc::new(arreo_relay::Router::new(store, limits));
    // The endpoint is created *inside* the runtime: quinn needs a reactor, and
    // building it before `block_on` fails with "no async runtime found".
    let result = runtime.block_on(async move {
        let endpoint = arreo_core::transport::server_endpoint(addr)
            .map_err(|e| format!("cannot listen on {addr}: {e}"))?;
        // Report the address actually bound, not the one requested: `--listen
        // 127.0.0.1:0` is how a caller asks the OS for a free port, and it can
        // only use the answer if we print it.
        let bound = endpoint.local_addr().unwrap_or(addr);
        // A non-loopback listener is a decision, so it is stated rather than
        // silently allowed: the relay authenticates devices by pinned
        // certificate and never reads what it routes, but the port is still
        // reachable by anyone who can route to it.
        eprintln!(
            "arreo-relay: router on {bound} — {}",
            arreo_relay::router::describe_listen(bound)
        );
        arreo_relay::router::serve(endpoint, router)
            .await
            .map_err(|e| format!("router stopped: {e}"))
    });
    if let Err(e) = result {
        eprintln!("arreo-relay: {e}");
        std::process::exit(1);
    }
    for thread in pairing_threads {
        let _ = thread.join();
    }
}

/// The operator's door to the account registry.
///
/// Deliberately a command and not an implicit step: an account's root key is the
/// anchor every device certificate in it is verified against, so registering one
/// is a decision a human makes once, not something the relay infers from
/// traffic. (The pairing flow will register accounts for real; until then this
/// is how a self-hoster brings one up.)
fn account(args: &[String]) {
    let mut state_dir: Option<PathBuf> = None;
    let mut account_id: Option<String> = None;
    let mut root_key: Option<String> = None;
    let mut args = args.iter();
    let verb = args.next().map(String::as_str);
    if verb != Some("add") {
        usage_error("account needs the subcommand `add`");
    }
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--state-dir" => state_dir = args.next().map(PathBuf::from),
            "--account" => account_id = args.next().cloned(),
            "--root-key" => root_key = args.next().cloned(),
            other => usage_error(&format!("unknown flag {other}")),
        }
    }
    let (Some(state_dir), Some(account_id), Some(root_key)) = (state_dir, account_id, root_key)
    else {
        usage_error("account add needs --state-dir, --account and --root-key");
    };
    let key = match hex_to_key(&root_key) {
        Some(key) => key,
        None => usage_error("--root-key must be 64 hex characters (an ed25519 public key)"),
    };
    if let Err(e) = std::fs::create_dir_all(&state_dir) {
        eprintln!("arreo-relay: cannot create {}: {e}", state_dir.display());
        std::process::exit(1);
    }
    let store = match arreo_relay::RelayStore::open(&state_dir.join("relay.db")) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("arreo-relay: cannot open the state dir: {e}");
            std::process::exit(1);
        }
    };
    let directory = arreo_relay::Directory::new(store);
    let now = arreo_relay::directory::now_ms();
    if let Err(e) = directory.create_account(&account_id, &key, now) {
        eprintln!("arreo-relay: cannot register {account_id}: {e}");
        std::process::exit(1);
    }
    println!(
        "registered account {account_id} with root key {}",
        root_key.to_lowercase()
    );
}

/// `arreo-relay audit export|prune` (T-0053): read or trim the relay's own trail.
///
/// The relay has no `--config` and no `--socket` — its state directory is the
/// whole address of its database — so these verbs take `--state-dir` like `serve`
/// and `account add` do, rather than inventing a second way to name the same file.
fn audit(args: &[String]) {
    let mut state_dir: Option<PathBuf> = None;
    let mut db: Option<PathBuf> = None;
    let mut format = arreo_core::store::ExportFormat::Jsonl;
    let mut since: Option<u64> = None;
    let mut until: Option<u64> = None;
    let mut action: Option<String> = None;
    let mut out: Option<String> = None;
    let mut before: Option<u64> = None;
    let mut args = args.iter();
    let verb = args.next().map(String::as_str);
    if verb != Some("export") && verb != Some("prune") {
        usage_error("audit needs a subcommand: `export` or `prune`");
    }
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--state-dir" => state_dir = args.next().map(PathBuf::from),
            "--db" => db = args.next().map(PathBuf::from),
            "--format" => {
                let value = args
                    .next()
                    .unwrap_or_else(|| usage_error("--format needs a value"));
                format = match arreo_core::store::ExportFormat::parse(value) {
                    Some(format) => format,
                    None => usage_error("--format is jsonl or json"),
                };
            }
            // Bare Unix milliseconds, exactly as `arreo audit export` takes them:
            // the two logs share one filter vocabulary, so a script that works on
            // one works on the other.
            "--since" => since = Some(number(args.next(), "--since")),
            "--until" => until = Some(number(args.next(), "--until")),
            "--action" => action = args.next().cloned(),
            "--out" => out = args.next().cloned(),
            "--before" => before = Some(number(args.next(), "--before")),
            other => usage_error(&format!("unknown flag {other}")),
        }
    }
    let Some(state_dir) = state_dir.or(db) else {
        usage_error("audit needs --state-dir DIR (or --db PATH)");
    };
    let path = if state_dir.extension().is_some_and(|ext| ext == "db") {
        state_dir
    } else {
        state_dir.join("relay.db")
    };
    let store = match arreo_relay::RelayStore::open(&path) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("arreo-relay: cannot open {}: {e}", path.display());
            std::process::exit(1);
        }
    };
    match verb {
        Some("prune") => {
            let Some(before) = before else {
                usage_error("audit prune needs --before MS (the cutoff, in Unix milliseconds)");
            };
            match store.audit_prune(before) {
                Ok(removed) => println!("pruned {removed} row(s)"),
                Err(e) => {
                    eprintln!("arreo-relay: prune failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            let query = arreo_core::store::AuditQuery {
                since_ms: since,
                until_ms: until,
                action,
                // The whole window: the export is a view of the trail, and a
                // silent cap would make it a partial one.
                limit: i64::MAX as usize,
            };
            let text = match store.audit_export(&query, format) {
                Ok(text) => text,
                Err(e) => {
                    eprintln!("arreo-relay: export failed: {e}");
                    std::process::exit(1);
                }
            };
            match out.as_deref() {
                None | Some("-") => print!("{text}"),
                Some(path) => match std::fs::write(path, &text) {
                    Ok(()) => println!("exported {path} ({})", format.as_str()),
                    Err(e) => {
                        eprintln!("arreo-relay: audit export: {path}: {e}");
                        std::process::exit(1);
                    }
                },
            }
        }
    }
}

/// Parse 64 hex characters into a verifying key.
fn hex_to_key(hex: &str) -> Option<arreo_core::identity::VerifyingKey> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (index, chunk) in hex.as_bytes().chunks(2).enumerate() {
        let hi = (chunk[0] as char).to_digit(16)?;
        let lo = (chunk[1] as char).to_digit(16)?;
        bytes[index] = (hi * 16 + lo) as u8;
    }
    arreo_core::identity::VerifyingKey::from_bytes(&bytes).ok()
}

/// Parse a numeric flag, refusing a missing or non-numeric value loudly.
fn number(value: Option<&String>, flag: &str) -> u64 {
    match value.and_then(|v| v.parse::<u64>().ok()) {
        Some(number) => number,
        None => usage_error(&format!("{flag} needs a positive whole number")),
    }
}

fn usage_error(message: &str) -> ! {
    eprintln!("arreo-relay: {message}");
    std::process::exit(2);
}
