//! `arreo sync` — the local half of ROADMAP §3.8 from the command line.
//!
//! One sentence: this is where a machine says which files it would sync and
//! where they are, publishes a revision, takes one in, looks at what landed,
//! and puts a broken provider list back the way it was.
//!
//! ## Local steps, and the one that reaches a peer
//!
//! Every verb here is a real operation on this machine's store: `push` records a
//! revision, `payload` prints what a peer receives, `apply` takes one in. The
//! two-root slice in `xtask sync --check` drives exactly these verbs on two
//! isolated roots, and nothing here is scaffolding.
//!
//! `push --machine <name>` is the one that leaves the machine (T-0086): it
//! counts the revision here, then hands the *same* payload to the peer's daemon
//! over the mesh (`Message::Sync`) and reports what that machine did with it —
//! applied, already up to date, a conflict with both copies kept, or refused
//! with the receiver's own reason. There is deliberately no `pull`: a payload
//! travels one way and the receiver's refusals are the answer, so a second verb
//! would be a second code path for the same exchange.
//!
//! ## Which identity counts a revision
//!
//! This machine's **device id** (`dev_…`, from `identity/device.key`), never its
//! display name: the name is a label the operator can change, and a counter
//! keyed by it would fork into "before the rename" and "after" — every peer would
//! then see a concurrent edit from a machine that did nothing, and keep a
//! conflict copy of it. A machine with no identity refuses these verbs by name
//! rather than falling back to the hostname.
//!
//! ## What is deliberately not printed
//!
//! A secret value, ever. `arreo sync env` reports which variables this machine
//! holds and which it does not **by name**; the value is injected at spawn from
//! the machine's own store and never crosses stdout, a log or a payload.
//! (`docs/harness-centralization.md` §3.3: `opencode debug config` resolves
//! `{env:VAR}` to the literal value, which is why nothing here does the same.)

use arreo_core::proto::{Message, SyncExchange, SyncOutcome, SyncStatus, VERSION};
use arreo_core::store::{SessionStore, SyncRevision};
use arreo_core::sync::engine::{ReceiveOutcome, SyncEngine, SyncError, SyncPayload};
use arreo_core::sync::keychain::SecretStore;
use arreo_core::sync::paths::MachineEnv;
use std::io::Read as _;
use std::path::PathBuf;
use std::process::ExitCode;

/// Flags every sub-verb accepts, plus the ones only some use.
struct Options {
    /// `--name NAME`: the display name this machine reports. **Not** the counter
    /// key — that is this machine's device id (T-0086), which no flag can
    /// override: a counter an operator could rename is a counter that forks.
    name: Option<String>,
    /// `--machine NAME`: *another* machine, reached over the mesh. The spelling
    /// every other verb in this CLI uses for the fleet, so `sync push --machine`
    /// means what `read --machine` means.
    machine: Option<String>,
    config: Option<PathBuf>,
    store: Option<PathBuf>,
    socket: PathBuf,
    out: Option<String>,
    to: Option<i64>,
    json: bool,
    positional: Vec<String>,
}

impl Options {
    fn parse(rest: &[String]) -> Result<Self, ExitCode> {
        let mut options = Options {
            name: None,
            machine: None,
            config: None,
            store: None,
            socket: super::default_socket(),
            out: None,
            to: None,
            json: false,
            positional: Vec::new(),
        };
        let mut i = 0;
        while i < rest.len() {
            let value = |i: usize| -> Result<String, ExitCode> {
                rest.get(i + 1).cloned().ok_or_else(|| {
                    eprintln!("sync: {} wants a value", rest[i]);
                    ExitCode::from(2)
                })
            };
            match rest[i].as_str() {
                "--name" => {
                    options.name = Some(value(i)?);
                    i += 2;
                }
                "--machine" => {
                    options.machine = Some(value(i)?);
                    i += 2;
                }
                "--config" => {
                    options.config = Some(PathBuf::from(value(i)?));
                    i += 2;
                }
                "--store" => {
                    options.store = Some(PathBuf::from(value(i)?));
                    i += 2;
                }
                "--socket" => {
                    options.socket = PathBuf::from(value(i)?);
                    i += 2;
                }
                "--out" => {
                    options.out = Some(value(i)?);
                    i += 2;
                }
                "--to" => {
                    let raw = value(i)?;
                    options.to = Some(raw.parse::<i64>().map_err(|_| {
                        eprintln!("sync: --to wants a revision id, got {raw:?}");
                        ExitCode::from(2)
                    })?);
                    i += 2;
                }
                "--json" => {
                    options.json = true;
                    i += 1;
                }
                other => {
                    options.positional.push(other.to_string());
                    i += 1;
                }
            }
        }
        Ok(options)
    }

    /// The store behind the daemon socket, or `--store`.
    ///
    /// Same file the daemon uses (`<socket>.db`), because the sync vectors and
    /// history are this machine's state like the pane topology and the audit log
    /// are: one store per machine, not a second one for config sync.
    fn store_path(&self) -> PathBuf {
        match &self.store {
            Some(path) => path.clone(),
            None => {
                let mut path = self.socket.clone().into_os_string();
                path.push(".db");
                PathBuf::from(path)
            }
        }
    }

    /// The display name this machine reports in the sentences an operator reads.
    ///
    /// **Not** the counter key: that is the device id (see the module docs). This
    /// is the half `MachineEnv` supplies, and `--name` overrides it for a caller
    /// standing in for another machine (the two-root slice).
    fn display_name(&self) -> String {
        self.name
            .clone()
            .unwrap_or_else(arreo_core::mesh::default_machine_name)
    }
}

pub fn run(rest: &[String]) -> ExitCode {
    if rest.is_empty() || rest[0] == "--help" || rest[0] == "-h" {
        usage();
        return ExitCode::from(2);
    }
    let verb = rest[0].as_str();
    let options = match Options::parse(&rest[1..]) {
        Ok(options) => options,
        Err(code) => return code,
    };
    // `secret set` reads the value from stdin, so it must run before anything
    // else touches stdin and before any store is opened.
    if verb == "secret" {
        return cmd_secret(&options);
    }
    // **The counter key is this machine's device identity** (T-0086), so a
    // machine without one refuses here, by name, rather than counting revisions
    // under a string the operator typed. Every verb below writes or reads
    // vectors keyed by it.
    let identity = match arreo_core::identity::own_device_id() {
        Ok(id) => id.display_id(),
        Err(e) => {
            eprintln!("sync: this machine has no device identity, so it has no id to count revisions under");
            eprintln!("sync: {e}");
            eprintln!(
                "sync: pair it (`arreo pair`) — a version vector is keyed by the device id the mesh \
                 authenticates, never by a name, which a rename would fork"
            );
            return ExitCode::FAILURE;
        }
    };
    let env = machine_env(&options);
    let secrets = match SecretStore::default_path(&env)
        .map_err(|e| e.to_string())
        .and_then(|path| SecretStore::open(path).map_err(|e| e.to_string()))
    {
        Ok(secrets) => secrets,
        Err(e) => {
            eprintln!("sync: {e}");
            return ExitCode::FAILURE;
        }
    };
    let store = match SessionStore::open(&options.store_path()) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("sync: {}", options.store_path().display());
            eprintln!("sync: {e}");
            return ExitCode::FAILURE;
        }
    };
    let engine = SyncEngine::new(&store, &env, &secrets, &identity);
    match verb {
        "list" | "status" => cmd_list(&engine, &options),
        "push" => cmd_push(&engine, &options),
        "payload" => cmd_payload(&engine, &options),
        "apply" => cmd_apply(&engine, &options),
        "history" => cmd_history(&engine, &options),
        "revert" => cmd_revert(&engine, &options),
        "conflicts" => cmd_conflicts(&engine, &options),
        "merge" => cmd_merge(&engine, &options),
        "env" => cmd_env(&engine, &options),
        other => {
            eprintln!("sync: unknown verb {other:?}");
            usage();
            ExitCode::from(2)
        }
    }
}

fn usage() {
    eprintln!("usage:");
    eprintln!(
        "  arreo sync list [--json]                      what this machine would sync, and where"
    );
    eprintln!("  arreo sync push <file> [--json]               validate it and count a revision");
    eprintln!(
        "  arreo sync push <file> --machine NAME         count it here, then send it there (T-0086)"
    );
    eprintln!(
        "  arreo sync payload <file> [--out PATH|-]      what a peer receives (neutral references)"
    );
    eprintln!("  arreo sync apply <payload.json> [--json]      take a peer's revision in");
    eprintln!("  arreo sync history <file> [--json]            every revision this machine kept");
    eprintln!("  arreo sync revert <file> [--to ID] [--json]   put a previous revision back");
    eprintln!("  arreo sync conflicts <file> [--json]          the copies both machines kept");
    eprintln!("  arreo sync merge <file> [--json]              reconcile them (arrays union, comments refuse)");
    eprintln!(
        "  arreo sync env <file> [--json]                which variables the file needs, by name"
    );
    eprintln!(
        "  arreo sync secret set <NAME>                  value on stdin; never echoed, stored 0600"
    );
    eprintln!("  arreo sync secret list [--json]               names only, never values");
    eprintln!("  --name NAME     the display name this machine reports (default: this hostname)");
    eprintln!(
        "  --machine NAME   push to that machine over the mesh (with --config PATH if needed)"
    );
    eprintln!("  --store PATH     the machine's store (default: <socket>.db, the daemon's own)");
    eprintln!("  <file> is a preset name (opencode.jsonc, models.json, …) or an absolute path of your own");
    eprintln!(
        "  revisions count under this machine's device identity (identity/device.key), never a name"
    );
}

fn machine_env(options: &Options) -> MachineEnv {
    MachineEnv::from_process(&options.display_name())
}

/// Print a refusal the way every other verb does: the sentence, then the code.
fn refused(error: &SyncError) -> ExitCode {
    eprintln!("sync: {error}");
    ExitCode::FAILURE
}

fn cmd_list(engine: &SyncEngine<'_>, options: &Options) -> ExitCode {
    let status = match engine.status() {
        Ok(status) => status,
        Err(e) => return refused(&e),
    };
    if options.json {
        let rows: Vec<serde_json::Value> = status
            .iter()
            .map(|s| {
                serde_json::json!({
                    "file": s.file,
                    "harness": s.harness,
                    "class": s.class.as_str(),
                    "path": s.path.as_ref().map(|p| p.display().to_string()),
                    "unresolved": s.unresolved,
                    "present": s.present,
                    "portable": s.portable,
                    "vector": s.vector,
                    "missing": s.missing,
                    "conflicts": s.conflicts,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({"machine": engine.machine(), "files": rows})
        );
        return ExitCode::SUCCESS;
    }
    println!(
        "{} — {} syncable file(s), presets for opencode/pi/omp only",
        engine.machine(),
        status.len()
    );
    for s in &status {
        let where_ = match (&s.path, &s.unresolved) {
            (Some(path), _) => path.display().to_string(),
            (None, Some(reason)) => format!("UNRESOLVED: {reason}"),
            (None, None) => "-".to_string(),
        };
        println!(
            "  {:<16} {:<9} {:<7} {}  vector={}",
            s.file,
            s.harness,
            if s.present { "present" } else { "absent" },
            where_,
            s.vector
        );
        if !s.missing.is_empty() {
            println!(
                "      needs {} (not set on this machine)",
                s.missing.join(", ")
            );
        }
        if s.conflicts > 0 {
            println!("      {} conflict copy(ies) kept beside it", s.conflicts);
        }
    }
    ExitCode::SUCCESS
}

fn require_file(options: &Options) -> Result<&str, ExitCode> {
    options
        .positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| {
            eprintln!("sync: this verb needs a file (a preset name or an absolute path)");
            ExitCode::from(2)
        })
}

fn cmd_push(engine: &SyncEngine<'_>, options: &Options) -> ExitCode {
    let file = match require_file(options) {
        Ok(file) => file,
        Err(code) => return code,
    };
    if options.machine.is_some() {
        return remote_push(engine, options, file);
    }
    let outcome = match engine.push(file) {
        Ok(outcome) => outcome,
        Err(e) => return refused(&e),
    };
    if options.json {
        println!(
            "{}",
            serde_json::json!({
                "file": outcome.file,
                "path": outcome.path.display().to_string(),
                "counter": outcome.counter,
                "revision": outcome.revision,
                "changed": outcome.changed,
                "references": outcome.references,
                "missing": outcome.missing,
            })
        );
        return ExitCode::SUCCESS;
    }
    if outcome.changed {
        println!(
            "{}: {} revision {} -> {}",
            outcome.file,
            engine.machine(),
            outcome.counter,
            outcome.path.display()
        );
    } else {
        println!(
            "{}: unchanged ({} already holds these bytes)",
            outcome.file,
            engine.machine()
        );
    }
    for name in &outcome.missing {
        eprintln!(
            "sync: warning: {name} is referenced but not set on {} — set it before a peer receives \
             this file: `arreo sync secret set {name}`",
            engine.machine()
        );
    }
    ExitCode::SUCCESS
}

/// `push --machine <name>` (T-0086): count the revision here — the sender is
/// the authority on its own counter, and this machine is the sender — then hand
/// the *same* payload the local form produces to the peer's daemon and report
/// what it did with it.
///
/// The exchange is `Message::Sync`; the daemon runs the same `receive` a local
/// `apply` would, and the reply is the outcome (applied, up to date, conflict,
/// refused). A refusal is a *typed answer*, not a transport failure, and it
/// prints like any other refusal here.
fn remote_push(engine: &SyncEngine<'_>, options: &Options, file: &str) -> ExitCode {
    let pushed = match engine.push(file) {
        Ok(outcome) => outcome,
        Err(e) => return refused(&e),
    };
    if pushed.changed {
        println!(
            "{}: {} revision {} recorded here",
            pushed.file,
            engine.machine(),
            pushed.counter
        );
    }
    for name in &pushed.missing {
        eprintln!(
            "sync: warning: {name} is referenced but not set on {} — the receiver will refuse \
             until it sets its own: `arreo sync secret set {name}`",
            engine.machine()
        );
    }
    let payload = match engine.payload(file) {
        Ok(payload) => payload,
        Err(e) => return refused(&e),
    };
    let exchange = match serde_json::to_vec(&payload) {
        Ok(bytes) => SyncExchange { payload: bytes },
        Err(e) => {
            eprintln!("sync: cannot encode the payload: {e}");
            return ExitCode::FAILURE;
        }
    };
    let machine = options.machine.clone().unwrap_or_default();
    // The whole round trip is one future returning the process's code, which is
    // the shape `rt::block_on` takes (and the shape every other async verb in
    // this CLI uses): the refusal prints here, where the operator is, rather
    // than being handed back up as a value nobody would look at.
    crate::rt::block_on(async move {
        match send_sync(options, &machine, exchange).await {
            Ok(outcome) => print_outcome(&machine, &outcome, options),
            Err((code, message)) => {
                eprintln!("sync: {message}");
                ExitCode::from(code)
            }
        }
    })
}

/// One `Message::Sync` round trip to a peer, by name, through the same client
/// the rest of the CLI uses.
async fn send_sync(
    options: &Options,
    machine: &str,
    exchange: SyncExchange,
) -> Result<SyncOutcome, (u8, String)> {
    let resolved = arreo_core::mesh::resolve::by_name(machine, options.config.as_deref())
        .await
        .map_err(|e| (crate::remote::exit_code(&e), e.message().to_string()))?;
    let mut client = arreo_core::mesh::session::Client::connect_to(&resolved.target)
        .await
        .map_err(|e| (4u8, format!("{}: {e}", resolved.name)))?;
    match client
        .call(&Message::Sync {
            v: VERSION,
            exchange,
        })
        .await
    {
        Ok(Message::SyncReply { outcome, .. }) => Ok(*outcome),
        Ok(Message::Error { message, .. }) => Err((5u8, message)),
        Ok(other) => Err((4u8, format!("{}: unexpected {other:?}", resolved.name))),
        Err(e) => Err((4u8, format!("{}: {e}", resolved.name))),
    }
}

/// Print what the receiver did, in the same sentences `apply` uses locally —
/// the difference is only whose machine is being talked about.
fn print_outcome(machine: &str, outcome: &SyncOutcome, options: &Options) -> ExitCode {
    if options.json {
        println!(
            "{}",
            serde_json::json!({
                "machine": machine,
                "file": outcome.file,
                "outcome": status_word(outcome.status),
                "from": outcome.from,
                "counter": outcome.counter,
                "revision": outcome.revision,
                "copy": outcome.copy,
                "reason": outcome.reason,
            })
        );
        return ExitCode::SUCCESS;
    }
    match outcome.status {
        SyncStatus::Applied => {
            println!(
                "{file}: {machine} applied {from}'s revision {counter}",
                file = outcome.file,
                machine = machine,
                from = outcome.from,
                counter = outcome.counter
            );
            ExitCode::SUCCESS
        }
        SyncStatus::UpToDate => {
            println!(
                "{file}: {machine} already had these bytes (nothing written)",
                file = outcome.file,
                machine = machine
            );
            ExitCode::SUCCESS
        }
        SyncStatus::Conflict => {
            println!(
                "{file}: {machine} edited this too — both kept; its live file is unchanged and \
                 this machine's copy is {copy} (arbitrate with `arreo sync merge {file}` on {machine})",
                file = outcome.file,
                machine = machine,
                copy = outcome.copy
            );
            ExitCode::SUCCESS
        }
        SyncStatus::Refused => {
            eprintln!(
                "sync: {machine} refused {file}: {reason}",
                machine = machine,
                file = outcome.file,
                reason = outcome.reason
            );
            ExitCode::FAILURE
        }
    }
}

/// The wire word for a status, for `--json`.
fn status_word(status: SyncStatus) -> &'static str {
    match status {
        SyncStatus::Applied => "applied",
        SyncStatus::UpToDate => "up-to-date",
        SyncStatus::Conflict => "conflict",
        SyncStatus::Refused => "refused",
    }
}

fn cmd_payload(engine: &SyncEngine<'_>, options: &Options) -> ExitCode {
    let file = match require_file(options) {
        Ok(file) => file,
        Err(code) => return code,
    };
    let payload = match engine.payload(file) {
        Ok(payload) => payload,
        Err(e) => return refused(&e),
    };
    let text = match serde_json::to_string_pretty(&payload) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("sync: {e}");
            return ExitCode::FAILURE;
        }
    };
    match options.out.as_deref() {
        None | Some("-") => println!("{text}"),
        Some(path) => {
            if let Err(e) = std::fs::write(path, format!("{text}\n")) {
                eprintln!("sync: {path}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    ExitCode::SUCCESS
}

fn cmd_apply(engine: &SyncEngine<'_>, options: &Options) -> ExitCode {
    let source = match options.positional.first() {
        Some(source) => source.as_str(),
        None => {
            eprintln!("sync apply needs a payload file (or `-` for stdin)");
            return ExitCode::from(2);
        }
    };
    let text = if source == "-" {
        let mut text = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut text) {
            eprintln!("sync: stdin: {e}");
            return ExitCode::FAILURE;
        }
        text
    } else {
        match std::fs::read_to_string(source) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("sync: {source}: {e}");
                return ExitCode::FAILURE;
            }
        }
    };
    let payload: SyncPayload = match serde_json::from_str(&text) {
        Ok(payload) => payload,
        Err(e) => {
            eprintln!("sync: {source}: not a sync payload: {e}");
            return ExitCode::FAILURE;
        }
    };
    let outcome = match engine.receive(&payload) {
        Ok(outcome) => outcome,
        Err(e) => return refused(&e),
    };
    if options.json {
        let value = match &outcome {
            ReceiveOutcome::Applied {
                file,
                path,
                from,
                counter,
                revision,
            } => serde_json::json!({
                "outcome": "applied", "file": file, "path": path.display().to_string(),
                "from": from, "counter": counter, "revision": revision,
            }),
            ReceiveOutcome::UpToDate { file } => {
                serde_json::json!({"outcome": "up-to-date", "file": file})
            }
            ReceiveOutcome::Conflict {
                file,
                live,
                copy,
                from,
                revision,
            } => serde_json::json!({
                "outcome": "conflict", "file": file, "live": live.display().to_string(),
                "copy": copy.display().to_string(), "from": from, "revision": revision,
            }),
        };
        println!("{value}");
        return ExitCode::SUCCESS;
    }
    match outcome {
        ReceiveOutcome::Applied {
            file,
            path,
            from,
            counter,
            ..
        } => println!(
            "{file}: applied {from}'s revision {counter} -> {}",
            path.display()
        ),
        ReceiveOutcome::UpToDate { file } => {
            println!("{file}: already up to date (nothing written)")
        }
        ReceiveOutcome::Conflict {
            file,
            live,
            copy,
            from,
            ..
        } => println!(
            "{file}: {from} edited this too — both kept; the live file is unchanged at {} and \
             {from}'s copy is {} (arbitrate with `arreo sync merge {file}`)",
            live.display(),
            copy.display()
        ),
    }
    ExitCode::SUCCESS
}

fn revision_json(revision: &SyncRevision) -> serde_json::Value {
    serde_json::json!({
        "id": revision.id,
        "file": revision.file,
        "machine": revision.machine,
        "counter": revision.counter,
        "created_ms": revision.created_ms,
        "reason": revision.reason,
    })
}

fn cmd_history(engine: &SyncEngine<'_>, options: &Options) -> ExitCode {
    let file = match require_file(options) {
        Ok(file) => file,
        Err(code) => return code,
    };
    let revisions = match engine.history(file) {
        Ok(revisions) => revisions,
        Err(e) => return refused(&e),
    };
    if options.json {
        let rows: Vec<serde_json::Value> = revisions.iter().map(revision_json).collect();
        println!("{}", serde_json::json!({"file": file, "revisions": rows}));
        return ExitCode::SUCCESS;
    }
    if revisions.is_empty() {
        println!("{file}: no revisions on this machine yet");
        return ExitCode::SUCCESS;
    }
    for revision in &revisions {
        println!(
            "  {:<5} {:<8} {} rev {}  {}",
            revision.id,
            revision.reason,
            revision.machine,
            revision.counter,
            arreo_core::store::rfc3339_ms(revision.created_ms as i64)
        );
    }
    ExitCode::SUCCESS
}

fn cmd_revert(engine: &SyncEngine<'_>, options: &Options) -> ExitCode {
    let file = match require_file(options) {
        Ok(file) => file,
        Err(code) => return code,
    };
    let outcome = match engine.revert(file, options.to) {
        Ok(outcome) => outcome,
        Err(e) => return refused(&e),
    };
    if options.json {
        println!(
            "{}",
            serde_json::json!({
                "file": outcome.file,
                "path": outcome.path.display().to_string(),
                "revision": outcome.revision,
                "counter": outcome.counter,
                "explicit": outcome.explicit,
            })
        );
        return ExitCode::SUCCESS;
    }
    println!(
        "{}: restored the previous revision -> {} ({} revision {})",
        outcome.file,
        outcome.path.display(),
        engine.machine(),
        outcome.counter
    );
    ExitCode::SUCCESS
}

fn cmd_conflicts(engine: &SyncEngine<'_>, options: &Options) -> ExitCode {
    let file = match require_file(options) {
        Ok(file) => file,
        Err(code) => return code,
    };
    let copies = match engine.conflicts(file) {
        Ok(copies) => copies,
        Err(e) => return refused(&e),
    };
    if options.json {
        let rows: Vec<String> = copies.iter().map(|p| p.display().to_string()).collect();
        println!("{}", serde_json::json!({"file": file, "conflicts": rows}));
        return ExitCode::SUCCESS;
    }
    if copies.is_empty() {
        println!("{file}: no conflict copies");
        return ExitCode::SUCCESS;
    }
    for copy in &copies {
        println!("  {}", copy.display());
    }
    ExitCode::SUCCESS
}

fn cmd_merge(engine: &SyncEngine<'_>, options: &Options) -> ExitCode {
    let file = match require_file(options) {
        Ok(file) => file,
        Err(code) => return code,
    };
    let outcome = match engine.merge(file) {
        Ok(outcome) => outcome,
        Err(e) => return refused(&e),
    };
    if options.json {
        let sources: Vec<String> = outcome
            .sources
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "file": outcome.file,
                "path": outcome.path.display().to_string(),
                "sources": sources,
                "revision": outcome.revision,
                "counter": outcome.counter,
            })
        );
        return ExitCode::SUCCESS;
    }
    println!(
        "{}: merged {} source(s) -> {} ({} revision {})",
        outcome.file,
        outcome.sources.len(),
        outcome.path.display(),
        engine.machine(),
        outcome.counter
    );
    for source in &outcome.sources {
        println!("  from {}", source.display());
    }
    ExitCode::SUCCESS
}

fn cmd_env(engine: &SyncEngine<'_>, options: &Options) -> ExitCode {
    let file = match require_file(options) {
        Ok(file) => file,
        Err(code) => return code,
    };
    let (resolved, plan) = match engine.injection(file) {
        Ok(plan) => plan,
        Err(e) => return refused(&e),
    };
    if options.json {
        println!(
            "{}",
            serde_json::json!({
                "file": resolved.file,
                "path": resolved.path.display().to_string(),
                "machine": engine.machine(),
                "resolved": plan.resolved_names(),
                "missing": plan.missing(),
                "complete": plan.is_complete(),
            })
        );
        return ExitCode::SUCCESS;
    }
    println!(
        "{} needs {} variable(s) on {}:",
        resolved.file,
        plan.resolved_names().len() + plan.missing().len(),
        engine.machine()
    );
    for name in plan.resolved_names() {
        println!("  {name}: set");
    }
    for line in plan.report() {
        println!("  MISSING: {line}");
    }
    if plan.is_complete() {
        println!("  the value is injected when Arreo spawns the harness; nothing is exported here");
    }
    ExitCode::SUCCESS
}

fn cmd_secret(options: &Options) -> ExitCode {
    let sub = options.positional.first().map(String::as_str);
    let env = machine_env(options);
    let path = match SecretStore::default_path(&env) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("sync: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut secrets = match SecretStore::open(path) {
        Ok(secrets) => secrets,
        Err(e) => {
            eprintln!("sync: {e}");
            return ExitCode::FAILURE;
        }
    };
    match sub {
        Some("set") => {
            let Some(name) = options.positional.get(1) else {
                eprintln!("sync secret set needs a NAME (the value comes from stdin)");
                return ExitCode::from(2);
            };
            // Read the value from stdin and never echo it: a value on argv is a
            // value in the process table and in the shell's history file.
            let mut value = String::new();
            if let Err(e) = std::io::stdin().read_to_string(&mut value) {
                eprintln!("sync: stdin: {e}");
                return ExitCode::FAILURE;
            }
            let value = value.trim_end_matches(['\n', '\r']);
            if value.is_empty() {
                eprintln!("sync secret set: the value on stdin was empty");
                return ExitCode::from(2);
            }
            match secrets.set(name, value) {
                Ok(()) => {
                    if options.json {
                        println!(
                            "{}",
                            serde_json::json!({
                                "action": "set", "name": name, "machine": env.name(),
                                "store": secrets.path().display().to_string(),
                            })
                        );
                    } else {
                        println!(
                            "set {name} on {} ({})",
                            env.name(),
                            secrets.path().display()
                        );
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("sync: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("list") => {
            let names = secrets.names();
            if options.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "machine": env.name(),
                        "store": secrets.path().display().to_string(),
                        "names": names,
                    })
                );
            } else if names.is_empty() {
                println!(
                    "no secrets set on {} ({})",
                    env.name(),
                    secrets.path().display()
                );
            } else {
                for name in names {
                    println!("  {name}");
                }
            }
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("sync secret: unknown sub-verb {other:?} (set, list)");
            ExitCode::from(2)
        }
        None => {
            eprintln!("sync secret: set <NAME> | list");
            ExitCode::from(2)
        }
    }
}
