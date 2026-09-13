//! `arreo machines` — the account's machine directory, read from the relay
//! (T-0044).
//!
//! **One implementation per fact.** The rows come from the relay's `machines`
//! request (T-0056), presence is the relay's rule (T-0043), the `[relay]`
//! configuration is `arreo_core::relay::config` — the same type and the same
//! parser the daemon uses, because the dependency rule forbids this crate
//! depending on `arreo-server` and a second parser would be a second answer to
//! "what is a valid `[relay]` section".
//!
//! **The CLI talks to the relay itself, not through the daemon.** It holds the
//! same device identity the daemon does (`identity/device.key` plus the
//! certificate `arreo pair` saved), so reading the directory needs no socket
//! verb, no daemon running, and no new protocol surface: one dial and one
//! request.
//!
//! **The cache is a mirror, never a source.** When the relay cannot be reached,
//! `list` and `status` print what the last successful contact said, labelled
//! `source: "cache"` — never silently promoting a remembered row to "online",
//! because a cached row has not been observed and claiming otherwise is the one
//! lie this file exists to prevent (T-0043's read-only mirror invariant).

use crate::ExitCode;
use arreo_core::identity::{DeviceId, Role};
use arreo_core::mesh::{CachedMachine, DirectoryCache, MachineId, MachineRow, Name, Presence};
use arreo_core::mesh::{GrantedDevice, TrustLedger};
use std::path::PathBuf;

/// Exit codes, stated in `--help` and stable (T-0044's contract).
pub const OK: u8 = 0;
pub const USAGE: u8 = 2;
pub const UNKNOWN_MACHINE: u8 = 3;
pub const UNREACHABLE: u8 = 4;
pub const CONFLICT: u8 = 5;
/// A store or identity this command needed and could not read. Distinct from
/// [`UNREACHABLE`], which is about the network: this one never touched it.
pub const FAILURE: u8 = 1;

/// The JSON schema version. Additive-only: the contract test fails on a removed
/// or renamed key and on an out-of-enum value, so breaking a script is a red
/// build rather than a surprise.
pub const SCHEMA: u32 = 1;

/// Dispatch `arreo machines <verb>`.
pub fn run(rest: &[String]) -> ExitCode {
    let Some(verb) = rest.first().map(String::as_str) else {
        return usage();
    };
    let args = &rest[1..];
    match verb {
        "list" => crate::rt::block_on(list(args)),
        "status" => crate::rt::block_on(status(args)),
        "rename" => crate::rt::block_on(rename(args)),
        "remove" => crate::rt::block_on(remove(args)),
        "add" => crate::rt::block_on(add(args)),
        // Local, not over the relay: trust is this machine's own decision, and
        // the operator administering it is the machine acting (see the task's
        // "The write path, decided").
        "trust" => trust(args),
        other => {
            eprintln!("machines: unknown verb {other:?}");
            usage()
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: arreo machines list   [--json] [--all] [--offline] [--config PATH]");
    eprintln!("       arreo machines status [<name>] [--json] [--offline] [--config PATH]");
    eprintln!("       arreo machines rename <old> <new> [--config PATH]");
    eprintln!("       arreo machines remove <name> [--stale] [--force] [--config PATH]");
    eprintln!("       arreo machines add <pairing-code> --uri <invite> [--name N]");
    eprintln!(
        "       arreo machines trust <device> [--machine <name>] [--role viewer|operator] [--yes]"
    );
    eprintln!("       arreo machines trust --list [--json]     (who may use *this* machine)");
    eprintln!();
    eprintln!(
        "  --json     the script contract (schema {SCHEMA}); the human table is NOT one and may"
    );
    eprintln!("             change at any time — parse --json, never the table");
    eprintln!("  --all      include names whose machines are gone (tombstoned)");
    eprintln!("  --offline  never contact the relay; answer from the last known rows (exit 0)");
    eprintln!("  --config   the file with the [relay] section (also $ARREO_CONFIG)");
    eprintln!("  --stale    remove every machine the presence rule calls stale (T-0043's rule)");
    eprintln!(
        "  --uri      (add) the invite the admitting machine printed: it carries the mailbox,"
    );
    eprintln!("             the session, its key, and the account and relay to join");
    eprintln!("  --force    tombstone a machine that is answering right now (asks once otherwise)");
    eprintln!("  --machine  which machine's trust to change: this one, or the command is refused");
    eprintln!("  --role     viewer (observe) or operator (also drive); defaults to viewer");
    eprintln!(
        "  --yes      skip the confirmation (for scripts); without it the device is shown first"
    );
    eprintln!("  --socket   the machine's socket, whose store holds its trust ledger");
    eprintln!();

    eprintln!(
        "exit codes: {OK} ok · {USAGE} usage · {UNKNOWN_MACHINE} unknown machine · \
         {UNREACHABLE} relay unreachable · {CONFLICT} name conflict or trust refusal"
    );
    ExitCode::from(USAGE)
}

/// What the caller asked for, parsed once.
struct Options {
    json: bool,
    all: bool,
    offline: bool,
    stale: bool,
    force: bool,
    config: Option<PathBuf>,
    /// `--name`: the name to use, for the one verb that takes a name as a flag
    /// rather than positionally (`add`, where the positional is the code).
    name_flag: Option<String>,
    /// `--uri`: the pairing invite (`add` only).
    uri: Option<String>,
    /// The positional argument, when the verb takes one — or, for `rename`,
    /// the first of the two.
    name: Option<String>,
    second: Option<String>,
}

fn parse(verb: &str, args: &[String], allow_name: bool) -> Result<Options, ExitCode> {
    let mut options = Options {
        json: false,
        all: false,
        offline: false,
        stale: false,
        force: false,
        config: None,
        name_flag: None,
        uri: None,
        name: None,
        second: None,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => options.json = true,
            "--all" => options.all = true,
            "--offline" => options.offline = true,
            "--stale" => options.stale = true,
            "--force" => options.force = true,
            "--config" if i + 1 < args.len() => {
                options.config = Some(PathBuf::from(&args[i + 1]));
                i += 2;
                continue;
            }
            "--name" if i + 1 < args.len() => {
                options.name_flag = Some(args[i + 1].clone());
                i += 2;
                continue;
            }
            "--uri" if i + 1 < args.len() => {
                options.uri = Some(args[i + 1].clone());
                i += 2;
                continue;
            }
            other if !other.starts_with('-') && allow_name => {
                if options.name.is_none() {
                    options.name = Some(other.to_string());
                } else if options.second.is_none() {
                    options.second = Some(other.to_string());
                } else {
                    eprintln!("machines {verb}: unexpected argument {other:?}");
                    return Err(usage());
                }
            }
            other => {
                eprintln!("machines {verb}: unexpected argument {other:?}");
                return Err(usage());
            }
        }
        i += 1;
    }
    Ok(options)
}

/// One row, as the JSON envelope and the table both see it.
///
/// A single shape for both renderers, so the human table can never show
/// something the script contract does not have (or the reverse): the difference
/// between them is formatting, and formatting is where a second source of truth
/// would hide.
#[derive(Clone)]
struct Row {
    name: String,
    machine_id: String,
    presence: Presence,
    last_seen_ms: i64,
    proto_version: u32,
    name_conflict: bool,
    /// When the name's tombstone expires, when the relay said there is one.
    tombstone_until_ms: Option<i64>,
    /// Whether this machine has published the key a peer dials (T-0045): a name
    /// with one can be reached by `arreo attach --machine`, and one without is in
    /// the directory but dialable by nobody. A boolean rather than the key itself —
    /// a script asking "can I reach this" wants an answer, not key material, and
    /// the CLI resolves the key internally.
    reachable: bool,
    /// Whether this row came from the cache rather than the relay: the flag that
    /// says "unverified" travels with it, so a script sees the same distinction
    /// the `source` field makes for the whole answer.
    from_cache: bool,
}

impl Row {
    fn from_relay(row: &MachineRow) -> Self {
        Self {
            name: row.name.as_str().to_string(),
            machine_id: row.machine_id.as_str().to_string(),
            // The relay computed presence against *its* clock; recomputing here
            // would be a second presence rule.
            presence: row.presence,
            last_seen_ms: row.last_seen_ms,
            proto_version: row.proto_version,
            name_conflict: row.name_conflict,
            tombstone_until_ms: row.tombstone_until_ms,
            reachable: row.daemon_key.is_some(),
            from_cache: false,
        }
    }

    /// A remembered row.
    ///
    /// **`online` becomes `offline` here, and that is the point.** A cached row
    /// is something the relay told us earlier; liveness is the one claim the CLI
    /// cannot substantiate with the relay unreachable, and printing `online` for
    /// a machine that died an hour ago is the single lie this file exists to
    /// prevent. The row still says *when* it was last seen (and how long ago), so
    /// nothing is hidden — only the unverifiable claim is refused. `stale` stays
    /// `stale`: it is a statement about the past that cannot become wrong by
    /// waiting.
    fn from_cache(name: &str, cached: &CachedMachine) -> Self {
        Self {
            name: name.to_string(),
            machine_id: cached.machine_id.as_str().to_string(),
            presence: match cached.presence {
                Presence::Online => Presence::Offline,
                other => other,
            },
            last_seen_ms: cached.last_seen_ms,
            proto_version: cached.proto_version,
            name_conflict: cached.name_conflict,
            tombstone_until_ms: cached.tombstone_until_ms,
            // A cached row says nothing about reachability: nobody asked, and a key
            // asserted from memory would be a claim the relay did not make.
            reachable: false,
            from_cache: true,
        }
    }

    /// The age of the last observation, in seconds. Saturating and never
    /// negative: a relay and a client whose clocks disagree must not print an age
    /// below zero, which reads as a bug in the product rather than a skew.
    fn age_secs(&self, now_ms: i64) -> i64 {
        now_ms.saturating_sub(self.last_seen_ms).max(0) / 1000
    }

    /// What an operator needs to know about this row beyond its name.
    ///
    /// `name-tombstoned` comes from the *row's* tombstone, not from its
    /// presence: a machine removed a minute ago is not stale, and a renderer that
    /// showed it as an ordinary online machine would hide the fact that its name
    /// is reserved. `name-reclaimable` is the stale case, where the name can be
    /// taken by anyone.
    fn flags(&self, now_ms: i64) -> Vec<&'static str> {
        let mut flags = Vec::new();
        if self.name_conflict {
            flags.push("name-suffixed");
        }
        if self.tombstone_active(now_ms) {
            flags.push("name-tombstoned");
        } else if self.presence == Presence::Stale {
            flags.push("name-reclaimable");
        }
        if self.from_cache {
            flags.push("unverified");
        }
        flags
    }

    fn tombstone_active(&self, now_ms: i64) -> bool {
        self.tombstone_until_ms.is_some_and(|until| until > now_ms)
    }

    fn presence_str(&self) -> &'static str {
        self.presence.as_str()
    }
}

/// Where the last successful directory read is remembered.
///
/// The identity directory, beside the device key: the same place `arreo pair`
/// and the daemon keep this machine's account knowledge, so there is one home
/// for it.
fn cache_path() -> PathBuf {
    arreo_core::identity::identity_root().join("machines.cache")
}

fn load_cache() -> DirectoryCache {
    let path = cache_path();
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            // A corrupt cache is not worth failing over: it is a mirror, and the
            // relay is the source. Said once, then ignored, so a bad file cannot
            // make `--offline` silently answer nothing.
            eprintln!(
                "machines: ignoring an unreadable cache at {}: {e}",
                path.display()
            );
            DirectoryCache::new()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DirectoryCache::new(),
        Err(e) => {
            eprintln!("machines: cannot read {}: {e}", path.display());
            DirectoryCache::new()
        }
    }
}

fn store_cache(cache: &DirectoryCache) {
    let path = cache_path();
    let Ok(bytes) = serde_json::to_vec(cache) else {
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            eprintln!("machines: cannot create {}: {e}", parent.display());
            return;
        }
    }
    // Best effort: a cache that cannot be written costs the next offline read
    // its memory, not this one its answer.
    if let Err(e) = std::fs::write(&path, bytes) {
        eprintln!("machines: cannot write {}: {e}", path.display());
    }
}

/// Rows, and whether they came from the relay.
struct Answer {
    rows: Vec<Row>,
    from_cache: bool,
}

/// Read the directory: the relay when it answers, the last known rows when it
/// does not.
///
/// `--offline` is a *choice*, not a failure: it answers from the cache without
/// dialing and exits 0. Without it, an unreachable relay still prints the rows —
/// an operator asking "what do I know" deserves the answer even when the network
/// is down — and exits 4 so a script can tell the difference.
async fn read(options: &Options) -> Result<Answer, ExitCode> {
    if options.offline {
        return Ok(Answer {
            rows: cached_rows(),
            from_cache: true,
        });
    }
    let path = config_path(options)?;
    let settings = match arreo_core::relay::config::load_config(&path) {
        Ok(Some(settings)) => settings,
        Ok(None) => {
            eprintln!(
                "machines: no relay is configured ({}), so the account's directory cannot be read; \
                 --offline answers from what this machine remembers",
                path.display()
            );
            return Err(ExitCode::from(UNREACHABLE));
        }
        Err(e) => {
            eprintln!("machines: {e}");
            return Err(ExitCode::from(USAGE));
        }
    };
    let (key, cert) = match paired_identity() {
        Ok(pair) => pair,
        Err(message) => {
            eprintln!("machines: {message}");
            return Err(ExitCode::from(UNREACHABLE));
        }
    };
    let session = match arreo_core::relay::session::RelaySession::dial(
        settings.addr,
        &settings.account,
        &key,
        &cert,
    )
    .await
    {
        Ok(session) => session,
        Err(e) => {
            eprintln!("machines: cannot reach the relay at {}: {e}", settings.addr);
            return Ok(Answer {
                rows: cached_rows(),
                from_cache: true,
            });
        }
    };
    match session.machines(options.all).await {
        Ok(reply) => match (reply.refused, reply.machines) {
            (Some(reason), _) => {
                eprintln!("machines: the relay refused to answer: {reason}");
                Err(ExitCode::from(CONFLICT))
            }
            (None, rows) => {
                let now_ms = now_ms();
                let mut cache = DirectoryCache::new();
                cache.mirror(&rows, now_ms);
                store_cache(&cache);
                Ok(Answer {
                    rows: rows.iter().map(Row::from_relay).collect(),
                    from_cache: false,
                })
            }
        },
        Err(e) => {
            eprintln!("machines: the relay did not answer: {e}");
            Ok(Answer {
                rows: cached_rows(),
                from_cache: true,
            })
        }
    }
}

fn cached_rows() -> Vec<Row> {
    load_cache()
        .rows()
        .into_iter()
        .map(|(name, cached)| Row::from_cache(name, cached))
        .collect()
}

/// This machine's paired identity: the key it holds and the certificate the
/// server issued it.
///
/// The same files `arreo pair --join` wrote and the daemon's relay client reads,
/// so a machine has one identity for every path that needs one. The certificate
/// is named after the **bare** hex id (its own spelling), not the `dev_` display
/// form — that spelling mismatch has cost this project three debugging cycles
/// (T-0023, T-0029, T-0044).
fn paired_identity() -> Result<
    (
        arreo_core::identity::DeviceKey,
        arreo_core::identity::DeviceCert,
    ),
    String,
> {
    let root = arreo_core::identity::identity_root();
    let key_path = root.join("device.key");
    let key = arreo_core::identity::DeviceKey::load(&key_path).map_err(|e| {
        format!(
            "this machine has no paired identity ({}): {e}",
            key_path.display()
        )
    })?;
    let id = arreo_core::identity::DeviceId::from_key(&key.public());
    let cert_path = root.join("devices").join(format!("{}.cert", id.as_str()));
    let cert = arreo_core::identity::DeviceCert::load(&cert_path)
        .map_err(|e| format!("no certificate at {}: {e}", cert_path.display()))?;
    Ok((key, cert))
}

/// The configuration file to read: `--config`, then `$ARREO_CONFIG`.
///
/// **Deliberately no default path.** The daemon reads `--config`/`$ARREO_CONFIG`
/// and nothing else (T-0051), and a CLI that invented its own
/// `$XDG_CONFIG_HOME/arreo/arreo.toml` would be a second answer to "which
/// configuration is this machine's relay configuration" — the same fact with two
/// sources, which is the defect class this project keeps finding. If the daemon
/// ever grows a default, this follows it; until then the operator names the file
/// once, in the same way for both.
fn config_path(options: &Options) -> Result<PathBuf, ExitCode> {
    if let Some(path) = &options.config {
        return Ok(path.clone());
    }
    if let Some(path) = std::env::var_os("ARREO_CONFIG") {
        return Ok(PathBuf::from(path));
    }
    eprintln!(
        "machines: which configuration? pass --config PATH, or set ARREO_CONFIG (the [relay] \
         section names the relay and the account)"
    );
    Err(ExitCode::from(USAGE))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn rfc3339(ms: i64) -> String {
    arreo_core::store::rfc3339_ms(ms)
}

async fn list(args: &[String]) -> ExitCode {
    let options = match parse("list", args, false) {
        Ok(options) => options,
        Err(code) => return code,
    };
    if let Err(code) = refuse_unused(&options, "list") {
        return code;
    }
    let now = now_ms();
    let answer = match read(&options).await {
        Ok(answer) => answer,
        Err(code) => return code,
    };
    // Sorted here rather than trusting any producer: the envelope says sorted,
    // and the table is easier to read that way.
    let mut rows = answer.rows;
    rows.sort_by(|a, b| a.name.cmp(&b.name));

    if options.json {
        println!("{}", envelope(&rows, now, answer.from_cache));
    } else {
        print_table(&rows, now, answer.from_cache);
    }
    if answer.from_cache && !options.offline {
        // A cache hit without `--offline` is a factual failure: the operator
        // asked the account and got its memory. Exit 4 says so to a script.
        return ExitCode::from(UNREACHABLE);
    }
    ExitCode::from(OK)
}

async fn status(args: &[String]) -> ExitCode {
    let options = match parse("status", args, true) {
        Ok(options) => options,
        Err(code) => return code,
    };
    if let Err(code) = refuse_unused(&options, "status") {
        return code;
    }
    let now = now_ms();
    let answer = match read(&options).await {
        Ok(answer) => answer,
        Err(code) => return code,
    };
    let rows = answer.rows;

    let selected: Vec<&Row> = match &options.name {
        None => rows.iter().collect(),
        Some(wanted) => {
            let found: Vec<&Row> = rows.iter().filter(|row| row.name == *wanted).collect();
            if found.is_empty() {
                // A name that is not a name and a name that is simply not there
                // are the same answer to the operator (exit 3), but only one of
                // them is worth explaining.
                match Name::parse(wanted) {
                    Ok(_) => eprintln!("machines: no machine named {wanted:?} in this account"),
                    Err(e) => eprintln!("machines: {wanted:?} is not a machine name: {e}"),
                }
                return ExitCode::from(UNKNOWN_MACHINE);
            }
            found
        }
    };

    if options.json {
        let owned: Vec<Row> = selected.into_iter().cloned().collect();
        println!("{}", envelope(&owned, now, answer.from_cache));
    } else {
        for row in &selected {
            print_status(row, now, answer.from_cache);
        }
    }
    if answer.from_cache && !options.offline {
        return ExitCode::from(UNREACHABLE);
    }
    ExitCode::from(OK)
}

/// Refuse a flag this verb does not read.
///
/// The parser knows every flag; a verb reads a subset. Accepting one and
/// ignoring it is the failure mode this guards: the caller believes something
/// happened. Every verb that does not read `--name`/`--uri` calls this.
fn refuse_unused(options: &Options, verb: &str) -> Result<(), ExitCode> {
    for (present, flag) in [
        (options.name_flag.is_some(), "--name"),
        (options.uri.is_some(), "--uri"),
    ] {
        if present {
            eprintln!("machines {verb}: {flag} belongs to another verb");
            return Err(usage());
        }
    }
    Ok(())
}

/// A flag that only makes sense for the read verbs, if the caller passed one.
///
/// `--json` is the read verbs' contract (the write verbs print one line, which is
/// not a contract), and `--offline` means "answer from memory", which a write
/// cannot do. Refusing them is the honest answer: accepting a flag and ignoring
/// it looks like it worked.
fn read_only_flag(options: &Options) -> Option<&'static str> {
    if options.json {
        Some("--json")
    } else if options.offline {
        Some("--offline")
    } else {
        None
    }
}

/// The local machine's identity and name, derived from the socket it serves.
///
/// One source for "which machine am I", shared by every trust verb: the root key
/// (T-0056's `MachineId::from_key`), and the same default name the directory row
/// uses. A machine that could not agree with itself about its own identity would
/// write grants nothing else could match.
fn local_machine(socket: &std::path::Path) -> Result<(MachineId, String, PathBuf), String> {
    let layout = arreo_core::identity::authority::Layout::for_socket(socket);
    let root = arreo_core::identity::RootKey::load_or_generate(&layout.root_key).map_err(|e| {
        format!(
            "cannot read this machine's key ({}): {e}",
            layout.root_key.display()
        )
    })?;
    Ok((
        MachineId::from_key(&root.public()),
        arreo_core::mesh::default_machine_name(),
        layout.store,
    ))
}

/// This machine's trust ledger, for a local administrative command.
fn local_ledger(socket: &std::path::Path) -> Result<TrustLedger, String> {
    let layout = arreo_core::identity::authority::Layout::for_socket(socket);
    TrustLedger::open(
        &layout.store,
        &layout.root_key,
        arreo_core::mesh::default_machine_name(),
    )
    .map_err(|e| e.to_string())
}

/// `arreo machines trust …`: extend, or list, this machine's grants (T-0059).
///
/// The command T-0046's refusals already tell operators to run — it exists so
/// that advice is followable. It is deliberately **local**: trust is this
/// machine's decision, there is no device-facing verb that can change it, and
/// `--machine <other>` is refused rather than forwarded, because neither another
/// machine nor the relay may grant here on someone else's behalf.
/// What `machines trust` was asked to do, parsed once.
struct TrustOptions {
    list: bool,
    machine: Option<String>,
    role: Option<String>,
    assume_yes: bool,
    json: bool,
    device: Option<String>,
}

fn trust(args: &[String]) -> ExitCode {
    let (socket, kept) = crate::take_socket(args);
    let mut options = TrustOptions {
        list: false,
        machine: None,
        role: None,
        assume_yes: false,
        json: false,
        device: None,
    };

    let mut i = 0;
    while i < kept.len() {
        match kept[i].as_str() {
            "--list" => options.list = true,
            "--yes" | "-y" => options.assume_yes = true,
            "--json" => options.json = true,
            "--machine" if i + 1 < kept.len() => {
                options.machine = Some(kept[i + 1].clone());
                i += 2;
                continue;
            }
            "--role" if i + 1 < kept.len() => {
                options.role = Some(kept[i + 1].clone());
                i += 2;
                continue;
            }
            other if !other.starts_with('-') && options.device.is_none() => {
                options.device = Some(other.to_string());
            }
            other => {
                eprintln!("machines trust: unexpected argument {other:?}");
                return usage();
            }
        }
        i += 1;
    }

    if options.list && options.device.is_some() {
        eprintln!("machines trust: pass a device to grant, or --list to see the grants — not both");
        return usage();
    }
    if !options.list && options.device.is_none() {
        eprintln!("machines trust: nothing to do — pass a device to grant, or --list");
        return usage();
    }
    let device = match &options.device {
        None => None,
        Some(raw) => match DeviceId::parse(raw) {
            Ok(device) => Some(device),
            Err(e) => {
                eprintln!("machines trust: {raw:?} is not a device id: {e}");
                return ExitCode::from(UNKNOWN_MACHINE);
            }
        },
    };
    let role = match options.role.as_deref() {
        // Least privilege by default: a grant is easy to widen and awkward to
        // notice, so the safe direction is the one that has to be asked for.
        None => Role::Viewer,
        Some(text) => match Role::parse(text) {
            Ok(role) => role,
            Err(_) => {
                eprintln!("machines trust: --role takes viewer or operator (got {text:?})");
                return usage();
            }
        },
    };

    let (this_machine, machine_name, _) = match local_machine(&socket) {
        Ok(local) => local,
        Err(message) => {
            eprintln!("machines trust: {message}");
            return ExitCode::from(FAILURE);
        }
    };

    // `--machine` names whose trust is being changed, and only this machine's can
    // be. A remote name is refused rather than silently redirected to the local
    // ledger: acting on the wrong machine quietly is worse than not acting.
    if let Some(named) = &options.machine {
        if !machine_matches(named, &this_machine, &machine_name) {
            eprintln!(
                "machines trust: {named:?} is not this machine (which is {machine_name:?}, \
                 {}). Trust is local: no machine — and not the relay — can grant on another's \
                 behalf. Run this on {named} itself.",
                this_machine.as_str()
            );
            return ExitCode::from(CONFLICT);
        }
    }

    let ledger = match local_ledger(&socket) {
        Ok(ledger) => ledger,
        Err(message) => {
            eprintln!("machines trust: {message}");
            return ExitCode::from(FAILURE);
        }
    };

    if options.list {
        return list_grants(&ledger, &this_machine, &machine_name, options.json);
    }
    let device = device.expect("checked above: not --list means a device was given");

    // A grant for a device this machine has never pinned is inert: the Noise
    // handshake resolves the peer from the pin list, so it could never open a
    // session to use the grant. Refusing is kinder than writing a row that does
    // nothing, and it catches a mistyped fingerprint — the mistake this command
    // is most likely to see. Checked *before* the confirmation, so the operator is
    // never asked to confirm something that cannot work.
    if !pinned_here(&socket, &device) {
        eprintln!(
            "machines trust: {} is not pinned on this machine, so a grant would do nothing \
             (it could not authenticate here). Pin it first: `arreo devices issue --key …` \
             on this machine, or `arreo pair` on the device.",
            device.display_id()
        );
        return ExitCode::from(UNKNOWN_MACHINE);
    }

    // The fingerprint before the act: a mistyped id is visible while it is still
    // cheap to stop.
    let existing = ledger.devices().unwrap_or_default();
    let previous = existing.iter().find(|row| row.device == device);
    if !options.assume_yes {
        println!(
            "grant {} ({}) {} access to this machine ({machine_name})?",
            device.display_id(),
            describe_previous(previous),
            role.operator_term()
        );
        if !confirm() {
            eprintln!("machines trust: not confirmed; nothing changed");
            return ExitCode::from(CONFLICT);
        }
    }

    match ledger.grant(&device, role, &device, crate::now_ms_i64()) {
        Ok(_) => {
            if options.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "granted": true,
                        "device": device.display_id(),
                        "role": role.operator_term(),
                        "machine": machine_name,
                        "machine_id": this_machine.as_str(),
                    })
                );
            } else {
                println!(
                    "{} may now {} on {machine_name}",
                    device.display_id(),
                    describe_role(role)
                );
            }
            ExitCode::from(OK)
        }
        Err(e) => {
            eprintln!("machines trust: the grant could not be recorded: {e}");
            ExitCode::from(FAILURE)
        }
    }
}

/// Does a `--machine` value name this machine? By id or by name, because both are
/// things an operator has in hand.
fn machine_matches(named: &str, this_machine: &MachineId, this_name: &str) -> bool {
    named == this_name || named.eq_ignore_ascii_case(this_name) || named == this_machine.as_str()
}

/// Is this device pinned in this machine's authority?
fn pinned_here(socket: &std::path::Path, device: &DeviceId) -> bool {
    match crate::open_authority(socket) {
        Ok(authority) => authority
            .devices()
            .iter()
            .any(|record| &record.id == device),
        // A store that cannot be read is not "pinned": the grant would be inert
        // anyway, and the caller reports the failure separately.
        Err(_) => false,
    }
}

fn describe_role(role: Role) -> &'static str {
    match role {
        // The roadmap's word for the operator role (ADR 0019): `owner` is what
        // the certificates say, `operator` is what a human says.
        Role::Owner => "observe and drive",
        Role::Viewer => "observe",
    }
}

/// What this machine already thinks of the device, for the confirmation line.
fn describe_previous(previous: Option<&GrantedDevice>) -> String {
    match previous {
        None => "no grant here yet".to_string(),
        Some(row) if row.is_live() => format!("currently {}", row.role.operator_term()),
        Some(row) => format!(
            "grant revoked at {}",
            arreo_core::store::rfc3339_ms(row.revoked_at_ms.unwrap_or_default())
        ),
    }
}

/// Ask on stdin. Reads one line and requires an explicit yes.
///
/// Deliberately not a default-yes: this writes an authorization, and a command
/// that acts when a human hits enter without reading is how the wrong device gets
/// trusted. `--yes` exists for scripts, which opt in knowingly.
fn confirm() -> bool {
    use std::io::Write as _;
    print!("type 'yes' to confirm: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "yes" | "y")
}

/// `machines trust --list`: who may use this machine.
fn list_grants(
    ledger: &TrustLedger,
    this_machine: &MachineId,
    machine_name: &str,
    json: bool,
) -> ExitCode {
    let rows = match ledger.devices() {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("machines trust: cannot read this machine's grants: {e}");
            return ExitCode::from(FAILURE);
        }
    };
    let now = crate::now_ms_i64();
    if json {
        println!(
            "{}",
            grants_envelope(&rows, machine_name, this_machine, now)
        );
        return ExitCode::from(OK);
    }
    if rows.is_empty() {
        println!("(no device is trusted on {machine_name})");
        println!();
        println!("grant one with: arreo machines trust <device> --role viewer --yes");
        return ExitCode::from(OK);
    }
    println!("DEVICE                              ROLE     GRANTED              BY                                  STATE");
    for row in &rows {
        println!(
            "{:<35} {:<8} {:<20} {:<35} {}",
            row.device.display_id(),
            row.role.operator_term(),
            arreo_core::store::rfc3339_ms(row.granted_at_ms),
            row.granted_by.display_id(),
            match row.revoked_at_ms {
                None => "live".to_string(),
                Some(at) => format!("revoked {}", arreo_core::store::rfc3339_ms(at)),
            }
        );
    }
    ExitCode::from(OK)
}

/// The `trust --list --json` contract.
///
/// Its own document with its own version: `machines list` describes the account's
/// machines (from the relay) and this describes one machine's grants (from its
/// own store). Two different questions with two different sources, so sharing one
/// envelope would mean fields that are meaningless in half the cases.
fn grants_envelope(
    rows: &[GrantedDevice],
    machine_name: &str,
    machine_id: &MachineId,
    now_ms: i64,
) -> serde_json::Value {
    let grants: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "device": row.device.display_id(),
                "role": row.role.operator_term(),
                "granted_at": arreo_core::store::rfc3339_ms(row.granted_at_ms),
                "granted_by": row.granted_by.display_id(),
                "revoked_at": row.revoked_at_ms.map(arreo_core::store::rfc3339_ms),
                "live": row.is_live(),
            })
        })
        .collect();
    serde_json::json!({
        "schema": SCHEMA,
        "machine": machine_name,
        "machine_id": machine_id.as_str(),
        "as_of": arreo_core::store::rfc3339_ms(now_ms),
        "grants": grants,
    })
}

/// `arreo machines add <pairing-code> --uri <invite>`: join an account.
///
/// This runs on the machine being **admitted** — the one that has no identity
/// yet — which is why the invite carries the account and relay (T-0058): it
/// cannot read them from its own configuration, and the machine that admits it
/// (holding the account root, which is the only key that can issue a certificate
/// the relay will accept) is the one that put them there. The exchange is the
/// same SPAKE2 pairing `arreo pair --join` runs; what `add` does afterwards is
/// assert this machine's own directory row (T-0056) and report what the
/// directory granted.
async fn add(args: &[String]) -> ExitCode {
    let options = match parse("add", args, true) {
        Ok(options) => options,
        Err(code) => return code,
    };
    // The positional is the code here, and the name travels as a flag — so the
    // slots the other verbs use positionally must be empty, and saying which one
    // is extra beats reading the wrong word as a code.
    if options.second.is_some() {
        eprintln!(
            "machines add: too many arguments (usage: machines add <pairing-code> --uri <invite>)"
        );
        return usage();
    }
    let Some(code) = options.name.clone() else {
        eprintln!("machines add: needs the pairing code the admitting machine displayed");
        return usage();
    };
    let Some(uri) = options.uri.clone() else {
        eprintln!(
            "machines add: needs --uri (the invite the admitting machine printed, which carries \
             the mailbox, the session and its key)"
        );
        return usage();
    };
    if let Some(flag) = read_only_flag(&options) {
        eprintln!(
            "machines add: {flag} belongs to the read verbs; joining must reach the admitting \
             machine"
        );
        return ExitCode::from(USAGE);
    }

    // The pairing exchange, and the certificate. Only success writes anything.
    let (paired, invite) = match crate::join_pairing(&code, &uri, options.name_flag.clone()) {
        Ok(pair) => pair,
        Err(crate::PairError::Usage(message)) => {
            eprintln!("machines add: {message}");
            return usage();
        }
        Err(crate::PairError::Failed(message)) => {
            eprintln!("machines add: {message}");
            return ExitCode::FAILURE;
        }
    };
    let Some(directory) = invite.directory else {
        eprintln!(
            "machines add: this invite names no account and relay, so there is nothing to join \
             (the admitting machine printed it without a [relay] configuration). Ask it to run \
             `arreo pair` again, with its relay configured, or use `arreo pair --join` for an \
             ordinary pairing"
        );
        return ExitCode::from(UNREACHABLE);
    };
    let addr: std::net::SocketAddr = match directory.relay.parse() {
        Ok(addr) => addr,
        Err(e) => {
            eprintln!(
                "machines add: the invite names the relay as {:?}, which is not an IP:PORT \
                 address: {e}",
                directory.relay
            );
            return ExitCode::from(USAGE);
        }
    };

    // Register with the relay using the identity just issued, then assert this
    // machine's row under its **own** root key: the row is keyed by a key this
    // machine holds (T-0056), so being admitted and being registered are two
    // distinct things and only the second makes it visible to the account.
    let session = match arreo_core::relay::session::RelaySession::dial(
        addr,
        &directory.account,
        &paired.key,
        &paired.cert,
    )
    .await
    {
        Ok(session) => session,
        Err(e) => {
            eprintln!(
                "machines add: joined the account's devices, but cannot reach the relay at \
                 {addr} to register this machine: {e}"
            );
            return ExitCode::from(UNREACHABLE);
        }
    };
    let root = match arreo_core::identity::RootKey::load_or_generate(&crate::root_key_path()) {
        Ok(root) => root,
        Err(e) => {
            eprintln!("machines add: cannot read this machine's key: {e}");
            return ExitCode::FAILURE;
        }
    };
    let fingerprint = arreo_core::mesh::MachineId::from_key(&root.public());
    let requested = options
        .name_flag
        .clone()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(arreo_core::mesh::default_machine_name);
    let machine_key = root.public_hex();
    let payload = arreo_core::relay::join_proof_payload(
        session.nonce(),
        &session.account(),
        &machine_key,
        &requested,
    );
    let request = arreo_core::relay::JoinRequest {
        v: arreo_core::relay::RELAY_VERSION,
        name: requested.clone(),
        proto_version: arreo_core::proto::VERSION,
        machine_key,
        signature: root.sign(&payload).to_bytes().to_vec(),
    };
    match session.join_machine(request).await {
        Ok(reply) => match (reply.refused, reply.granted) {
            (Some(reason), _) => {
                eprintln!("machines add: the relay would not register this machine: {reason}");
                ExitCode::from(CONFLICT)
            }
            (None, Some(row)) => {
                let granted = row.name.as_str();
                if granted == requested {
                    println!(
                        "joined as {} ({granted})",
                        paired.cert.device().display_id()
                    );
                } else {
                    // T-0043's rule: the name was live for another machine, so
                    // this one got the deterministic suffix. Saying so is what
                    // keeps "why is my machine called workbox-2" from being a
                    // mystery — and the granted name is what is true, never the
                    // one that was asked for.
                    println!(
                        "joined as {} ({granted} — {requested:?} was taken, so the relay added \
                         the suffix)",
                        paired.cert.device().display_id()
                    );
                }
                println!("machine:  {granted}");
                println!("id:       {}", fingerprint.as_str());
                ExitCode::from(OK)
            }
            (None, None) => {
                eprintln!("machines add: the relay answered a join without a row");
                ExitCode::from(CONFLICT)
            }
        },
        Err(e) => {
            eprintln!("machines add: the relay did not answer: {e}");
            ExitCode::from(UNREACHABLE)
        }
    }
}

/// Dial the relay with this machine's identity, for a verb that writes.
///
/// The read path folds "no config" and "no identity" into its cache fallback; a
/// write cannot fall back to anything, so it stops with the reason.
async fn session(options: &Options) -> Result<arreo_core::relay::session::RelaySession, ExitCode> {
    let path = config_path(options)?;
    let settings = match arreo_core::relay::config::load_config(&path) {
        Ok(Some(settings)) => settings,
        Ok(None) => {
            eprintln!(
                "machines: no relay is configured ({}), so the directory cannot be written",
                path.display()
            );
            return Err(ExitCode::from(UNREACHABLE));
        }
        Err(e) => {
            eprintln!("machines: {e}");
            return Err(ExitCode::from(USAGE));
        }
    };
    let (key, cert) = match paired_identity() {
        Ok(pair) => pair,
        Err(message) => {
            eprintln!("machines: {message}");
            return Err(ExitCode::from(UNREACHABLE));
        }
    };
    arreo_core::relay::session::RelaySession::dial(settings.addr, &settings.account, &key, &cert)
        .await
        .map_err(|e| {
            eprintln!("machines: cannot reach the relay at {}: {e}", settings.addr);
            ExitCode::from(UNREACHABLE)
        })
}

/// Resolve a name to the row the directory holds, or the exit code for "no such
/// machine" (3).
async fn row_named(
    session: &arreo_core::relay::session::RelaySession,
    wanted: &str,
    all: bool,
) -> Result<Row, ExitCode> {
    if let Err(e) = Name::parse(wanted) {
        eprintln!("machines: {wanted:?} is not a machine name: {e}");
        return Err(ExitCode::from(UNKNOWN_MACHINE));
    }
    match session.machines(all).await {
        Ok(reply) if reply.refused.is_none() => match reply
            .machines
            .iter()
            .find(|row| row.name.as_str() == wanted)
        {
            Some(row) => Ok(Row::from_relay(row)),
            None => {
                eprintln!("machines: no machine named {wanted:?} in this account");
                Err(ExitCode::from(UNKNOWN_MACHINE))
            }
        },
        Ok(reply) => {
            eprintln!(
                "machines: the relay refused to read the directory: {}",
                reply.refused.unwrap_or_default()
            );
            Err(ExitCode::from(CONFLICT))
        }
        Err(e) => {
            eprintln!("machines: the relay did not answer: {e}");
            Err(ExitCode::from(UNREACHABLE))
        }
    }
}

/// Turn a directory refusal into the contract's exit code.
///
/// The relay's reply is a sentence, not a code, so the mapping is by what the
/// refusal says: a taken name or a name that is not a name is a conflict (5), an
/// unknown machine is 3 — the same codes the read verbs use for the same
/// situations, so a script that switches on them does not need a second table.
fn refusal_code(reason: &str) -> ExitCode {
    if reason.contains("NoSuchMachine") || reason.contains("no machine") {
        ExitCode::from(UNKNOWN_MACHINE)
    } else {
        ExitCode::from(CONFLICT)
    }
}

async fn rename(args: &[String]) -> ExitCode {
    let options = match parse("rename", args, true) {
        Ok(options) => options,
        Err(code) => return code,
    };
    if let Err(code) = refuse_unused(&options, "rename") {
        return code;
    }
    let (Some(old), Some(new)) = (options.name.clone(), options.second.clone()) else {
        eprintln!("machines rename: needs the current name and the new one");
        return usage();
    };
    if let Some(flag) = read_only_flag(&options) {
        // Said rather than ignored: a flag that is accepted and does nothing is
        // worse than one that is refused, because the caller believes it worked.
        eprintln!(
            "machines rename: {flag} belongs to the read verbs; a write must reach the relay"
        );
        return ExitCode::from(USAGE);
    }
    let session = match session(&options).await {
        Ok(session) => session,
        Err(code) => return code,
    };
    // The row first: the verb takes names, the wire takes ids, and this is also
    // what makes "no such machine" exit 3 before anything is written.
    let row = match row_named(&session, &old, true).await {
        Ok(row) => row,
        Err(code) => return code,
    };
    if let Err(e) = Name::parse(&new) {
        eprintln!("machines: {new:?} is not a machine name: {e}");
        return ExitCode::from(CONFLICT);
    }
    match session.rename_machine(&row.machine_id, &new).await {
        Ok(reply) => match (reply.refused, reply.granted) {
            (Some(reason), _) => {
                eprintln!("machines: {reason}");
                refusal_code(&reason)
            }
            (None, Some(updated)) => {
                // The relay's row is what is printed: the caller sees what the
                // directory now holds, not what it asked for.
                println!("renamed {old} → {}", updated.name.as_str());
                ExitCode::from(OK)
            }
            (None, None) => {
                eprintln!("machines: the relay answered a rename without a row");
                ExitCode::from(CONFLICT)
            }
        },
        Err(e) => {
            eprintln!("machines: the relay did not answer: {e}");
            ExitCode::from(UNREACHABLE)
        }
    }
}

async fn remove(args: &[String]) -> ExitCode {
    let options = match parse("remove", args, true) {
        Ok(options) => options,
        Err(code) => return code,
    };
    if let Err(code) = refuse_unused(&options, "remove") {
        return code;
    }
    if let Some(flag) = read_only_flag(&options) {
        eprintln!(
            "machines remove: {flag} belongs to the read verbs; a write must reach the relay"
        );
        return ExitCode::from(USAGE);
    }
    let session = match session(&options).await {
        Ok(session) => session,
        Err(code) => return code,
    };

    // `--stale` is T-0043's bulk rule: the relay decides the set, and the reply
    // carries the rows it pruned, so "what it reclaimed" needs no second read.
    if options.stale {
        return match session.prune_stale().await {
            Ok(reply) => match reply.refused {
                Some(reason) => {
                    eprintln!("machines: {reason}");
                    refusal_code(&reason)
                }
                None => {
                    if reply.machines.is_empty() {
                        println!("nothing was stale; the directory is unchanged");
                    } else {
                        for row in &reply.machines {
                            println!("removed {} (name held for the tombstone window)", row.name);
                        }
                    }
                    ExitCode::from(OK)
                }
            },
            Err(e) => {
                eprintln!("machines: the relay did not answer: {e}");
                ExitCode::from(UNREACHABLE)
            }
        };
    }

    let Some(name) = options.name.clone() else {
        eprintln!("machines remove: needs a machine name, or --stale for the bulk prune");
        return usage();
    };
    let row = match row_named(&session, &name, true).await {
        Ok(row) => row,
        Err(code) => return code,
    };
    // An online machine is answering right now: tombstoning it is almost always
    // a mistake, so it takes the explicit word. The check is here rather than in
    // the directory because the directory's rule is about names, and this is
    // about the operator's intent.
    if row.presence == Presence::Online && !options.force {
        eprintln!(
            "machines: {name} is online right now (seen {} ago); pass --force to tombstone it anyway",
            human_age(row.age_secs(now_ms()))
        );
        return ExitCode::from(CONFLICT);
    }
    match session.remove_machine(&row.machine_id).await {
        Ok(reply) => match (reply.refused, reply.granted) {
            (Some(reason), _) => {
                eprintln!("machines: {reason}");
                refusal_code(&reason)
            }
            (None, Some(removed)) => {
                match removed.tombstone_until_ms {
                    Some(until) => println!(
                        "removed {} — the name is held until {} ({}), then it is free",
                        removed.name,
                        rfc3339(until),
                        human_age((until - now_ms()).max(0) / 1000)
                    ),
                    None => println!(
                        "removed {} — the relay reported no tombstone, which means it is gone",
                        removed.name
                    ),
                }
                ExitCode::from(OK)
            }
            (None, None) => {
                eprintln!("machines: the relay answered a removal without a row");
                ExitCode::from(CONFLICT)
            }
        },
        Err(e) => {
            eprintln!("machines: the relay did not answer: {e}");
            ExitCode::from(UNREACHABLE)
        }
    }
}

/// The `--json` envelope: the script contract, schema 1.
///
/// `source` is the honest field: `cache` means these rows are remembered, and
/// every timestamp is when the machine was last *seen*, not when it was read.
fn envelope(rows: &[Row], now_ms: i64, from_cache: bool) -> serde_json::Value {
    let machines: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "name": row.name,
                "machine_id": row.machine_id,
                "presence": row.presence_str(),
                "last_seen": rfc3339(row.last_seen_ms),
                "age_secs": row.age_secs(now_ms),
                "proto_version": row.proto_version,
                "reachable": row.reachable,
                "flags": row.flags(now_ms),
            })
        })
        .collect();
    serde_json::json!({
        "schema": SCHEMA,
        "source": if from_cache { "cache" } else { "relay" },
        "as_of": rfc3339(now_ms),
        "machines": machines,
    })
}

fn print_table(rows: &[Row], now_ms: i64, from_cache: bool) {
    println!(
        "NAME                     PRESENCE     LAST SEEN   PROTO{}",
        if from_cache {
            "  (from cache — the relay was not reached)"
        } else {
            ""
        }
    );
    if rows.is_empty() {
        println!("(no machines)");
        return;
    }
    for row in rows {
        println!(
            "{:<24} {:<12} {:>10} {:>5}  {}",
            row.name,
            row.presence_str(),
            human_age(row.age_secs(now_ms)),
            row.proto_version,
            row.flags(now_ms).join(",")
        );
    }
}

fn print_status(row: &Row, now_ms: i64, from_cache: bool) {
    println!("{}", row.name);
    println!("  machine id     {}", row.machine_id);
    println!(
        "  presence       {}{}",
        row.presence_str(),
        if from_cache { " (from cache)" } else { "" }
    );
    println!(
        "  last seen      {} ({} ago)",
        rfc3339(row.last_seen_ms),
        human_age(row.age_secs(now_ms))
    );
    println!("  protocol       {}", row.proto_version);
    // The trusted-device count comes from T-0046's API; until it lands this says
    // `unknown` rather than a fabricated 0 — a 0 would read as "no device is
    // trusted", which is a claim, and the criteria say `null` until it is real.
    println!("  trusted devices unknown (per-machine trust lands with T-0046)");
    if !row.flags(now_ms).is_empty() {
        println!("  flags          {}", row.flags(now_ms).join(","));
    }
}

fn human_age(secs: i64) -> String {
    match secs {
        0..=1 => "now".to_string(),
        2..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

/// The unit tests live beside the code they describe; the end-to-end ones (a
/// real relay binary, a real config, the real CLI) live in
/// `crates/arreo-cli/tests/machines.rs`.
#[cfg(test)]
mod tests {
    use super::*;

    fn grant(hex: char, role: Role, granted_by: char, revoked: Option<i64>) -> GrantedDevice {
        GrantedDevice {
            device: DeviceId::parse(&hex.to_string().repeat(32)).expect("device"),
            role,
            granted_at_ms: 1_000,
            granted_by: DeviceId::parse(&granted_by.to_string().repeat(32)).expect("device"),
            revoked_at_ms: revoked,
        }
    }

    /// The `trust --list --json` contract: its own document with its own version,
    /// and the same additive-only discipline as the relay listing — a removed or
    /// renamed key is a red build, because a script parses it.
    #[test]
    fn the_grants_envelope_is_schema_1_with_its_own_keys() {
        let machine = MachineId::parse("22222222222222222222222222222222").expect("id");
        let rows = vec![
            grant('a', Role::Viewer, 'a', None),
            grant('b', Role::Owner, 'c', Some(2_000)),
        ];
        let envelope = grants_envelope(&rows, "the-pi", &machine, 10_000);
        assert_eq!(envelope["schema"], serde_json::json!(1));
        assert_eq!(envelope["machine"], serde_json::json!("the-pi"));
        assert_eq!(envelope["machine_id"], serde_json::json!(machine.as_str()));
        assert!(envelope["as_of"].as_str().is_some_and(|t| t.ends_with('Z')));

        let grants = envelope["grants"].as_array().expect("an array");
        assert_eq!(grants.len(), 2);
        for entry in grants {
            for key in [
                "device",
                "role",
                "granted_at",
                "granted_by",
                "revoked_at",
                "live",
            ] {
                assert!(
                    entry.get(key).is_some(),
                    "the contract requires {key} (additive-only): {entry}"
                );
            }
            let role = entry["role"].as_str().expect("role is text");
            assert!(
                ["viewer", "operator"].contains(&role),
                "the role is the word --role accepts, not the certificate's: {role:?}"
            );
        }
        // A live grant has no revocation time, and says so with `null` rather than
        // by omitting the key.
        assert_eq!(grants[0]["revoked_at"], serde_json::json!(null));
        assert_eq!(grants[0]["live"], serde_json::json!(true));
        assert_eq!(grants[1]["live"], serde_json::json!(false));
        assert!(
            grants[1]["revoked_at"].as_str().is_some(),
            "a revoked grant carries when: {grants:?}"
        );
        assert_eq!(
            grants[1]["role"],
            serde_json::json!("operator"),
            "the roadmap's word for the owner role (ADR 0019)"
        );
    }

    /// `--machine` accepts what an operator has in hand: the name and the id.
    #[test]
    fn a_machine_name_matches_by_name_or_id() {
        let machine = MachineId::parse("33333333333333333333333333333333").expect("id");
        assert!(machine_matches("the-pi", &machine, "the-pi"));
        assert!(machine_matches("THE-PI", &machine, "the-pi"));
        assert!(machine_matches(machine.as_str(), &machine, "the-pi"));
        assert!(!machine_matches("the-vps", &machine, "the-pi"));
        // A name that merely contains this one is not this machine.
        assert!(!machine_matches("the-pi-2", &machine, "the-pi"));
    }

    /// Argument mistakes are reported in the terms they were typed, and are
    /// rejected before anything is opened or created — so a typo cannot leave a
    /// key file or a store behind, and this test needs no filesystem.
    #[test]
    fn trust_rejects_bad_arguments_before_touching_anything() {
        // Nothing to do: no device, no --list.
        assert_eq!(run(&["trust".to_string()]), ExitCode::from(USAGE));
        // Both at once: ambiguous, refused.
        assert_eq!(
            run(&[
                "trust".to_string(),
                "11111111111111111111111111111111".to_string(),
                "--list".to_string()
            ]),
            ExitCode::from(USAGE)
        );
        // A device id that is not one.
        assert_eq!(
            run(&["trust".to_string(), "not-a-device".to_string()]),
            ExitCode::from(UNKNOWN_MACHINE)
        );
        // A role the policy does not have. `admin` is Team-tier (ROADMAP §4).
        assert_eq!(
            run(&[
                "trust".to_string(),
                "11111111111111111111111111111111".to_string(),
                "--role".to_string(),
                "admin".to_string()
            ]),
            ExitCode::from(USAGE)
        );
        // An unknown flag.
        assert_eq!(
            run(&["trust".to_string(), "--nope".to_string()]),
            ExitCode::from(USAGE)
        );
    }

    fn row(name: &str, presence: Presence, last_seen_ms: i64, conflict: bool) -> Row {
        Row {
            name: name.to_string(),
            machine_id: "11111111111111111111111111111111".to_string(),
            presence,
            last_seen_ms,
            proto_version: 1,
            name_conflict: conflict,
            tombstone_until_ms: None,
            reachable: true,
            from_cache: false,
        }
    }

    /// The envelope's keys and values are the contract: this test is what makes
    /// a removed or renamed key a red build.
    #[test]
    fn the_json_envelope_is_schema_1_with_closed_presence_values() {
        let rows = vec![
            row("alpha", Presence::Online, 1_000, false),
            row("beta", Presence::Stale, 0, true),
        ];
        let envelope = envelope(&rows, 10_000, false);
        assert_eq!(envelope["schema"], serde_json::json!(1));
        assert_eq!(envelope["source"], serde_json::json!("relay"));
        assert!(
            envelope["as_of"].as_str().is_some_and(|t| t.ends_with('Z')),
            "as_of is RFC3339 UTC: {}",
            envelope["as_of"]
        );

        let machines = envelope["machines"].as_array().expect("an array");
        assert_eq!(machines.len(), 2);
        assert_eq!(
            machines[0]["flags"],
            serde_json::json!([] as [&str; 0]),
            "a live row carries no unverified flag"
        );
        for machine in machines {
            for key in [
                "name",
                "machine_id",
                "presence",
                "last_seen",
                "age_secs",
                "proto_version",
                "reachable",
                "flags",
            ] {
                assert!(
                    machine.get(key).is_some(),
                    "schema 1 requires {key} (additive-only): {machine}"
                );
            }
            let presence = machine["presence"].as_str().expect("presence is text");
            assert!(
                ["online", "offline", "stale", "unknown"].contains(&presence),
                "presence is a closed enum, got {presence:?}"
            );
        }
        // The flags an operator needs: a suffixed name is not a mistake, and a
        // stale machine's name is up for reclaiming.
        assert_eq!(
            machines[1]["flags"],
            serde_json::json!(["name-suffixed", "name-reclaimable"]),
            "the conflict flag is what tells an operator a suffixed name is not a mistake"
        );
    }

    /// A cache-sourced envelope says so, and its rows keep the presence the relay
    /// last stated — a cached row is never silently promoted to `online`.
    #[test]
    fn a_cached_envelope_is_labelled() {
        let cached = CachedMachine {
            machine_id: arreo_core::mesh::MachineId::parse("22222222222222222222222222222222")
                .expect("id"),
            presence: Presence::Offline,
            last_seen_ms: 1_000,
            proto_version: 1,
            name_conflict: false,
            tombstone_until_ms: None,
        };
        let rows = vec![Row::from_cache("beta", &cached)];
        let envelope = envelope(&rows, 10_000, true);
        assert_eq!(envelope["source"], serde_json::json!("cache"));
        assert_eq!(
            envelope["machines"][0]["presence"],
            serde_json::json!("offline")
        );
        assert_eq!(envelope["machines"][0]["age_secs"], serde_json::json!(9));
        assert_eq!(
            envelope["machines"][0]["reachable"],
            serde_json::json!(false),
            "a remembered row is not a claim about reachability"
        );
        assert_eq!(
            envelope["machines"][0]["flags"],
            serde_json::json!(["unverified"]),
            "a cached row is marked, and never silently trusted"
        );
    }

    /// A cached row is never `online`, whatever the relay last said: liveness is
    /// the claim the CLI cannot substantiate without the relay.
    #[test]
    fn a_cached_row_is_never_online() {
        let cached = CachedMachine {
            machine_id: arreo_core::mesh::MachineId::parse("22222222222222222222222222222222")
                .expect("id"),
            presence: Presence::Online,
            last_seen_ms: 9_999,
            proto_version: 1,
            name_conflict: false,
            tombstone_until_ms: None,
        };
        let row = Row::from_cache("alpha", &cached);
        assert_eq!(
            row.presence_str(),
            "offline",
            "a remembered row is not live"
        );
        assert!(row.flags(10_000).contains(&"unverified"));
        assert_eq!(
            row.age_secs(10_000),
            0,
            "and the age is still the honest part"
        );
        // `stale` is a statement about the past, so it survives the trip.
        let stale = CachedMachine {
            presence: Presence::Stale,
            ..cached
        };
        assert_eq!(Row::from_cache("alpha", &stale).presence_str(), "stale");
    }

    /// A negative age (a relay whose clock is ahead of ours) prints zero, not a
    /// number that reads as a bug in the product.
    #[test]
    fn an_age_is_never_negative() {
        let row = row("alpha", Presence::Online, 20_000, false);
        assert_eq!(row.age_secs(10_000), 0, "clock skew is not a negative age");
        assert_eq!(row.age_secs(20_000), 0);
        assert_eq!(row.age_secs(25_000), 5);
    }

    /// A flag a verb does not read is refused, never accepted and ignored: the
    /// caller would otherwise believe something happened.
    #[test]
    fn a_flag_belongs_to_one_verb_or_none() {
        // `--json` is the read verbs' contract; the write verbs print one line.
        for verb in ["rename", "remove"] {
            assert_eq!(
                run(&[verb.to_string(), "--json".to_string()]),
                ExitCode::from(USAGE),
                "`machines {verb} --json` claims a contract this verb does not have"
            );
        }
        // `--uri` is `add`'s alone.
        for verb in ["list", "status", "rename", "remove"] {
            assert_eq!(
                run(&[
                    verb.to_string(),
                    "--uri".to_string(),
                    "arreo://pair?v=1".to_string()
                ]),
                ExitCode::from(USAGE),
                "`machines {verb} --uri` is another verb's flag"
            );
        }
        // `--name` is `add`'s; `rename` takes its names positionally.
        assert_eq!(
            run(&[
                "rename".to_string(),
                "old".to_string(),
                "new".to_string(),
                "--name".to_string(),
                "nope".to_string()
            ]),
            ExitCode::from(USAGE)
        );
    }

    /// `add` needs a code *and* an invite, and says which is missing rather than
    /// dialing anything: a unit test has no relay, so a missing check would hang
    /// rather than fail.
    #[test]
    fn add_asks_for_the_code_and_the_invite() {
        assert_eq!(run(&["add".to_string()]), ExitCode::from(USAGE));
        assert_eq!(
            run(&["add".to_string(), "four words".to_string()]),
            ExitCode::from(USAGE),
            "a code without an invite cannot find the admitting machine"
        );
        assert_eq!(
            run(&[
                "add".to_string(),
                "four words".to_string(),
                "--uri".to_string(),
                "not an invite".to_string()
            ]),
            ExitCode::from(USAGE),
            "a malformed invite is refused before any exchange"
        );
        assert_eq!(
            run(&[
                "add".to_string(),
                "four words".to_string(),
                "extra".to_string(),
                "--uri".to_string(),
                "arreo://pair?v=1".to_string()
            ]),
            ExitCode::from(USAGE),
            "one code, one invite, nothing else"
        );
    }

    /// Usage errors are exit 2, whether the verb is unknown or an argument is.
    #[test]
    fn usage_errors_are_exit_2() {
        assert_eq!(run(&[]), ExitCode::from(USAGE));
        assert_eq!(run(&["wat".to_string()]), ExitCode::from(USAGE));
        assert_eq!(
            run(&["list".to_string(), "--nope".to_string()]),
            ExitCode::from(USAGE)
        );
    }

    /// Every exit code in the contract is distinct — a script that switches on
    /// them must be able to tell the cases apart.
    #[test]
    fn the_exit_codes_are_distinct() {
        let codes = [OK, USAGE, UNKNOWN_MACHINE, UNREACHABLE, CONFLICT];
        let unique: std::collections::HashSet<u8> = codes.iter().copied().collect();
        assert_eq!(unique.len(), codes.len(), "two codes collide: {codes:?}");
        assert!(!codes.contains(&1), "1 is reserved for a general failure");
    }

    /// Ages read the way an operator says them.
    #[test]
    fn ages_are_human() {
        assert_eq!(human_age(0), "now");
        assert_eq!(human_age(1), "now");
        assert_eq!(human_age(45), "45s");
        assert_eq!(human_age(120), "2m");
        assert_eq!(human_age(7_200), "2h");
        assert_eq!(human_age(172_800), "2d");
    }
}
