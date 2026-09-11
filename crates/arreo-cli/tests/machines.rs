//! T-0044 acceptance tests: the real CLI, a real relay, a real directory.
//!
//! Nothing is mocked. `arreo-relay` runs as a child process (as a *binary*, never
//! a linked crate: it is AGPL and this crate must not depend on it, T-0035), the
//! CLI runs as a child process with its own identity directory, and the rows it
//! prints were written into the relay's durable directory through the same wire
//! RPC every other client uses (T-0056).
//!
//! The relay binary is located beside this test's own binary, so these tests need
//! `cargo test --workspace` (which builds every binary) — the same requirement
//! `crates/arreo-server/tests/relay_daemon.rs` has, and for the same reason.

use arreo_core::identity::{DeviceCert, DeviceId, DeviceKey, Role, RootKey, VerifyingKey};
use arreo_core::relay::join_proof_payload;
use arreo_core::relay::session::RelaySession;
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

fn binary(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_arreo"))
        .parent()
        .expect("target dir")
        .join(name);
    assert!(
        path.exists(),
        "{} is missing — run `cargo test --workspace` (which builds every binary) first",
        path.display()
    );
    path
}

/// A running relay, its state directory, and everything it has logged.
struct Relay {
    child: Child,
    addr: SocketAddr,
    state_dir: PathBuf,
    mailbox: Option<PathBuf>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.state_dir);
    }
}

impl Relay {
    fn start(tag: &str) -> Self {
        Self::start_with(tag, None)
    }

    /// A relay that also serves the pairing mailbox, which is what one process
    /// running both jobs looks like on a self-hosted box (T-0024 + T-0029 in one
    /// binary). `add` needs both halves: the mailbox carries the pairing flights,
    /// the router carries the directory write that follows.
    fn start_with_mailbox(tag: &str) -> Self {
        let mailbox = std::env::temp_dir().join(format!(
            "arreo-cli-machines-{tag}-{}-{:?}-mailbox.sock",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&mailbox);
        Self::start_with(tag, Some(mailbox))
    }

    fn start_with(tag: &str, mailbox: Option<PathBuf>) -> Self {
        let state_dir = std::env::temp_dir().join(format!(
            "arreo-cli-machines-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&state_dir);
        std::fs::create_dir_all(&state_dir).expect("scratch state dir");
        let mut command = Command::new(binary("arreo-relay"));
        command.args([
            "serve",
            "--listen",
            "127.0.0.1:0",
            "--state-dir",
            &state_dir.display().to_string(),
        ]);
        if let Some(mailbox) = &mailbox {
            command.arg("--pairing-socket").arg(mailbox);
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the relay starts");

        let stderr = child.stderr.take().expect("stderr is piped");
        let (ready_tx, ready_rx) = mpsc::channel();
        // The reader thread must keep draining stderr for the relay's whole
        // life: dropping the pipe would kill it on its next log line (EPIPE),
        // which reads like an authentication failure. Nothing here reads the
        // log, so nothing here stores it.
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(rest) = line.split("router on ").nth(1) {
                    if let Some(addr) = rest.split_whitespace().next() {
                        if let Ok(addr) = addr.parse::<SocketAddr>() {
                            let _ = ready_tx.send(addr);
                        }
                    }
                }
            }
        });
        let addr = ready_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the relay announces its address");
        Self {
            child,
            addr,
            state_dir,
            mailbox,
        }
    }

    fn register_account(&self, account_id: &str, root: &VerifyingKey) {
        let output = Command::new(binary("arreo-relay"))
            .args([
                "account",
                "add",
                "--state-dir",
                &self.state_dir.display().to_string(),
                "--account",
                account_id,
                "--root-key",
                &hex(&root.to_bytes()),
            ])
            .output()
            .expect("the account command runs");
        assert!(
            output.status.success(),
            "registering an account failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// The pairing mailbox this relay serves, if any.
    fn mailbox(&self) -> &Path {
        self.mailbox
            .as_deref()
            .expect("this relay was started with a mailbox")
    }

    /// Stop the relay without forgetting its state: the offline case is "the
    /// relay is not answering", not "there is no relay".
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// One machine: a CLI identity directory plus the config naming the relay.
struct Client {
    dir: PathBuf,
    config: PathBuf,
    /// Whether this machine has a `[relay]` configuration at all. A joining
    /// machine does not: learning the account and relay is what `add` is for.
    configured: bool,
}

impl Client {
    /// A machine with no configuration and no identity: what a machine being
    /// admitted looks like before it runs `add`.
    fn new_bare(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "arreo-cli-machines-client-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("identity")).expect("identity dir");
        Self {
            config: dir.join("arreo.toml"),
            dir,
            configured: false,
        }
    }

    fn new(tag: &str, relay: &Relay, account: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "arreo-cli-machines-client-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let identity = dir.join("identity");
        std::fs::create_dir_all(identity.join("devices")).expect("identity dir");
        let config = dir.join("arreo.toml");
        std::fs::write(
            &config,
            format!(
                "[relay]\nenabled = true\naddr = \"{}\"\naccount = \"{account}\"\n",
                relay.addr
            ),
        )
        .expect("config");
        Self {
            dir,
            config,
            configured: true,
        }
    }

    /// Write this client's device identity, the way `arreo pair --join` does:
    /// `identity/device.key` and `identity/devices/<bare hex>.cert`.
    fn identify(&self, root: &RootKey, key: &DeviceKey, name: &str, serial: u64) -> DeviceCert {
        let identity = self.dir.join("identity");
        std::fs::create_dir_all(identity.join("devices")).expect("identity dir");
        key.save(&identity.join("device.key")).expect("device key");
        let cert = DeviceCert::issue(root, &key.public(), name, Role::Owner, 1_000, serial);
        cert.save(&identity.join("devices")).expect("certificate");
        cert
    }

    fn run(&self, args: &[&str]) -> Out {
        let mut command = Command::new(binary("arreo"));
        command
            .args(args)
            // `identity_root()` is `$ARREO_IDENTITY_DIR/identity`, so the env
            // var names the *base* directory (the same convention `arreo pair`
            // and the daemon use).
            .env("ARREO_IDENTITY_DIR", &self.dir);
        if self.configured {
            command.env("ARREO_CONFIG", &self.config);
        } else {
            command.env_remove("ARREO_CONFIG");
        }
        Out::of(command.output().expect("the CLI runs"))
    }

    /// `--config` (or nothing at all, for a machine that has none).
    fn config_args(&self) -> Vec<String> {
        if self.configured {
            vec!["--config".to_string(), self.config.display().to_string()]
        } else {
            Vec::new()
        }
    }

    /// Run with `--config` instead of the environment variable.
    fn run_with_config(&self, args: &[&str]) -> Out {
        let mut all: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        all.push("--config".to_string());
        all.push(self.config.display().to_string());
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        let output = Command::new(binary("arreo"))
            .args(&refs)
            .env("ARREO_IDENTITY_DIR", &self.dir)
            .env_remove("ARREO_CONFIG")
            .output()
            .expect("the CLI runs");
        Out::of(output)
    }
}

/// One run's outcome, with the streams kept apart.
///
/// Separately, not interleaved: `--json` writes the envelope to stdout while a
/// warning may land on stderr, and a test that concatenated them would fail to
/// parse a perfectly good answer (or worse, parse an interleaved one).
struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Out {
    fn of(output: std::process::Output) -> Self {
        Self {
            code: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }
    }

    /// Both streams, for "did anything say this".
    fn all(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }

    /// The JSON envelope from stdout.
    fn json(&self) -> serde_json::Value {
        serde_json::from_str(self.stdout.trim())
            .unwrap_or_else(|e| panic!("stdout is not one JSON envelope ({e}): {}", self.stdout))
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The admitting machine's `arreo pair`, with its stdout read line by line.
///
/// The reader is kept alive on purpose: dropping the read end of a child's
/// stdout pipe makes the child's next `println!` fail, and a test that closed it
/// would be measuring its own harness.
struct PairingServer {
    child: Child,
    lines: std::io::BufReader<std::process::ChildStdout>,
}

impl PairingServer {
    fn start(admitting: &Client, relay: &Relay) -> Self {
        let mut command = Command::new(binary("arreo"));
        command
            .args(["pair", "--json", "--ttl-secs", "30"])
            .args(admitting.config_args())
            .args(["--mailbox", &relay.mailbox().display().to_string()])
            .arg("--socket")
            .arg(admitting.dir.join("arreo.sock"))
            .env("ARREO_IDENTITY_DIR", &admitting.dir);
        if admitting.configured {
            command.env_remove("ARREO_CONFIG");
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the admitting machine shows a code");
        let stdout = child.stdout.take().expect("stdout is piped");
        Self {
            child,
            lines: std::io::BufReader::new(stdout),
        }
    }

    /// The JSON invite the admitting machine prints before it blocks.
    fn invite(&mut self) -> serde_json::Value {
        let mut line = String::new();
        self.lines
            .read_line(&mut line)
            .expect("the admitting machine keeps talking");
        assert!(!line.is_empty(), "it stopped printing early");
        serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("the invite is not JSON ({e}): {line}"))
    }

    /// The JSON result it prints once the joining machine is in.
    fn result(&mut self) -> serde_json::Value {
        let mut line = String::new();
        self.lines
            .read_line(&mut line)
            .expect("the admitting machine keeps talking");
        assert!(!line.is_empty(), "it stopped printing early");
        serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("the result is not JSON ({e}): {line}"))
    }
}

impl Drop for PairingServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Register a machine in the account through the wire, the way a machine's own
/// daemon does (T-0056).
async fn join_machine(session: &RelaySession, machine: &RootKey, account: &str, name: &str) {
    let key = machine.public_hex();
    let payload = join_proof_payload(session.nonce(), account, &key, name);
    let request = arreo_core::relay::JoinRequest {
        v: arreo_core::relay::RELAY_VERSION,
        name: name.to_string(),
        proto_version: arreo_core::proto::VERSION,
        machine_key: key,
        signature: machine.sign(&payload).to_bytes().to_vec(),
    };
    let reply = session
        .join_machine(request)
        .await
        .expect("the relay answers");
    assert_eq!(reply.refused, None, "the machine joins: {reply:?}");
}

/// The main path: `list --json`, the human table, `status`, and the exit codes.
#[test]
fn list_and_status_read_the_accounts_directory() {
    let relay = Relay::start("read");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());

    let client = Client::new("read", &relay, "acct-1");
    let key = DeviceKey::generate().expect("entropy");
    client.identify(&root, &key, "laptop", 1);

    // Two machines in the account, written the only way anything writes one.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let session = RelaySession::dial(relay.addr, "acct-1", &key, &client_cert(&client))
            .await
            .expect("the client registers");
        join_machine(
            &session,
            &RootKey::generate().expect("entropy"),
            "acct-1",
            "workbox",
        )
        .await;
        join_machine(
            &session,
            &RootKey::generate().expect("entropy"),
            "acct-1",
            "pi",
        )
        .await;
    });

    // --json is the contract.
    let out = client.run_with_config(&["machines", "list", "--json"]);
    assert_eq!(out.code, 0, "a reachable relay is exit 0: {}", out.all());
    let value = out.json();
    assert_eq!(value["schema"], serde_json::json!(1));
    assert_eq!(value["source"], serde_json::json!("relay"));
    let names: Vec<&str> = value["machines"]
        .as_array()
        .expect("an array")
        .iter()
        .map(|m| m["name"].as_str().expect("name"))
        .collect();
    assert_eq!(
        names,
        vec!["pi", "workbox"],
        "sorted by name, whatever order the relay used: {}",
        out.all()
    );
    for machine in value["machines"].as_array().expect("array") {
        assert_eq!(
            machine["machine_id"].as_str().expect("id").len(),
            32,
            "the bare hex id is the machine id (one spelling): {machine}"
        );
        assert_eq!(machine["presence"], serde_json::json!("online"));
        assert_eq!(machine["proto_version"], serde_json::json!(0));
        assert!(machine["age_secs"].as_i64().is_some());
    }

    // The human table says the same thing, and is not the contract.
    let table = client.run(&["machines", "list"]);
    assert_eq!(table.code, 0, "{}", table.all());
    assert!(table.stdout.contains("NAME"), "{}", table.all());
    assert!(table.stdout.contains("workbox"), "{}", table.all());
    assert!(
        !table.all().contains("from cache"),
        "a live read must not claim to be a cache: {}",
        table.all()
    );

    // status <name>: the stable subset, and the honest `unknown` for the count
    // T-0046 will provide.
    let out = client.run(&["machines", "status", "workbox"]);
    assert_eq!(out.code, 0, "{}", out.all());
    assert!(out.stdout.contains("workbox"), "{}", out.all());
    assert!(out.stdout.contains("online"), "{}", out.all());
    assert!(
        out.stdout.contains("trusted devices unknown"),
        "the trusted-device count must say unknown, never a fabricated 0: {}",
        out.all()
    );

    let out = client.run_with_config(&["machines", "status", "workbox", "--json"]);
    assert_eq!(out.code, 0, "{}", out.all());
    assert_eq!(out.json()["machines"].as_array().expect("array").len(), 1);

    // Unknown machines are exit 3, and a name that is not a name is explained.
    let out = client.run(&["machines", "status", "nosuch"]);
    assert_eq!(out.code, 3, "{}", out.all());
    assert!(out.stderr.contains("no machine named"), "{}", out.all());
    let out = client.run(&["machines", "status", "NOT A NAME"]);
    assert_eq!(out.code, 3, "{}", out.all());
    assert!(out.stderr.contains("not a machine name"), "{}", out.all());

    // `add` needs a code and an invite; with neither it says which is missing
    // rather than dialing anything (the whole handoff is tested separately).
    let out = client.run(&["machines", "add"]);
    assert_eq!(out.code, 2, "`machines add`: {}", out.all());
    assert!(out.stderr.contains("pairing code"), "{}", out.all());
}

/// The offline path: rows are still printed, labelled, and never silently
/// promoted to `online`; `--offline` makes the same answer intentional.
#[test]
fn an_unreachable_relay_answers_from_the_cache_and_says_so() {
    let mut relay = Relay::start("offline");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let client = Client::new("offline", &relay, "acct-1");
    let key = DeviceKey::generate().expect("entropy");
    client.identify(&root, &key, "laptop", 1);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let session = RelaySession::dial(relay.addr, "acct-1", &key, &client_cert(&client))
            .await
            .expect("the client registers");
        join_machine(
            &session,
            &RootKey::generate().expect("entropy"),
            "acct-1",
            "workbox",
        )
        .await;
    });

    // First read fills the cache.
    let out = client.run(&["machines", "list", "--json"]);
    assert_eq!(out.code, 0, "{}", out.all());

    relay.stop();

    // Without --offline: the rows are printed, the source is honest, exit is 4.
    let out = client.run(&["machines", "list", "--json"]);
    assert_eq!(
        out.code,
        4,
        "an unreachable relay is exit 4 even when rows are printed: {}",
        out.all()
    );
    let value = out.json();
    assert_eq!(value["source"], serde_json::json!("cache"));
    let machines = value["machines"].as_array().expect("array");
    assert_eq!(
        machines.len(),
        1,
        "the cached row is still printed: {}",
        out.all()
    );
    assert_eq!(machines[0]["name"], serde_json::json!("workbox"));
    assert_ne!(
        machines[0]["presence"],
        serde_json::json!("online"),
        "a cached row must never be presented as online: {}",
        out.all()
    );

    // The table says where the rows came from, so a human is not misled either.
    let table = client.run(&["machines", "list"]);
    assert_eq!(table.code, 4, "{}", table.all());
    assert!(
        table.stdout.contains("from cache"),
        "the table must say the rows are remembered: {}",
        table.all()
    );

    // --offline: the same answer, on purpose, exit 0.
    let out = client.run(&["machines", "list", "--json", "--offline"]);
    assert_eq!(
        out.code,
        0,
        "--offline is a choice, not a failure: {}",
        out.all()
    );
    let value = out.json();
    assert_eq!(value["source"], serde_json::json!("cache"));
    assert_eq!(value["machines"].as_array().expect("array").len(), 1);

    // And status names the row from the cache too.
    let out = client.run(&["machines", "status", "workbox", "--offline"]);
    assert_eq!(out.code, 0, "{}", out.all());
    assert!(out.stdout.contains("from cache"), "{}", out.all());
    assert!(
        out.stdout.contains("unknown"),
        "and never a fabricated count: {}",
        out.all()
    );
}

/// The write verbs through the real CLI: rename live and refused, remove with
/// the tombstone holding the name, and the stale prune.
#[test]
fn rename_and_remove_write_the_directory_and_never_leave_partial_state() {
    let mut relay = Relay::start("write");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let client = Client::new("write", &relay, "acct-1");
    let key = DeviceKey::generate().expect("entropy");
    client.identify(&root, &key, "laptop", 1);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let session = RelaySession::dial(relay.addr, "acct-1", &key, &client_cert(&client))
            .await
            .expect("the client registers");
        join_machine(
            &session,
            &RootKey::generate().expect("entropy"),
            "acct-1",
            "workbox",
        )
        .await;
        join_machine(
            &session,
            &RootKey::generate().expect("entropy"),
            "acct-1",
            "pi",
        )
        .await;
    });

    // A rename to a free name is reported with the name the directory now holds.
    let out = client.run(&["machines", "rename", "pi", "pi-2"]);
    assert_eq!(out.code, 0, "{}", out.all());
    assert!(out.stdout.contains("pi-2"), "{}", out.all());

    // A rename onto a live name is exit 5, and *nothing* changes.
    let out = client.run(&["machines", "rename", "pi-2", "workbox"]);
    assert_eq!(out.code, 5, "{}", out.all());
    let listed = client.run_with_config(&["machines", "list", "--json"]);
    let names: Vec<String> = listed.json()["machines"]
        .as_array()
        .expect("array")
        .iter()
        .map(|m| m["name"].as_str().expect("name").to_string())
        .collect();
    assert_eq!(
        names,
        vec!["pi-2", "workbox"],
        "a refused rename leaves both names untouched: {}",
        listed.all()
    );

    // An unknown name is exit 3, and does not reach the relay's rules.
    let out = client.run(&["machines", "rename", "ghost", "whatever"]);
    assert_eq!(out.code, 3, "{}", out.all());
    let out = client.run(&["machines", "rename", "pi-2", "NOT A NAME"]);
    assert_eq!(out.code, 5, "{}", out.all());
    assert!(out.stderr.contains("not a machine name"), "{}", out.all());

    // Removing an online machine takes the deliberate word: without it, exit 5
    // and nothing written.
    let out = client.run(&["machines", "remove", "workbox"]);
    assert_eq!(out.code, 5, "{}", out.all());
    assert!(out.stderr.contains("--force"), "{}", out.all());
    let listed = client.run_with_config(&["machines", "list", "--json"]);
    assert_eq!(
        listed.json()["machines"].as_array().expect("array").len(),
        2,
        "a refused removal changes nothing: {}",
        listed.all()
    );

    // With --force it is tombstoned, and the name is held.
    let out = client.run(&["machines", "remove", "workbox", "--force"]);
    assert_eq!(out.code, 0, "{}", out.all());
    assert!(out.stdout.contains("removed workbox"), "{}", out.all());
    assert!(
        out.stdout.contains("held until"),
        "the operator is told what the tombstone does: {}",
        out.all()
    );
    // The tombstoned row is visible with --all, and its name is still taken.
    let listed = client.run_with_config(&["machines", "list", "--json", "--all"]);
    let rows = listed.json()["machines"].as_array().expect("array").clone();
    let workbox = rows
        .iter()
        .find(|row| row["name"] == serde_json::json!("workbox"))
        .expect("the tombstoned row is listed with --all");
    assert_eq!(
        workbox["flags"],
        serde_json::json!(["name-tombstoned"]),
        "the row says the name is reserved: {}",
        listed.all()
    );
    // Without --all it is not listed: a name whose machine is gone is not a
    // machine you can attach to.
    let listed = client.run_with_config(&["machines", "list", "--json"]);
    assert_eq!(
        listed.json()["machines"].as_array().expect("array").len(),
        1,
        "the tombstoned row is hidden without --all: {}",
        listed.all()
    );

    // The prune is idempotent and reclaims nothing here: every machine was seen
    // seconds ago, so none is stale.
    for _ in 0..2 {
        let out = client.run(&["machines", "remove", "--stale"]);
        assert_eq!(out.code, 0, "{}", out.all());
        assert!(out.stdout.contains("nothing was stale"), "{}", out.all());
    }

    // `--offline` is a read-side idea: a write cannot fall back to memory, and
    // saying so beats accepting a flag that would do nothing.
    let out = client.run(&["machines", "remove", "--stale", "--offline"]);
    assert_eq!(out.code, 2, "{}", out.all());
    assert!(out.stderr.contains("--offline"), "{}", out.all());
    relay.stop();
    let out = client.run(&["machines", "rename", "pi-2", "pi-3"]);
    assert_eq!(
        out.code,
        4,
        "an unreachable relay means the write did not happen: {}",
        out.all()
    );
}

/// The whole point (T-0058): a machine that has never seen this account joins it
/// with one command, and the account can see it afterwards.
///
/// Three real processes: the relay (router **and** pairing mailbox), the
/// admitting machine's `arreo pair`, and the joining machine's
/// `arreo machines add`. Nothing is stubbed, because the claim under test is
/// exactly the handoff between them.
#[test]
fn add_takes_a_new_machine_into_the_account() {
    let relay = Relay::start_with_mailbox("add");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());

    // The admitting machine: it holds the account root (the only key that can
    // issue a certificate the relay will verify) and it knows the relay.
    let adm = Client::new("add-admitter", &relay, "acct-1");
    adm.identify(
        &root,
        &DeviceKey::generate().expect("entropy"),
        "admitting",
        1,
    );
    root.save(&adm.dir.join("identity").join("root.key"))
        .expect("the account root key belongs to the admitting machine");

    // It shows a code. The invite must carry the account and the relay, or the
    // joining machine has nowhere to register.
    let mut pairing = PairingServer::start(&adm, &relay);
    let invite = pairing.invite();
    let code = invite["code"].as_str().expect("code").to_string();
    let uri = invite["uri"].as_str().expect("uri").to_string();
    let parsed = arreo_core::pairing::Invite::parse_uri(&uri).expect("the invite parses");
    assert_eq!(
        parsed.directory.as_ref().map(|d| d.account.as_str()),
        Some("acct-1"),
        "the admitting machine's [relay] section must reach the invite: {uri}"
    );
    assert_eq!(
        parsed.directory.as_ref().map(|d| d.relay.as_str()),
        Some(relay.addr.to_string().as_str()),
        "{uri}"
    );

    // The joining machine: a fresh identity directory, no configuration, no
    // relay of its own — everything it needs is in the invite.
    let joined = Client::new_bare("add-joiner");
    let out = joined.run(&["machines", "add", &code, "--uri", &uri, "--name", "the-pi"]);
    assert_eq!(out.code, 0, "joining must succeed: {}", out.all());
    let text = out.all();
    assert!(
        text.contains("the-pi"),
        "the granted name is reported: {text}"
    );
    assert!(
        text.contains("joined as"),
        "the device it is now known as: {text}"
    );

    // It has an identity now — the claim `add` makes about the machine.
    let identity = joined.dir.join("identity");
    assert!(
        identity.join("device.key").exists(),
        "a device key was saved"
    );
    let cert = client_cert(&joined);
    assert_eq!(cert.name(), "the-pi");
    assert!(
        identity.join("server.key").exists(),
        "the server key is pinned"
    );

    // And the account can see the machine, read by an independent third device.
    let reader_key = DeviceKey::generate().expect("entropy");
    let reader_cert = DeviceCert::issue(
        &root,
        &reader_key.public(),
        "reader",
        Role::Viewer,
        1_000,
        7,
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let listed = runtime.block_on(async {
        let session = RelaySession::dial(relay.addr, "acct-1", &reader_key, &reader_cert)
            .await
            .expect("the reader registers");
        session.machines(false).await.expect("the relay answers")
    });
    assert_eq!(
        listed.machines.len(),
        1,
        "the joining machine asserted its own row: {:?}",
        listed.machines
    );
    assert_eq!(listed.machines[0].name.as_str(), "the-pi");
    // The row is keyed by the *joining* machine's own root key: it registered
    // itself, rather than the admitting machine registering it.
    // The machine's own key, durable: `add` must persist it, or the row it just
    // wrote would be keyed by a key that vanishes with the process.
    let root_path = identity.join("root.key");
    assert!(root_path.exists(), "the joined machine's key was saved");
    let joined_root = RootKey::load_or_generate(&root_path).expect("joined root key");
    assert_eq!(
        listed.machines[0].machine_id,
        arreo_core::mesh::MachineId::from_key(&joined_root.public()),
        "the machine asserted its own identity, not one assigned to it"
    );
    assert_ne!(
        listed.machines[0].machine_id,
        arreo_core::mesh::MachineId::from_key(&root.public()),
        "and it is not the admitting machine"
    );

    let result = pairing.result();
    assert_eq!(result["paired"], serde_json::json!(true), "{result}");
    assert_eq!(result["name"], serde_json::json!("the-pi"));
}

/// A colliding name is suffixed, not refused: T-0043's rule for a *claim*, and
/// the joining machine is told which name it actually got.
#[test]
fn add_reports_the_name_the_relay_actually_granted() {
    let relay = Relay::start_with_mailbox("add-collide");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let adm = Client::new("add-collide-admitter", &relay, "acct-1");
    adm.identify(
        &root,
        &DeviceKey::generate().expect("entropy"),
        "admitting",
        1,
    );
    root.save(&adm.dir.join("identity").join("root.key"))
        .expect("root key");

    // A machine already holds "workbox", registered the ordinary way.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let seed_key = DeviceKey::generate().expect("entropy");
    let seed_cert = DeviceCert::issue(&root, &seed_key.public(), "seed", Role::Owner, 1_000, 2);
    runtime.block_on(async {
        let session = RelaySession::dial(relay.addr, "acct-1", &seed_key, &seed_cert)
            .await
            .expect("the seed registers");
        join_machine(
            &session,
            &RootKey::generate().expect("entropy"),
            "acct-1",
            "workbox",
        )
        .await;
    });

    let mut pairing = PairingServer::start(&adm, &relay);
    let invite = pairing.invite();
    let code = invite["code"].as_str().expect("code").to_string();
    let uri = invite["uri"].as_str().expect("uri").to_string();

    let joined = Client::new_bare("add-collide-joiner");
    let out = joined.run(&["machines", "add", &code, "--uri", &uri, "--name", "workbox"]);
    assert_eq!(out.code, 0, "{}", out.all());
    let text = out.all();
    assert!(
        text.contains("workbox-2"),
        "the granted name is the suffixed one, and it is reported: {text}"
    );
    assert!(
        text.contains("was taken"),
        "and the reason is stated, so the operator is not left guessing: {text}"
    );
    let _ = pairing.result();
}

/// An invite with no account or relay cannot join anything, and says so before
/// it burns the pairing session.
#[test]
fn add_refuses_an_invite_that_names_no_account() {
    let relay = Relay::start_with_mailbox("add-noaccount");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    // An admitting machine with **no** relay configuration: its invite is an
    // ordinary pairing invite.
    let adm = Client::new_bare("add-noaccount-admitter");
    root.save(&adm.dir.join("identity").join("root.key"))
        .expect("root key");
    adm.identify(
        &root,
        &DeviceKey::generate().expect("entropy"),
        "admitting",
        1,
    );

    let mut pairing = PairingServer::start(&adm, &relay);
    let invite = pairing.invite();
    let code = invite["code"].as_str().expect("code").to_string();
    let uri = invite["uri"].as_str().expect("uri").to_string();
    assert!(
        !uri.contains("&a="),
        "no relay configured means no directory hint: {uri}"
    );

    let joined = Client::new_bare("add-noaccount-joiner");
    let out = joined.run(&["machines", "add", &code, "--uri", &uri]);
    assert_eq!(
        out.code,
        4,
        "there is nothing to join, and the machine is told why: {}",
        out.all()
    );
    assert!(out.stderr.contains("names no account"), "{}", out.all());
}

/// The trust surface through the real CLI (T-0059): the command T-0046's refusals
/// tell operators to run, now that it exists.
///
/// Local, not over the relay: trust is this machine's own decision, so these need
/// no relay at all — which is the point of the design (an operator must be able to
/// fix a machine whose daemon will not start).
#[test]
fn trust_grants_lists_and_cuts_access_locally() {
    let client = Client::new_bare("trust-local");
    // A device for this machine to decide about, pinned through the product's own
    // door (which also records the pairing default grant).
    let device = DeviceKey::generate().expect("entropy");
    let id = DeviceId::from_key(&device.public());
    let issued = client.run(&[
        "devices",
        "issue",
        "--name",
        "phone",
        "--role",
        "owner",
        "--key",
        &device.public_hex(),
    ]);
    assert_eq!(issued.code, 0, "{}", issued.all());

    // The pairing default is visible, in the operator's words.
    let listed = client.run(&["machines", "trust", "--list"]);
    assert_eq!(listed.code, 0, "{}", listed.all());
    assert!(listed.stdout.contains(&id.display_id()), "{}", listed.all());
    assert!(
        listed.stdout.contains("operator"),
        "the roadmap's word for the role, not the certificate's: {}",
        listed.all()
    );

    let machine_name = arreo_core::mesh::default_machine_name();

    // Cut this machine's grant only.
    let cut = client.run(&[
        "devices",
        "revoke",
        &id.display_id(),
        "--machine",
        &machine_name,
    ]);
    assert_eq!(cut.code, 0, "{}", cut.all());
    assert!(
        cut.stdout.contains("keeps whatever access"),
        "{}",
        cut.all()
    );

    let listed = client.run(&["machines", "trust", "--list", "--json"]);
    let value = listed.json();
    assert_eq!(
        value["grants"][0]["live"],
        serde_json::json!(false),
        "the grant is revoked: {value}"
    );
    assert!(
        value["grants"][0]["revoked_at"].as_str().is_some(),
        "{value}"
    );

    // Restore it with the command the refusal would print.
    let granted = client.run(&[
        "machines",
        "trust",
        &id.display_id(),
        "--role",
        "operator",
        "--yes",
    ]);
    assert_eq!(granted.code, 0, "{}", granted.all());
    let listed = client.run(&["machines", "trust", "--list", "--json"]);
    assert_eq!(listed.json()["grants"][0]["live"], serde_json::json!(true));

    // Trust is local: another machine's name is refused, and nothing changes.
    let other = client.run(&[
        "machines",
        "trust",
        &id.display_id(),
        "--machine",
        "some-other-machine",
        "--yes",
    ]);
    assert_eq!(other.code, 5, "{}", other.all());
    assert!(
        other.stderr.contains("Trust is local"),
        "the refusal explains why: {}",
        other.all()
    );

    // A device this machine never pinned would be inert: refused, so a typo in a
    // fingerprint is caught instead of recorded.
    let stranger = DeviceKey::generate().expect("entropy");
    let unpinned = client.run(&[
        "machines",
        "trust",
        &DeviceId::from_key(&stranger.public()).display_id(),
        "--yes",
    ]);
    assert_eq!(unpinned.code, 3, "{}", unpinned.all());
    assert!(
        unpinned.stderr.contains("not pinned on this machine"),
        "{}",
        unpinned.all()
    );

    // Revoking a grant on a machine that is not this one is refused, not
    // silently applied to the local ledger.
    let wrong_machine = client.run(&[
        "devices",
        "revoke",
        &id.display_id(),
        "--machine",
        "some-other-machine",
    ]);
    assert_eq!(wrong_machine.code, 5, "{}", wrong_machine.all());
    assert!(
        wrong_machine.stderr.contains("not this machine"),
        "{}",
        wrong_machine.all()
    );

    // And revoking the *device* reports the machine-local grant it leaves behind.
    let revoked = client.run(&["devices", "revoke", &id.display_id()]);
    assert_eq!(revoked.code, 0, "{}", revoked.all());
    assert!(
        revoked.stdout.contains("still holds a live"),
        "the operator must not be left with a revoked device and a live grant: {}",
        revoked.all()
    );
    assert!(
        revoked.stdout.contains("--machine"),
        "and is told the command that fixes it: {}",
        revoked.all()
    );
}

/// A confirmation is required unless `--yes`: an authorization that writes itself
/// when a human hits enter is how the wrong device gets trusted.
#[test]
fn trust_asks_before_granting() {
    let client = Client::new_bare("trust-confirm");
    let device = DeviceKey::generate().expect("entropy");
    let id = DeviceId::from_key(&device.public());
    assert_eq!(
        client
            .run(&[
                "devices",
                "issue",
                "--name",
                "phone",
                "--role",
                "viewer",
                "--key",
                &device.public_hex(),
            ])
            .code,
        0
    );
    // Downgrade to revoked so a re-grant is a real change to observe.
    let name = arreo_core::mesh::default_machine_name();
    assert_eq!(
        client
            .run(&["devices", "revoke", &id.display_id(), "--machine", &name])
            .code,
        0
    );

    // No `--yes` and no answer on stdin: nothing changes.
    let unconfirmed = client.run(&["machines", "trust", &id.display_id()]);
    assert_eq!(unconfirmed.code, 5, "{}", unconfirmed.all());
    assert!(
        unconfirmed.stderr.contains("not confirmed"),
        "{}",
        unconfirmed.all()
    );
    assert_eq!(
        client
            .run(&["machines", "trust", "--list", "--json"])
            .json()["grants"][0]["live"],
        serde_json::json!(false),
        "an unconfirmed grant must not be recorded"
    );
}

/// A relay that is not configured at all is exit 4 with a message that says
/// which file was read — an operator is never left guessing why there is no
/// directory.
#[test]
fn a_missing_relay_configuration_is_exit_4() {
    let relay = Relay::start("noconfig");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let client = Client::new("noconfig", &relay, "acct-1");
    let key = DeviceKey::generate().expect("entropy");
    client.identify(&root, &key, "laptop", 1);
    // A configuration that names no relay (the file is gone, so the section is
    // not there) is exit 4: the directory is unreachable and the machine says so.
    std::fs::remove_file(&client.config).expect("remove config");
    let out = client.run_with_config(&["machines", "list", "--json"]);
    assert_eq!(
        out.code,
        4,
        "no relay configured means the directory is unreachable: {}",
        out.all()
    );
    assert!(
        out.stderr.contains("no relay is configured"),
        "{}",
        out.all()
    );
    assert!(
        out.stderr.contains(&client.config.display().to_string()),
        "and names the file it read: {}",
        out.all()
    );

    // With a configuration that names no relay there is nothing to write to
    // either, and the write path says so rather than falling back to a cache.
    let out = client.run(&["machines", "rename", "pi", "pi-2"]);
    assert_eq!(out.code, 4, "{}", out.all());
    assert!(
        out.stderr.contains("no relay is configured"),
        "{}",
        out.all()
    );

    // And with no configuration named at all, the CLI asks for one instead of
    // guessing a path: exit 2 (usage), naming both ways to supply it.
    let output = Command::new(binary("arreo"))
        .args(["machines", "list"])
        .env("ARREO_IDENTITY_DIR", &client.dir)
        .env_remove("ARREO_CONFIG")
        .output()
        .expect("the CLI runs");
    assert_eq!(output.status.code().unwrap_or(-1), 2);
    let text = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(text.contains("--config"), "{text}");
    assert!(text.contains("ARREO_CONFIG"), "{text}");
}

/// A CLI with no paired identity cannot read the account, and says which file is
/// missing instead of panicking or printing an empty list as if it were true.
#[test]
fn an_unpaired_client_is_told_what_is_missing() {
    let relay = Relay::start("unpaired");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let client = Client::new("unpaired", &relay, "acct-1");
    // Deliberately no identify(): no device key, no certificate.

    let out = client.run_with_config(&["machines", "list", "--json"]);
    assert_eq!(out.code, 4, "{}", out.all());
    assert!(
        out.stderr.contains("device.key"),
        "the message names the missing file: {}",
        out.all()
    );
}

/// The certificate this client's identity dir holds, loaded the way the CLI
/// loads it (so the test cannot pass by holding a different object).
fn client_cert(client: &Client) -> DeviceCert {
    let identity = client.dir.join("identity");
    let key = DeviceKey::load(&identity.join("device.key")).expect("device key");
    let id = arreo_core::identity::DeviceId::from_key(&key.public());
    let path = identity
        .join("devices")
        .join(format!("{}.cert", id.as_str()));
    DeviceCert::load(&path).expect("certificate")
}

/// The config file the client writes must be one the *daemon's* loader also
/// accepts: one parser, one answer (the CLI reads `arreo_core::relay::config`).
#[test]
fn the_test_config_is_one_the_shared_loader_accepts() {
    let relay = Relay::start("config-shape");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let client = Client::new("config-shape", &relay, "acct-1");
    let settings = arreo_core::relay::config::load_config(&client.config)
        .expect("the shared loader parses it")
        .expect("and it enables the relay");
    assert_eq!(settings.account, "acct-1");
    assert_eq!(settings.addr, relay.addr);
}
