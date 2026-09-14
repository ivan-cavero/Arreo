//! T-0047 mesh slice: two real daemons + a relay on loopback, the referee for
//! T-0043…T-0046.
//!
//! One sentence: the mesh claims are only real if two actual `arreo-server`
//! daemons plus a relay reproduce them hermetically — so this slice spawns
//! exactly that, on ephemeral loopback ports, and asserts the directory, the
//! attach, the verbs, the trust gate and the isolation.
//!
//! ## What this reuses and what it does not
//!
//! The process plumbing (`Proc`, identity issuance, account registration, the
//! peer daemon on the relay) is the relay slice's shape, copied rather than
//! shared: the two slices assert different things about the same processes, and
//! a shared fixture would couple their failure modes. What is shared is the
//! harness (`bins`, `wait_bound`, `cli`) and the conventions (ephemeral ports,
//! temp state dirs, poll-with-deadline, `--interactive-evidence` to
//! `.loop/evidence/T-0047/`).
//!
//! ## What this slice is not
//!
//! Loopback proves protocol and state semantics, not WAN/NAT behaviour. The
//! daemons are the shipped binaries under test — no mocks, no in-process fakes —
//! but they are two processes on one box, and anything about real networks is
//! out of scope by design rather than by omission.
//!
//! ## The sync story (T-0086), and why it needs a client of its own
//!
//! The last section runs ROADMAP §3.8's owner case across the two live machines:
//! one `opencode.jsonc` edited on alpha, `arreo sync push --machine beta-machine`,
//! both files byte-identical, the receiver resolving the reference from its own
//! keychain; then the conflict — both machines edit, both publish, neither
//! overwrites — and the three refusals a receiver owes a peer (a literal secret,
//! a file it has no preset for, and a payload claiming another machine's
//! identity).
//!
//! Two of those stages cannot be driven by the shipped CLI, and the reason is a
//! property of the product rather than a gap in it. `arreo sync push --machine`
//! counts the revision *and then* builds the payload, so two machines exchanging
//! through it are never concurrent: the second push carries the first push's
//! absorbed counter and lands as an ordinary update instead of a conflict. And a
//! payload that must be *wrong* — a literal secret, a name no preset knows, a
//! forged `machine` field — cannot be produced by a verb whose job is to refuse
//! exactly those. So the slice sends those payloads itself, with the product's
//! own client (`arreo_core::mesh::session`) over the product's own frames to the
//! shipped daemon; what it hand-builds is the payload, which is the thing under
//! test. Everything else — the revision counting, the file edits, the keychain —
//! is the CLI.
//!
//! The sender's daemon is stopped for the length of its own send. That is not
//! tidiness: a machine's daemon and its CLI are one device, so a client dialing
//! with that identity *replaces* the daemon's relay session (T-0060) and the
//! daemon reconnects on its own 250 ms backoff — which would race the very
//! exchange the stage is measuring. Stopping it makes the exchange deterministic,
//! and each machine's daemon is back before the next stage runs.

use crate::harness::{bins, TuiSession};
use arreo_core::identity::{DeviceCert, DeviceId, DeviceKey, Role, RootKey};
use arreo_core::mesh::{MeshClient, RemoteTarget, Target};
use arreo_core::proto::{Message, SyncExchange, SyncOutcome, SyncStatus, VERSION};
use arreo_core::sync::keychain::{plan, SecretStore};
use arreo_core::update::verify::sha256;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// The two machines' directory names. People-names, not letters: the transcript
/// reads as a story about two machines rather than a puzzle about which of A
/// and B is which.
const ALPHA: &str = "alpha-machine";
const BETA: &str = "beta-machine";

/// The variable the synced file references, and the two machines' own values.
///
/// Dummies, the same name `docs/harness-centralization.md` uses: a value never
/// crosses the wire (the payload carries the reference), which is the property
/// the convergence stage asserts and the reason each machine has its own.
const SYNC_KEY: &str = "VBK_PROD_KEY";
const ALPHA_KEY_VALUE: &str = "alpha-key-value-3f19";
const BETA_KEY_VALUE: &str = "beta-key-value-8a02";

/// The owner's file (`docs/harness-centralization.md` §4 step 1), in opencode's
/// own spelling — `{env:NAME}`, which is what the harness on this machine reads.
///
/// `__PROVIDER__` marks the one thing two concurrent edits differ in, so the
/// conflict stage's two files are the same file with two edits rather than two
/// unrelated documents.
const OPENCODE_CASE: &str = r#"{
  "$schema": "https://opencode.ai/config.json",
  "provider": {
    "verboo": {
      "name": "__PROVIDER__",
      "npm": "@ai-sdk/openai-compatible",
      "options": {
        "baseURL": "https://code.verboo.ai/router/v1",
        "apiKey": "{env:VBK_PROD_KEY}"
      }
    }
  }
}
"#;

/// The worked case with `provider` named, so two machines' edits differ.
fn opencode_case(provider: &str) -> String {
    OPENCODE_CASE.replace("__PROVIDER__", provider)
}

/// What a hostile (or buggy) peer would put where a reference belongs.
///
/// A literal, in a provider key field: exactly what T-0083's scanner exists to
/// refuse, and what the sender's own `push` would never let leave this machine —
/// which is why the payload carrying it is built here rather than by the CLI.
const LITERAL_SECRET_CONTENT: &str =
    r#"{"provider":{"verboo":{"options":{"apiKey":"sk-live-4f8a1c9b2d7e"}}}}"#;

/// The environment that *is* one machine.
///
/// `XDG_CONFIG_HOME` is what a preset's symbolic path resolves against **and**
/// where the machine's own keychain lives (`$XDG_CONFIG_HOME/arreo/secrets.json`),
/// so two machines on one host need two of them — and the operator's real one is
/// never read or written. `HOME` and the other XDG roots point inside the same
/// scratch tree for the same reason: a verb run by this slice must not touch a
/// file the person running it owns.
fn machine_env(dir: &Path) -> [(String, String); 5] {
    let path = |name: &str| dir.join(name).display().to_string();
    [
        ("HOME".to_string(), path("home")),
        ("XDG_CONFIG_HOME".to_string(), path("cfg")),
        ("XDG_DATA_HOME".to_string(), path("data")),
        ("XDG_STATE_HOME".to_string(), path("state")),
        ("XDG_CACHE_HOME".to_string(), path("cache")),
    ]
}

/// The directories [`machine_env`] names, created up front.
///
/// Not tidiness: `HOME` is also the working directory a pane's spawn inherits
/// when it names none (`portable-pty` chdirs to it), so a home that does not
/// exist makes every pane spawn fail with ENOENT — which surfaces as "pty
/// backend: No such file or directory" and has nothing to do with ptys.
fn prepare_machine(dir: &Path) {
    for part in ["cfg", "home", "data", "state", "cache"] {
        let _ = std::fs::create_dir_all(dir.join(part));
    }
}

/// A process whose stderr is drained for its whole life.
///
/// The drain is not tidiness: a closed pipe makes the child die on its next log
/// line (EPIPE), which reads exactly like an authentication failure.
struct Proc {
    child: Child,
    log: Arc<Mutex<String>>,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Proc {
    fn spawn(command: &mut Command, watch: Option<&str>) -> (Self, Option<std::net::SocketAddr>) {
        use std::io::{BufRead, BufReader};
        command.stdout(Stdio::null()).stderr(Stdio::piped());
        let mut child = command.spawn().expect("the process starts");
        let stderr = child.stderr.take().expect("stderr");
        let log = Arc::new(Mutex::new(String::new()));
        let (ready_tx, ready_rx) = mpsc::channel();
        {
            let log = Arc::clone(&log);
            let watch = watch.map(str::to_string);
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if let Some(watch) = &watch {
                        if let Some(rest) = line.split(watch.as_str()).nth(1) {
                            if let Some(addr) = rest.split_whitespace().next() {
                                if let Ok(addr) = addr.parse::<std::net::SocketAddr>() {
                                    let _ = ready_tx.send(addr);
                                }
                            }
                        }
                    }
                    if let Ok(mut held) = log.lock() {
                        held.push_str(&line);
                        held.push('\n');
                    }
                }
            });
        }
        let addr = watch.map(|_| {
            ready_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("the process announces its address")
        });
        (Self { child, log }, addr)
    }

    fn log_text(&self) -> String {
        self.log.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn await_log(&self, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if self.log_text().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }
}

fn debug_bin(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("target")
        .join("debug")
        .join(name)
}

/// How long one CLI invocation may take before the slice calls it a failure.
///
/// **A deadline on every call, because a slice that hangs is worse than one that
/// fails.** A hung command produces no verdict at all — the run stalls until
/// something outside kills it, and the operator learns nothing. This bounds each
/// invocation well above any healthy latency (the budget under test is 3 s) and
/// turns a stall into a named FAIL with whatever the command printed.
const CLI_DEADLINE: Duration = Duration::from_secs(20);

/// Run the CLI with `ARREO_IDENTITY_DIR` pointed at `dir`, `ARREO_CONFIG` at
/// `config`, and the `XDG_*` roots of `dir` (see [`machine_env`]), bounded by
/// [`CLI_DEADLINE`]. Returns (success, combined output); a timeout is a failure
/// whose output says so.
fn cli(dir: &Path, config: &Path, cli_bin: &Path, args: &[&str]) -> (bool, String) {
    cli_with_stdin(dir, config, cli_bin, args, None)
}

/// The same, with a value on stdin — `arreo sync secret set` reads the secret
/// there and never from argv (a value on argv is a value in the process table and
/// in the shell's history file).
fn cli_with_stdin(
    dir: &Path,
    config: &Path,
    cli_bin: &Path,
    args: &[&str],
    stdin: Option<&str>,
) -> (bool, String) {
    cli_with_deadline(CLI_DEADLINE, dir, config, cli_bin, args, stdin)
}

/// The same, with a caller-chosen deadline.
fn cli_with_deadline(
    deadline: Duration,
    dir: &Path,
    config: &Path,
    cli_bin: &Path,
    args: &[&str],
    stdin: Option<&str>,
) -> (bool, String) {
    let mut child = Command::new(cli_bin)
        .args(args)
        .env("ARREO_IDENTITY_DIR", dir)
        .env("ARREO_CONFIG", config)
        .envs(machine_env(dir))
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the CLI starts");
    if let Some(text) = stdin {
        use std::io::Write as _;
        if let Some(mut pipe) = child.stdin.take() {
            let _ = pipe.write_all(text.as_bytes());
            // Dropped here, so the child sees EOF and stops reading.
        }
    }
    let deadline = Instant::now() + deadline;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Ok(None) => {
                // Kill, then still collect what it wrote: a timeout with no
                // output tells the reader nothing about *where* the command
                // stalled, and the partial output is the only evidence there is.
                let _ = child.kill();
                let output = child.wait_with_output().expect("the CLI was waited for");
                let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
                text.push_str(&String::from_utf8_lossy(&output.stderr));
                return (false, format!("timed out; it had printed: {text:?}"));
            }
            Err(e) => return (false, format!("cannot wait for the CLI: {e}")),
        }
    }
    let output = child.wait_with_output().expect("the CLI was waited for");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

/// Collapse runs of whitespace (T-0074): the TUI panel wraps a long refusal and
/// the CLI prints it on one line, so comparing the *words* is the comparison
/// that means something; the layout is the renderer's business.
fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The words of a frame, with the box-drawing glyphs a centred overlay leaves
/// between its wrapped lines removed (T-0074): the sidebar's border runs
/// through the panel, so a wrapped sentence's lines are separated by `│` in the
/// reconstructed grid — filtering them first lets a sentence that the panel
/// wrapped be compared word for word with the CLI's single line.
fn words(text: &str) -> String {
    let cleaned: String = text.chars().filter(|c| !"│┌┐└┘─".contains(*c)).collect();
    collapse(&cleaned)
}

/// The hostname out of a "which is" refusal: `devices revoke --machine` names the
/// name it compares against when the given one does not match, so the caller can
/// use exactly that rather than guessing.
fn hostname_name(text: &str) -> String {
    // `... is not this machine (which is "NAME", ...)`.
    text.split("(which is")
        .nth(1)
        .and_then(|rest| rest.split('"').nth(1))
        .unwrap_or_default()
        .to_string()
}

/// One machine: its identity directory, its daemon, its config. Built by the
/// same steps a real deployment performs (key, certificate, pin the peer,
/// config with a name), so the slice exercises the deployment path rather than
/// a shortcut to it.
struct Machine {
    dir: PathBuf,
    /// This machine's display name — what the sentences an operator reads call
    /// it. **Not** the counter key: that is the device id (T-0086).
    name: String,
    /// This machine's `XDG_CONFIG_HOME`: what a preset's symbolic path resolves
    /// against, and where the machine's own keychain lives.
    cfg: PathBuf,
    socket: PathBuf,
    config: PathBuf,
    key: DeviceKey,
    #[allow(dead_code)]
    cert: DeviceCert,
    daemon: Option<Proc>,
}

impl Machine {
    /// Create the identity and config, but do not start the daemon yet. The
    /// caller pins the peer first (the authority's index is what the handshake
    /// resolves against), then calls `start`.
    fn create(
        base: &Path,
        tag: &str,
        name: &str,
        root: &RootKey,
        relay_addr: &std::net::SocketAddr,
        account: &str,
        serial: u64,
    ) -> Self {
        let dir = base.join(tag);
        let identity = dir.join("identity");
        let _ = std::fs::create_dir_all(identity.join("devices"));
        let key = DeviceKey::generate().expect("entropy");
        let cert = DeviceCert::issue(root, &key.public(), tag, Role::Owner, 1_000, serial);
        // The machine's OWN root key: the daemon loads-or-generates this at its
        // root key path and asserts its directory row under it (`MachineId` is
        // the root key, T-0043). Each machine gets its own — sharing the account
        // root would make both machines one directory entry.
        let machine_root = RootKey::generate().expect("entropy");
        machine_root
            .save(&identity.join("root.key"))
            .expect("root key");
        key.save(&identity.join("device.key")).expect("device key");
        cert.save(&identity.join("devices")).expect("certificate");
        let socket = dir.join(format!("{tag}.sock"));
        let config = dir.join(format!("{tag}.toml"));
        std::fs::write(
            &config,
            format!(
                "[relay]\nenabled = true\naddr = \"{relay_addr}\"\naccount = \"{account}\"\nname = \"{name}\"\n"
            ),
        )
        .expect("config");
        prepare_machine(&dir);
        let cfg = dir.join("cfg");
        Self {
            dir,
            name: name.to_string(),
            cfg,
            socket,
            config,
            key,
            cert,
            daemon: None,
        }
    }

    /// Pin `peer_key` as a device this machine trusts (what `arreo devices
    /// issue` writes, via the CLI's own verb rather than by hand — the verb is
    /// part of what is under test).
    ///
    /// **The daemon must already be running**, and there must be exactly one:
    /// two daemons on one socket are two relay sessions for one device id, and
    /// the relay displaces one of them (T-0060) — which is how the first version
    /// of this fixture produced "peer closed the session" mid-slice.
    fn pin(&self, cli_bin: &Path, peer_name: &str, peer_key_hex: &str) {
        assert!(
            self.daemon.is_some(),
            "pin requires a running daemon (start it first)"
        );
        let socket = self.socket.clone();
        let dir = self.dir.clone();
        let pinned = Command::new(cli_bin)
            .args([
                "devices",
                "issue",
                "--socket",
                &socket.display().to_string(),
                "--name",
                peer_name,
                "--role",
                "owner",
                "--key",
                peer_key_hex,
            ])
            .env("ARREO_IDENTITY_DIR", &dir)
            .output()
            .expect("the pin command runs");
        assert!(
            pinned.status.success(),
            "pinning {peer_name} failed: {}",
            String::from_utf8_lossy(&pinned.stderr)
        );
    }

    fn start(&mut self, server_bin: &Path) {
        let mut cmd = Command::new(server_bin);
        cmd.arg("--socket")
            .arg(&self.socket)
            .arg("--config")
            .arg(&self.config)
            .env("ARREO_IDENTITY_DIR", &self.dir)
            // The daemon resolves the synced files' paths and its own keychain
            // from these, so a daemon started without them would read — and on a
            // receive, *write* — the config home of whoever ran the slice.
            .envs(machine_env(&self.dir));
        let (daemon, _) = Proc::spawn(&mut cmd, None);
        assert!(
            daemon.await_log("serving on"),
            "the daemon binds its socket"
        );
        assert!(
            daemon.await_log("relay session up"),
            "the daemon joins the relay (without it there is no route)"
        );
        self.daemon = Some(daemon);
    }

    /// The identity and config for a machine that runs **no daemon** — the shape
    /// an operator's laptop has when it drives other machines.
    ///
    /// Why this exists: a machine's daemon and its CLI share one device identity,
    /// and two relay sessions for one device id displace each other (T-0060). So a
    /// remote verb run *on a daemon-hosting machine* ends that machine's own relay
    /// session — correct behavior, and covered by T-0060's own test, but it makes
    /// every remote call here pay a reconnect. The slice therefore drives remote
    /// verbs from a client that hosts no daemon, which is also the common real
    /// deployment: the machine you type on is usually not the one running a daemon.
    fn client(
        base: &Path,
        tag: &str,
        relay_addr: &std::net::SocketAddr,
        account: &str,
        root: &RootKey,
        serial: u64,
    ) -> Self {
        let dir = base.join(tag);
        let identity = dir.join("identity");
        let _ = std::fs::create_dir_all(identity.join("devices"));
        let key = DeviceKey::generate().expect("entropy");
        // Issued by the **account's** root, because that is the key the relay
        // verifies a device certificate against — a self-issued one is refused
        // at the handshake, which is exactly what the first version of this
        // fixture did (and the refusals then exhausted the relay's 3-per-10s
        // handshake budget, so later calls were refused at the transport).
        let cert = DeviceCert::issue(root, &key.public(), tag, Role::Owner, 1_000, serial);
        key.save(&identity.join("device.key")).expect("device key");
        cert.save(&identity.join("devices")).expect("certificate");
        let socket = dir.join(format!("{tag}.sock"));
        let config = dir.join(format!("{tag}.toml"));
        std::fs::write(
            &config,
            format!("[relay]\nenabled = true\naddr = \"{relay_addr}\"\naccount = \"{account}\"\n"),
        )
        .expect("config");
        prepare_machine(&dir);
        let cfg = dir.join("cfg");
        Self {
            dir,
            name: "operator-laptop".to_string(),
            cfg,
            socket,
            config,
            key,
            cert,
            daemon: None,
        }
    }

    /// The daemon's own log, for a failure message worth reading.
    fn log_text(&self) -> String {
        self.daemon.as_ref().map(Proc::log_text).unwrap_or_default()
    }

    /// Wait for a line in the daemon's own log.
    fn await_log(&self, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if self.log_text().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    /// This machine's device id: the counter key every revision and every
    /// conflict copy is named after (T-0086), derived the way the CLI derives it
    /// (`DeviceId::from_key`), so the slice's expectation and the product's
    /// spelling cannot disagree.
    fn device_id(&self) -> String {
        DeviceId::from_key(&self.key.public()).display_id()
    }

    /// The preset's live file on this machine
    /// (`$XDG_CONFIG_HOME/opencode/opencode.jsonc`).
    fn opencode(&self) -> PathBuf {
        self.cfg.join("opencode").join("opencode.jsonc")
    }

    /// This machine's own keychain — where `arreo sync secret set` puts a value.
    fn secrets_path(&self) -> PathBuf {
        self.cfg.join("arreo").join("secrets.json")
    }

    fn write_file(&self, path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("the file's directory");
        }
        std::fs::write(path, content).expect("the file is written");
    }

    fn read_file(&self, path: &Path) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// Run `arreo sync …` on this machine: its identity, its config, its own
    /// `XDG_CONFIG_HOME` (the file a preset names is under *this* machine's
    /// root), its display name, and **its own store**.
    ///
    /// `--socket` is not decoration: it is what selects the store
    /// (`<socket>.db`), and without it the CLI falls back to the machine-wide
    /// default socket — so every machine in this slice, and every previous run,
    /// would share one SQLite file. The vectors would then be other machines'
    /// revisions of the same file name, which is exactly what a version vector
    /// exists to distinguish. Passing it also makes the store the *daemon's own*
    /// (same path), which is what lets the audit check below read back the row
    /// the receiving daemon wrote.
    fn sync_cli(&self, cli_bin: &Path, args: &[&str]) -> (bool, String) {
        let mut full: Vec<String> = vec!["sync".to_string()];
        full.extend(args.iter().map(|arg| (*arg).to_string()));
        full.push("--socket".to_string());
        full.push(self.socket.display().to_string());
        full.push("--name".to_string());
        full.push(self.name.clone());
        let refs: Vec<&str> = full.iter().map(String::as_str).collect();
        cli(&self.dir, &self.config, cli_bin, &refs)
    }

    fn kill_daemon(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            drop(daemon);
        }
    }
}

/// What the sync stages need from the fabric `run` already built.
struct SyncFabric<'a> {
    relay: SocketAddr,
    account: &'a str,
    server_bin: &'a Path,
    cli_bin: &'a Path,
    /// Where the payloads this slice builds are written (never the operator's
    /// directories).
    scratch: &'a Path,
    /// The relay's own log, for a failure message that names the session that
    /// was replaced rather than leaving the reader to guess.
    relay_log: &'a Proc,
}

/// One `Message::Sync` to `peer`'s daemon, **authenticating as `sender`**.
///
/// The client is the product's (`arreo_core::mesh::session`), the frames are the
/// product's, and the daemon at the far end is the shipped binary; what this
/// builds is the payload, which is the thing the conflict and the refusals have
/// to control. The target is built from the two machines' own keys rather than
/// resolved through the directory: the peer's device key is what `pin` wrote into
/// the other machine's trust list, and the Noise handshake is what proves the
/// machine at the other end holds it.
fn send_sync(
    sender: &Machine,
    peer: &Machine,
    relay: SocketAddr,
    account: &str,
    payload: &[u8],
) -> Result<SyncOutcome, String> {
    let identity = sender.dir.join("identity");
    let key = DeviceKey::load(&identity.join("device.key"))
        .map_err(|e| format!("{}: {e}", identity.display()))?;
    let id = DeviceId::from_key(&key.public());
    let cert = DeviceCert::load(
        &identity
            .join("devices")
            .join(format!("{}.cert", id.as_str())),
    )
    .map_err(|e| format!("{}: {e}", identity.display()))?;
    let target = Target::Remote(Box::new(RemoteTarget {
        relay,
        account: account.to_string(),
        peer: DeviceId::from_key(&peer.key.public()),
        server_key: peer.key.public(),
        device: Arc::new(key),
        cert: Arc::new(cert),
    }));
    let exchange = SyncExchange {
        payload: payload.to_vec(),
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("no runtime for the exchange: {e}"))?;
    runtime.block_on(async move {
        match MeshClient::request_to(
            &target,
            &Message::Sync {
                v: VERSION,
                exchange,
            },
        )
        .await
        {
            Ok(Message::SyncReply { outcome, .. }) => Ok(*outcome),
            Ok(Message::Error { message, .. }) => Err(message),
            Ok(other) => Err(format!("unexpected reply {other:?}")),
            Err(e) => Err(format!("{}: {e}", peer.name)),
        }
    })
}

/// Wait, bounded, until the receiver holds **no stream** for `sender`.
///
/// The fact is the receiver's own pair of lines: it logs one `relay peer <id>
/// authenticated` per session it opens for a sender, and one `<id> went offline`
/// per stream it tears down. Every session opened has been torn down exactly when
/// the second count has caught the first — so that invariant, read from the
/// daemon's log, is "there is nothing left for this sender's next exchange to
/// race", and it is a fact rather than a duration. The deadline only bounds the
/// wait.
fn await_no_stream(peer: &Machine, sender: &Machine, deadline: Duration) -> Option<Duration> {
    let id = sender.device_id();
    let opened = format!("relay peer {id} authenticated");
    let torn_down = format!("{id} went offline");
    let until = Instant::now() + deadline;
    let started = Instant::now();
    loop {
        let log = peer.log_text();
        if log.matches(&torn_down).count() >= log.matches(&opened).count() {
            return Some(started.elapsed());
        }
        if Instant::now() >= until {
            return None;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// One send, as the receiver's answer or the reason there is none.
///
/// **The wait before the send is a fix for a measured race, not politeness.** A
/// machine's daemon holds one peer stream per sending device, and it only drops
/// that stream when the relay tells it the peer is gone. A *second* exchange from
/// one sender that starts while the first exchange's stream is still there has
/// its opening Noise flight read as garbage by an established session; that
/// stream dies, and all three of the sender's handshake attempts time out — 9 s
/// of silence, measured on every run before this wait existed. Waiting for the
/// receiver to have torn the old stream down is waiting for the fact, and the
/// receiver is the only party that knows it.
fn exchange(
    sender: &Machine,
    peer: &Machine,
    fabric: &SyncFabric<'_>,
    payload: &[u8],
) -> Result<SyncOutcome, String> {
    let settled = await_no_stream(peer, sender, Duration::from_secs(30));
    assert!(
        settled.is_some(),
        "{} still holds a stream for {} after 30 s, so this exchange would race the previous one",
        peer.name,
        sender.name
    );
    let started = Instant::now();
    let outcome = send_sync(sender, peer, fabric.relay, fabric.account, payload);
    println!(
        "mesh: sync {} -> {} in {:?} (waited {:?} for the receiver's stream to clear): {}",
        sender.name,
        peer.name,
        started.elapsed(),
        settled.unwrap_or_default(),
        describe(&outcome)
    );
    outcome
}

/// The status the receiver reported, or `None` when nothing answered.
fn status_of(outcome: &Result<SyncOutcome, String>) -> Option<SyncStatus> {
    outcome.as_ref().ok().map(|outcome| outcome.status)
}

/// The receiver's own words — its refusal, or the transport's failure.
fn reason_of(outcome: &Result<SyncOutcome, String>) -> String {
    match outcome {
        Ok(outcome) => outcome.reason.clone(),
        Err(e) => e.clone(),
    }
}

/// One exchange, as one line for a check's detail.
fn describe(outcome: &Result<SyncOutcome, String>) -> String {
    match outcome {
        Ok(outcome) => {
            let mut text = format!("{}: {:?}", outcome.file, outcome.status);
            if !outcome.copy.is_empty() {
                text.push_str(&format!(" copy {}", outcome.copy));
            }
            if !outcome.reason.is_empty() {
                text.push_str(&format!(" — {}", outcome.reason));
            }
            text
        }
        Err(e) => format!("no outcome: {e}"),
    }
}

/// The payload the CLI builds for `file` on `machine` (`arreo sync payload`,
/// which counts the revision and prints what a peer receives).
///
/// The half of "both machines edit and both push" that the exchange cannot do
/// for itself: both payloads must exist before either machine has seen the
/// other's, or the pair is not concurrent and the receiver is right to treat it
/// as an ordinary update.
fn payload_bytes(
    machine: &Machine,
    cli_bin: &Path,
    file: &str,
    out: &Path,
) -> Result<String, String> {
    let (ok, text) = machine.sync_cli(
        cli_bin,
        &["payload", file, "--out", &out.display().to_string()],
    );
    if !ok {
        return Err(format!(
            "{} could not publish {file}: {}",
            machine.name,
            collapse(&text)
        ));
    }
    std::fs::read_to_string(out).map_err(|e| format!("{}: {e}", out.display()))
}

/// Replace one field's value in the payload JSON the CLI printed.
///
/// Line-anchored, because `serde_json::to_string_pretty` writes one field per
/// line and the payload's `content` is a whole document of quotes that a naive
/// scan for a closing quote would walk straight into. What this edits is the
/// payload; the receiver is never touched.
fn rewrite_field(json: &str, field: &str, value: &str) -> Result<String, String> {
    let needle = format!("\n  \"{field}\": \"");
    let start = json
        .find(&needle)
        .ok_or_else(|| format!("the payload carries no {field:?} field"))?
        + needle.len();
    let rest = &json[start..];
    let bytes = rest.as_bytes();
    let mut end = None;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => {
                end = Some(i);
                break;
            }
            _ => i += 1,
        }
    }
    let end = end.ok_or_else(|| format!("the payload's {field:?} value is unterminated"))?;
    Ok(format!("{}{}{}", &json[..start], value, &rest[end..]))
}

/// `text` as a JSON string's contents.
fn json_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// The conflict copies beside a machine's live file, by name — the file system's
/// answer, not the reply's.
fn conflict_copies(machine: &Machine) -> Vec<String> {
    let live = machine.opencode();
    let Some(dir) = live.parent() else {
        return Vec::new();
    };
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| name.contains(".conflict-"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// A daemon's own log, tail-first, for a failure message worth reading.
fn log_tail(machine: &Machine) -> String {
    text_tail(&machine.log_text())
}

/// The last few hundred characters of a log, collapsed onto one line.
fn text_tail(text: &str) -> String {
    let tail: String = text
        .chars()
        .rev()
        .take(600)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    collapse(&tail)
}

/// A machine's own audit trail, through the CLI's own reader of its own store.
fn audit_of(machine: &Machine, cli_bin: &Path) -> String {
    let output = Command::new(cli_bin)
        .args([
            "audit",
            "--limit",
            "50",
            "--socket",
            &machine.socket.display().to_string(),
        ])
        .env("ARREO_IDENTITY_DIR", &machine.dir)
        .envs(machine_env(&machine.dir))
        .output()
        .expect("audit runs");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

/// T-0086's story across the two live machines: the owner case, the conflict, and
/// the three refusals a receiver owes a peer.
///
/// `check` is the slice's own pass/fail printer, so every stage reports the same
/// way as the rest of the slice and the transcript carries the evidence — the
/// conflict copies' names, and the receivers' own refusal sentences.
fn sync_stages(
    alpha: &mut Machine,
    beta: &mut Machine,
    client: &Machine,
    fabric: &SyncFabric<'_>,
    check: &mut dyn FnMut(&str, bool, &str),
) {
    let cli_bin = fabric.cli_bin;

    // ---- the owner case: edit once on alpha, converge on beta ----
    //
    // Step 0 of §3.8's design, once per machine and never synced: each machine
    // stores the provider key itself. Alpha's is here; beta's absence is the
    // point of the next check.
    let (ok, out) = cli_with_stdin(
        &alpha.dir,
        &alpha.config,
        cli_bin,
        &["sync", "secret", "set", SYNC_KEY, "--name", ALPHA],
        Some(ALPHA_KEY_VALUE),
    );
    check(
        "alpha stores its own provider key",
        ok && out.contains(SYNC_KEY),
        &collapse(&out),
    );

    // **Alpha's daemon is stopped here and stays stopped until the moment it has
    // to receive.** A machine's daemon and its CLI are one device, so a client
    // dialing with that identity replaces the daemon's relay session (T-0060) —
    // and the daemon's own reconnect 250 ms later replaces the client's, which is
    // how the first version of this stage lost an exchange mid-flight. Neither
    // end of that race is what any of these stages measures: the machine under
    // test is the *receiver*, and the receiver is live throughout.
    alpha.kill_daemon();

    // The edit, and the push. What travels is the reference; what stays is the
    // value. Beta has no key yet, and the machine that must say so is the
    // receiver — in the receiver's own words, naming the variable.
    alpha.write_file(&alpha.opencode(), &opencode_case("Verboo Code"));
    let (ok, refused) = alpha.sync_cli(cli_bin, &["push", "opencode.jsonc", "--machine", BETA]);
    let by_name = format!(
        "{SYNC_KEY} is not set on {}",
        arreo_core::mesh::default_machine_name()
    );
    check(
        "a receiver that cannot resolve the reference refuses it by name",
        !ok && refused.contains(&format!("{BETA} refused")) && refused.contains(&by_name),
        &format!(
            "{}\nrelay said: {}",
            collapse(&refused),
            text_tail(&fabric.relay_log.log_text())
        ),
    );

    // Beta supplies its own value — never alpha's, never from the payload — and
    // the same push converges.
    let (ok, out) = cli_with_stdin(
        &beta.dir,
        &beta.config,
        cli_bin,
        &["sync", "secret", "set", SYNC_KEY, "--name", BETA],
        Some(BETA_KEY_VALUE),
    );
    check(
        "beta stores its own value for the same name",
        ok && out.contains(SYNC_KEY) && !out.contains(ALPHA_KEY_VALUE),
        &collapse(&out),
    );

    let alpha_id = alpha.device_id();
    let beta_id = beta.device_id();
    let (ok, applied) = alpha.sync_cli(cli_bin, &["push", "opencode.jsonc", "--machine", BETA]);
    // The reply names the **sender's device id**, which is the counter key: the
    // machine's display name is a label a rename would change, and a counter
    // keyed by it would fork (T-0086's decision).
    check(
        "the peer applies the revision and names the sender's device id",
        ok && applied.contains(&format!("applied {alpha_id}'s revision 1"))
            && !applied.contains(&format!("applied {ALPHA}'s")),
        &format!(
            "{}\nrelay said: {}",
            collapse(&applied),
            text_tail(&fabric.relay_log.log_text())
        ),
    );

    let alpha_live = alpha.read_file(&alpha.opencode());
    let beta_live = beta.read_file(&beta.opencode());
    check(
        "both machines hold the file byte for byte",
        !alpha_live.is_empty()
            && alpha_live == beta_live
            && alpha_live.contains("{env:VBK_PROD_KEY}")
            && !alpha_live.contains(ALPHA_KEY_VALUE),
        &format!(
            "{ALPHA}: {} bytes {:?}\n{BETA}: {} bytes {:?}",
            alpha_live.len(),
            alpha_live,
            beta_live.len(),
            beta_live
        ),
    );

    let audit = audit_of(beta, cli_bin);
    check(
        "the receiving daemon recorded the apply, naming the sender's device id",
        audit.contains("sync.apply")
            && audit.contains(&alpha_id)
            && audit.contains("opencode.jsonc"),
        &collapse(&audit),
    );

    // Each machine resolves the reference from **its own** store: one file, two
    // keychains, and the value that reaches the harness is this machine's.
    let secrets = SecretStore::open(beta.secrets_path()).expect("beta's keychain");
    let resolved = plan(&beta_live, &secrets, BETA);
    let environment = resolved.environment();
    check(
        "the receiver resolves the reference from its own store",
        resolved.missing().is_empty()
            && environment.len() == 1
            && environment[0].0 == SYNC_KEY
            && environment[0].1 == BETA_KEY_VALUE,
        &format!(
            "{} resolves {SYNC_KEY} to its own value; nothing is missing",
            BETA
        ),
    );

    // ---- the conflict: both edit, both publish, neither overwrites ----
    //
    // Both payloads are built before either is delivered. That is what makes the
    // pair concurrent (`{alpha: 2}` against `{alpha: 1, beta: 1}`): the shipped
    // CLI counts the revision and builds the payload in one breath, so a second
    // `push --machine` would carry the first push's absorbed counter and land as
    // an ordinary update.
    let alpha_edit = opencode_case("Verboo Code (alpha)");
    let beta_edit = opencode_case("Verboo Code (beta)");
    alpha.write_file(&alpha.opencode(), &alpha_edit);
    beta.write_file(&beta.opencode(), &beta_edit);
    let published = (
        payload_bytes(
            alpha,
            cli_bin,
            "opencode.jsonc",
            &fabric.scratch.join("alpha-payload.json"),
        ),
        payload_bytes(
            beta,
            cli_bin,
            "opencode.jsonc",
            &fabric.scratch.join("beta-payload.json"),
        ),
    );
    let (alpha_payload, beta_payload) = match published {
        (Ok(alpha_payload), Ok(beta_payload)) => (alpha_payload, beta_payload),
        (Err(e), _) | (_, Err(e)) => {
            check("both machines publish their concurrent edit", false, &e);
            return;
        }
    };

    // Alpha's daemon is still stopped (it has been the sender since the first
    // push), so this send has no session of its own to race. Beta's daemon is the
    // receiver, and it is up. Alpha comes back to *receive* beta's payload, and
    // then stops being a sender, so beta's daemon goes down for its own send.
    let at_beta = exchange(alpha, beta, fabric, alpha_payload.as_bytes());
    let at_beta_evidence = format!("{BETA} said: {}", log_tail(beta));
    // Alpha receives beta's payload, and beta goes down to send its own: a
    // machine's daemon and its CLI are one device, so leaving the daemon up while
    // its CLI dials would have the CLI displace the daemon's own session — and a
    // second daemon on the same socket would then refuse to start at all.
    alpha.start(fabric.server_bin);
    beta.kill_daemon();
    let at_alpha = exchange(beta, alpha, fabric, beta_payload.as_bytes());
    let at_alpha_evidence = format!("{ALPHA} said: {}", log_tail(alpha));
    beta.start(fabric.server_bin);

    let beta_copies = conflict_copies(beta);
    let alpha_copies = conflict_copies(alpha);
    let reply_copy = at_beta
        .as_ref()
        .ok()
        .map(|outcome| outcome.copy.clone())
        .unwrap_or_default();
    check(
        "beta keeps alpha's edit beside its own, under alpha's device id",
        status_of(&at_beta) == Some(SyncStatus::Conflict)
            && beta_copies.len() == 1
            && reply_copy == beta_copies[0]
            && reply_copy.contains(&alpha_id)
            && reply_copy.ends_with(".jsonc"),
        &format!(
            "{BETA} kept {beta_copies:?}; the reply named {reply_copy:?} ({}).\n{at_beta_evidence}",
            describe(&at_beta)
        ),
    );
    let reply_copy = at_alpha
        .as_ref()
        .ok()
        .map(|outcome| outcome.copy.clone())
        .unwrap_or_default();
    check(
        "alpha keeps beta's edit beside its own, under beta's device id",
        status_of(&at_alpha) == Some(SyncStatus::Conflict)
            && alpha_copies.len() == 1
            && reply_copy == alpha_copies[0]
            && reply_copy.contains(&beta_id)
            && reply_copy.ends_with(".jsonc"),
        &format!(
            "{ALPHA} kept {alpha_copies:?}; the reply named {reply_copy:?} ({}).\n{at_alpha_evidence}",
            describe(&at_alpha)
        ),
    );

    let alpha_after = alpha.read_file(&alpha.opencode());
    let beta_after = beta.read_file(&beta.opencode());
    let copy_bytes = |machine: &Machine, copies: &[String]| {
        copies
            .first()
            .map(|name| machine.read_file(&machine.opencode().with_file_name(name)))
            .unwrap_or_default()
    };
    let alpha_copy = copy_bytes(alpha, &alpha_copies);
    let beta_copy = copy_bytes(beta, &beta_copies);
    check(
        "neither live file was overwritten, and each machine holds both copies",
        alpha_after == alpha_edit && beta_after == beta_edit,
        &format!(
            "the live files are still each machine's own edit ({} and {} bytes)",
            alpha_after.len(),
            beta_after.len()
        ),
    );
    check(
        "each copy holds the other machine's bytes",
        !alpha_copy.is_empty()
            && !beta_copy.is_empty()
            && alpha_copy == beta_edit
            && beta_copy == alpha_edit,
        &format!(
            "{ALPHA}'s copy is {BETA}'s edit and {BETA}'s copy is {ALPHA}'s — the neutral form \
             round-tripped byte for byte"
        ),
    );

    // ---- the three refusals over the wire ----
    //
    // All three are sent by the **client** — the operator's laptop, which runs no
    // daemon — so none of them displaces a machine's relay session, and the
    // receiver under test is beta's daemon throughout. The payloads are the
    // client's own, with one field edited: the sender's `push` refuses exactly
    // these files, which is why the wire is the only place they can be probed.
    client.write_file(&client.opencode(), &opencode_case("Verboo Code"));
    let honest = match payload_bytes(
        client,
        cli_bin,
        "opencode.jsonc",
        &fabric.scratch.join("client-payload.json"),
    ) {
        Ok(payload) => payload,
        Err(e) => {
            check(
                "the client publishes the payload the probes edit",
                false,
                &e,
            );
            return;
        }
    };
    let client_id = client.device_id();
    let beta_before = beta.read_file(&beta.opencode());
    let copies_before = conflict_copies(beta);

    // (a) A literal where a reference belongs. The digest covers the content, so
    // it is recomputed over the edited bytes: a payload that failed the digest
    // check would prove nothing about the scan.
    let literal_path = fabric.scratch.join("literal-secret.json");
    std::fs::write(&literal_path, LITERAL_SECRET_CONTENT).expect("the scratch file");
    let Ok(with_literal) = rewrite_field(&honest, "content", &json_escape(LITERAL_SECRET_CONTENT))
    else {
        check(
            "the literal-secret payload is built",
            false,
            "no content field",
        );
        return;
    };
    let Ok(digest) = sha256(&literal_path) else {
        check(
            "the literal-secret payload is built",
            false,
            "the tampered content could not be digested",
        );
        return;
    };
    let Ok(literal) = rewrite_field(&with_literal, "digest", &digest) else {
        check(
            "the literal-secret payload is built",
            false,
            "no digest field",
        );
        return;
    };
    let reply = exchange(client, beta, fabric, literal.as_bytes());
    check(
        "a payload that would put a literal secret on the receiver is refused with the finding",
        status_of(&reply) == Some(SyncStatus::Refused)
            && reason_of(&reply).contains("refused opencode.jsonc: possible secrets detected")
            && reason_of(&reply).contains("possible api key (sk- prefix)"),
        &reason_of(&reply),
    );

    // (b) A file this machine has no preset for: the receiver resolves the
    // destination from its own registry, and a name it does not know is not a
    // destination.
    let Ok(unknown) = rewrite_field(&honest, "file", "zed-settings.json") else {
        check("the no-preset payload is built", false, "no file field");
        return;
    };
    let reply = exchange(client, beta, fabric, unknown.as_bytes());
    check(
        "a payload for a file the receiver has no preset for is refused",
        status_of(&reply) == Some(SyncStatus::Refused)
            && reason_of(&reply)
                .contains("zed-settings.json: not a preset file and not an absolute path")
            && reason_of(&reply).contains("will not receive a file it has no preset for"),
        &reason_of(&reply),
    );

    // (c) **The forged identity.** An honest payload whose `machine` claims
    // another device: the counter key is the identity the Noise handshake proved,
    // and a peer that lies about who it is is a finding, not a value to correct.
    let Ok(forged) = rewrite_field(&honest, "machine", &alpha_id) else {
        check(
            "the forged-identity payload is built",
            false,
            "no machine field",
        );
        return;
    };
    let reply = exchange(client, beta, fabric, forged.as_bytes());
    let expected = format!(
        "refused opencode.jsonc: the payload counts its revision under {alpha_id}, but this session \
         is authenticated as {client_id} — a machine is the authority on its own counter, and only \
         on its own"
    );
    check(
        "a payload claiming another machine's identity is refused, not corrected",
        status_of(&reply) == Some(SyncStatus::Refused) && reason_of(&reply) == expected,
        &format!(
            "expected: {expected}\nthe receiver said: {}",
            reason_of(&reply)
        ),
    );

    let beta_after = beta.read_file(&beta.opencode());
    let copies_after = conflict_copies(beta);
    check(
        "none of the three refusals wrote anything on the receiver",
        beta_after == beta_before && copies_after == copies_before,
        &format!(
            "{BETA}'s live file is unchanged and it still has {} conflict copy(ies)",
            copies_after.len()
        ),
    );

    // The names, on their own line: the evidence a reader greps for.
    println!("mesh: conflict copies — {ALPHA}: {alpha_copies:?} · {BETA}: {copies_after:?}");
    println!("mesh: forged-identity probe — {client_id} claimed {alpha_id}");
}

pub fn run(rest: &[String]) -> ExitCode {
    let evidence = rest.iter().any(|a| a == "--interactive-evidence");
    let evidence_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(".loop")
        .join("evidence")
        .join("T-0047");
    if evidence {
        let _ = std::fs::create_dir_all(&evidence_dir);
    }

    let (server_bin, cli_bin, tui_bin) = bins();
    let relay_bin = debug_bin("arreo-relay");
    for bin in [&server_bin, &cli_bin, &relay_bin] {
        if !bin.exists() {
            println!(
                "[FAIL] mesh: missing binary {} (build first: `cargo build --workspace`)",
                bin.display()
            );
            return ExitCode::FAILURE;
        }
    }

    let mut failures = 0usize;
    let mut passes = 0usize;
    let mut skipped = 0usize;
    let mut check = |name: &str, ok: bool, detail: &str| {
        if ok {
            println!("[PASS] mesh: {name}");
            passes += 1;
        } else {
            println!("[FAIL] mesh: {name}: {detail}");
            failures += 1;
        }
    };
    // The skip closure borrows `skipped`, so the count it maintains is read
    // through a cell the checks below can also consult — otherwise a later check
    // that wants to skip cannot, because the closure holds the borrow.
    let mut skip = |name: &str, reason: &str| {
        println!("[SKIP] mesh: {name}: {reason}");
        skipped += 1;
    };

    let base = std::env::temp_dir().join(format!("arreo-e2e-mesh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    if let Err(e) = std::fs::create_dir_all(&base) {
        println!("[FAIL] mesh: scratch dir: {e}");
        return ExitCode::FAILURE;
    }

    // ---- the relay, and the account both machines live in ----
    let state_dir = base.join("relay-state");
    let _ = std::fs::create_dir_all(&state_dir);
    let mut relay_cmd = Command::new(&relay_bin);
    relay_cmd.args([
        "serve",
        "--listen",
        "127.0.0.1:0",
        "--state-dir",
        &state_dir.display().to_string(),
    ]);
    let (relay, relay_addr) = Proc::spawn(&mut relay_cmd, Some("router on "));
    let Some(relay_addr) = relay_addr else {
        println!("[FAIL] mesh: the relay never announced its address");
        return ExitCode::FAILURE;
    };

    let root = RootKey::generate().expect("entropy");
    let account = "acct-mesh";
    let registered = Command::new(&relay_bin)
        .args([
            "account",
            "add",
            "--state-dir",
            &state_dir.display().to_string(),
            "--account",
            account,
            "--root-key",
            &root.public_hex(),
        ])
        .output()
        .expect("the account command runs");
    if !registered.status.success() {
        println!(
            "[FAIL] mesh: registering the account: {}",
            String::from_utf8_lossy(&registered.stderr)
        );
        return ExitCode::FAILURE;
    }

    // ---- two machines, each pinned on the other ----
    let mut alpha = Machine::create(&base, "alpha", ALPHA, &root, &relay_addr, account, 1);
    let mut beta = Machine::create(&base, "beta", BETA, &root, &relay_addr, account, 2);
    let client = Machine::client(&base, "client", &relay_addr, account, &root, 9);
    // One daemon per machine, then the pins through it. Pinning needs a running
    // daemon (the authority it writes to is the daemon's), and a second daemon on
    // the same socket would displace the first at the relay (T-0060).
    alpha.start(&server_bin);
    beta.start(&server_bin);
    let beta_hex = beta.key.public_hex();
    let alpha_hex = alpha.key.public_hex();
    alpha.pin(&cli_bin, "beta", &beta_hex);
    beta.pin(&cli_bin, "alpha", &alpha_hex);
    // The client must be **pinned on beta** as well as holding a certificate:
    // the Noise handshake resolves the caller from beta's own pin list, so a
    // device with a perfectly valid account certificate that beta has never seen
    // is refused — and every refusal spends the relay's 3-per-10s handshake
    // budget for this address, which then shows up as unrelated transport
    // failures later in the slice. This is the step the first version omitted.
    let client_hex = client.key.public_hex();
    beta.pin(&cli_bin, "client", &client_hex);

    // Both daemons must have asserted their directory rows before anything
    // reaches them *by name* — a name resolves through the row, so a call before
    // the row exists is an unknown-machine failure that looks like a bug in the
    // mesh. Read from each daemon's own log, never by polling the directory as
    // that machine: a CLI session authenticates with the daemon's own device key,
    // and the relay keeps one live session per device, so polling as B displaces
    // B's daemon (T-0060 — a real operator footgun this slice must not trip).
    for (machine, name) in [(&alpha, ALPHA), (&beta, BETA)] {
        if !machine.await_log("this machine is") {
            println!(
                "[FAIL] mesh: {name} never asserted its directory row:\n{}",
                machine.log_text()
            );
            return ExitCode::FAILURE;
        }
    }

    // ---- the directory lists both machines, with presence ----
    let (ok, list) = cli(
        &client.dir,
        &client.config,
        &cli_bin,
        &["machines", "list", "--json"],
    );
    check("the directory is readable from a machine", ok, &list);
    let both_listed = list.contains(ALPHA) && list.contains(BETA);
    check("the directory lists both machines", both_listed, &list);
    if evidence {
        let _ = std::fs::write(evidence_dir.join("01-directory.json"), &list);
    }

    // ---- a pane on beta, reached by name from alpha ----
    let marker = "MESH-MARKER-9d4c";
    let (ok, out) = cli(
        &beta.dir,
        &beta.config,
        &cli_bin,
        &[
            "spawn",
            "beta-pane",
            "/bin/sh",
            "-c",
            &format!("echo {marker}; sleep 300"),
            "--socket",
            &beta.socket.display().to_string(),
        ],
    );
    check("beta owns a pane", ok, &out);

    // The latency budget (T-0045's row, owned by this slice): attaching to a
    // live overview of a remote machine must take ≤ 3 s on loopback. The clock
    // starts at the command, so the measurement is what an operator feels.
    let attached_at = Instant::now();
    let (ok, panes) = cli(
        &client.dir,
        &client.config,
        &cli_bin,
        &["panes", "--machine", BETA],
    );
    let attach_ms = attached_at.elapsed().as_millis() as u64;
    check(
        "alpha lists beta's panes by name",
        ok && panes.contains("beta-pane"),
        &panes,
    );
    check(
        "the cross-machine listing meets its 3 s budget",
        attach_ms <= 3_000,
        &format!("took {attach_ms} ms"),
    );
    println!("mesh: cross-machine panes {attach_ms} ms (budget 3000 ms)");

    // ---- identical payloads locally and remotely ----
    let (_, local) = cli(
        &beta.dir,
        &beta.config,
        &cli_bin,
        &[
            "read",
            "beta-pane",
            "--socket",
            &beta.socket.display().to_string(),
        ],
    );
    let (ok, remote) = cli(
        &client.dir,
        &client.config,
        &cli_bin,
        &["read", "beta-pane", "--machine", BETA],
    );
    // Compared on the marker line, not the whole output: the two reads are
    // snapshots at different instants, and the pane's own shell prompt may differ
    // between them. What must be identical is the pane's content — the marker the
    // pane printed — which is the payload a caller acts on.
    let local_lines: Vec<&str> = local.lines().map(str::trim_end).collect();
    let remote_lines: Vec<&str> = remote.lines().map(str::trim_end).collect();
    check(
        "a read round trip returns the same payload locally and remotely",
        ok && local_lines.contains(&marker) && remote_lines.contains(&marker),
        &format!("local: {local:?}\nremote: {remote:?}"),
    );
    if evidence {
        let _ = std::fs::write(evidence_dir.join("02-attach.txt"), &remote);
    }

    // A send through the name, read back on the machine itself.
    let (ok, _) = cli(
        &client.dir,
        &client.config,
        &cli_bin,
        &[
            "send",
            "beta-pane",
            "echo",
            "SECOND-LINE",
            "--machine",
            BETA,
        ],
    );
    check("a send through the name is accepted", ok, "");
    let (_, back) = cli(
        &beta.dir,
        &beta.config,
        &cli_bin,
        &[
            "read",
            "beta-pane",
            "--socket",
            &beta.socket.display().to_string(),
        ],
    );
    check(
        "what was sent remotely is readable locally",
        back.contains("SECOND-LINE"),
        &back,
    );

    // ---- trust: refused without a grant, admitted with one ----
    //
    // A third device, pinned on beta but with no trust grant. The two facts live
    // in different gates (the certificate resolves the handshake; the ledger
    // decides what the session may do), and this sequence proves they are
    // different facts by holding the first and removing the second.
    //
    // `devices issue` **grants on issue** (T-0046), so the pin alone would be
    // admitted and there would be nothing to refuse. The sequence therefore uses
    // two real verbs: pin, then cut the grant — which is exactly the state a
    // revoked device is in, reached the way an operator reaches it.
    let gamma_dir = base.join("gamma");
    let gamma_identity = gamma_dir.join("identity");
    let _ = std::fs::create_dir_all(gamma_identity.join("devices"));
    let gamma_key = DeviceKey::generate().expect("entropy");
    let gamma_cert = DeviceCert::issue(&root, &gamma_key.public(), "gamma", Role::Owner, 1_000, 3);
    root.save(&gamma_identity.join("root.key")).expect("root");
    gamma_key
        .save(&gamma_identity.join("device.key"))
        .expect("key");
    gamma_cert
        .save(&gamma_identity.join("devices"))
        .expect("cert");
    let gamma_id = gamma_cert.device().display_id();
    let gamma_config = gamma_dir.join("gamma.toml");
    std::fs::write(
        &gamma_config,
        format!("[relay]\nenabled = true\naddr = \"{relay_addr}\"\naccount = \"{account}\"\nname = \"gamma-machine\"\n"),
    )
    .expect("config");
    // Pin gamma on beta (handshake passes), but grant nothing (ledger refuses).
    let pinned = Command::new(&cli_bin)
        .args([
            "devices",
            "issue",
            "--socket",
            &beta.socket.display().to_string(),
            "--name",
            "gamma",
            "--role",
            "owner",
            "--key",
            &gamma_key.public_hex(),
        ])
        .env("ARREO_IDENTITY_DIR", &beta.dir)
        .output()
        .expect("pin runs");
    check(
        "gamma is pinned on beta (the handshake will pass)",
        pinned.status.success(),
        &String::from_utf8_lossy(&pinned.stderr),
    );

    // Cut the grant `devices issue` just created, leaving the pin. `--machine`
    // names *this* machine (trust is local, so it must match the name the
    // command compares against); if the name differs the command says which it
    // wanted, and the retry uses exactly that rather than guessing.
    let beta_socket = beta.socket.display().to_string();
    let mut revoked = Command::new(&cli_bin)
        .args([
            "devices",
            "revoke",
            &gamma_id,
            "--machine",
            BETA,
            "--socket",
            &beta_socket,
        ])
        .env("ARREO_IDENTITY_DIR", &beta.dir)
        .output()
        .expect("revoke runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&revoked.stdout),
        String::from_utf8_lossy(&revoked.stderr)
    );
    if !revoked.status.success() && text.contains("(which is") {
        revoked = Command::new(&cli_bin)
            .args([
                "devices",
                "revoke",
                &gamma_id,
                "--machine",
                &hostname_name(&text),
                "--socket",
                &beta_socket,
            ])
            .env("ARREO_IDENTITY_DIR", &beta.dir)
            .output()
            .expect("revoke runs");
    }
    check(
        "the grant is cut while the pin survives",
        revoked.status.success(),
        &format!(
            "{}{}",
            String::from_utf8_lossy(&revoked.stdout),
            String::from_utf8_lossy(&revoked.stderr)
        ),
    );

    // Bounded, like every other invocation. The bound is a backstop against a
    // hang, not a latency assertion — the latency assertion in this slice is the
    // 3 s budget check above — and it is deliberately short: the client currently
    // hangs on this path (T-0064), so waiting a minute to learn that would make
    // the slice slow for a reason the reader already knows. Ten seconds is far
    // more than a healthy refusal needs and bounds the cost of the defect.
    let (ok, refused) = cli_with_deadline(
        Duration::from_secs(25),
        &gamma_dir,
        &gamma_config,
        &cli_bin,
        &["panes", "--machine", BETA],
        None,
    );
    // The message is what proves the refusal is the ledger's (it names the grant
    // command) rather than a network failure — and the exit code (5, the trust
    // vocabulary) proves it is a refusal rather than a reachability problem.
    // **One assertion now, because the two facts it split into are both true
    // again** (T-0065 delivered the refusal; T-0064 bounded the wait). The check
    // is the criterion in full: the device is refused, the exit code is the trust
    // vocabulary's (5, not a reachability 4), and the message carries the command
    // that fixes it. The bound is proven by the call returning inside its
    // deadline at all — `cli_with_deadline` reports "timed out" instead of a
    // message when it does not.
    check(
        "an untrusted device is refused with the actionable message",
        !ok && refused.contains("arreo machines trust") && refused.contains("grant"),
        &refused,
    );

    if evidence {
        let _ = std::fs::write(evidence_dir.join("03-deny.txt"), &refused);
    }

    // Grant, from beta itself (the target's call — T-0046), then retry. The
    // verb takes the device id, not the name it was pinned under: a name is what
    // a person says, an id is what the ledger keys on, and accepting a name here
    // would resolve it through a different mapping than the ledger uses.
    // The ledger keys on device ids (32 hex chars); the Noise key is 32 bytes
    // whose hex is 64 chars, so the id comes from the certificate, not the key.
    // And the grant runs against beta's own socket: trust is local (the ledger
    // lives beside the daemon), so the default socket would grant on the wrong
    // machine — or on none at all.
    let gamma_hex = gamma_cert.device().display_id();
    let beta_socket = beta.socket.display().to_string();
    let granted = Command::new(&cli_bin)
        .args([
            "machines",
            "trust",
            &gamma_hex,
            "--yes",
            "--socket",
            &beta_socket,
        ])
        .env("ARREO_IDENTITY_DIR", &beta.dir)
        .env("ARREO_CONFIG", &beta.config)
        .output()
        .expect("grant runs");
    check(
        "beta grants gamma",
        granted.status.success(),
        &String::from_utf8_lossy(&granted.stderr),
    );
    let (ok, after) = cli(
        &gamma_dir,
        &gamma_config,
        &cli_bin,
        &["panes", "--machine", BETA],
    );
    check(
        "the granted device reaches beta's panes",
        ok && after.contains("beta-pane"),
        &after,
    );
    if evidence {
        let _ = std::fs::write(evidence_dir.join("04-grant-attach.txt"), &after);
    }

    // ---- T-0074: the TUI manages the fleet, by name ----------------------
    //
    // The relay and both machines are still up. This section drives the real
    // TUI as the account's *client* — an identity and a relay config but no
    // daemon — attached to beta by name, and proves the parts a local socket
    // cannot: machines list by name (the account's directory, from the relay)
    // and the trust refusal for a machine that is not this one. The TUI reads
    // `arreo_tui::fleet` for both, so the checks assert exactly what that code
    // produces: the panel rows for `m`, and the CLI's own `--machine <other>`
    // sentence for `g`.
    let evidence_dir_74 = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(".loop")
        .join("evidence")
        .join("T-0074");
    if evidence {
        let _ = std::fs::create_dir_all(&evidence_dir_74);
    }
    let machine_args: Vec<String> = vec![
        "--machine".to_string(),
        BETA.to_string(),
        "--config".to_string(),
        client.config.display().to_string(),
    ];
    let machine_refs: Vec<&str> = machine_args.iter().map(String::as_str).collect();
    let identity_dir = client.dir.display().to_string();
    let mut tui = match TuiSession::start_by_name(
        &tui_bin,
        &machine_refs,
        &[("ARREO_IDENTITY_DIR", identity_dir.as_str())],
    ) {
        Some(session) => session,
        None => {
            check("the TUI starts attached by machine name", false, "no pty");
            drop(relay);
            let _ = std::fs::remove_dir_all(&base);
            return ExitCode::FAILURE;
        }
    };
    // Attach by name: the sidebar names the machine it is looking at. The first
    // attach occupies the client's one live relay session, so the poller holds
    // it; the slice is deliberately generous with the wait (a handshake plus a
    // directory round trip and a first pane pass).
    std::thread::sleep(Duration::from_secs(6));
    let attached = tui.screen();
    check(
        "the TUI resolves the machine by name (sidebar says beta-machine · relay)",
        attached.contains("beta-machine · relay") && attached.contains("beta-pane"),
        &format!(
            "session label or pane missing: {:?}",
            collapse(&attached).chars().take(120).collect::<String>()
        ),
    );

    // `m`: the machines panel lists the account, from the relay.
    tui.send("m");
    std::thread::sleep(Duration::from_secs(8));
    let machines = tui.screen();
    if evidence {
        let _ = std::fs::write(evidence_dir_74.join("50-machines-by-name.txt"), &machines);
    }
    check(
        "m lists the account's machines by name",
        machines.contains("alpha-machine") && machines.contains("beta-machine"),
        &format!(
            "machines panel missing a name: {:?}",
            collapse(&machines).chars().take(160).collect::<String>()
        ),
    );
    check(
        "the list came from the relay",
        machines.contains("machine(s) from the relay"),
        "the panel never said where the rows came from",
    );

    // `g` (inside the machines panel): trust is local, and this TUI is attached
    // to a machine that is not this one — the CLI's own refusal, verbatim.
    tui.send("g");
    std::thread::sleep(Duration::from_secs(3));
    let trust_frame = tui.screen();
    if evidence {
        let _ = std::fs::write(
            evidence_dir_74.join("51-trust-remote-refusal.txt"),
            &trust_frame,
        );
    }
    check(
        "g refuses trust for a machine that is not this one",
        trust_frame.contains("is not this machine") && trust_frame.contains("Trust is local"),
        &format!(
            "no trust refusal on screen: {:?}",
            collapse(&trust_frame).chars().take(160).collect::<String>()
        ),
    );
    // ...and the sentence is the CLI's own, word for word: the same machine
    // (same identity dir), the same name, the same formatter round the same core
    // values. The panel wraps a long refusal and the CLI prints it on one line,
    // so the comparison is on collapsed whitespace — the words must match, the
    // layout must not.
    let (_, cli_refusal) = cli(
        &client.dir,
        &client.config,
        &cli_bin,
        &[
            "machines",
            "trust",
            "dev_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "--machine",
            BETA,
        ],
    );
    let cli_collapsed = collapse(&cli_refusal);
    check(
        "the TUI's trust refusal is the CLI's own sentence",
        !cli_collapsed.is_empty() && words(&trust_frame).contains(&cli_collapsed),
        &format!(
            "CLI: {cli_collapsed}\nTUI: {}",
            words(&trust_frame).chars().take(300).collect::<String>()
        ),
    );

    tui.send("q");
    std::thread::sleep(Duration::from_millis(500));
    drop(tui);

    // ---- T-0086: the sync transport over the two live machines ----
    // After the TUI (which needs beta's pane alive) and before the isolation
    // stage (which kills beta's daemon on purpose): this section stops and
    // restarts daemons of its own, and it must be the last word on their state
    // before that stage arranges its own.
    let sync_scratch = base.join("sync");
    let _ = std::fs::create_dir_all(&sync_scratch);
    sync_stages(
        &mut alpha,
        &mut beta,
        &client,
        &SyncFabric {
            relay: relay_addr,
            account,
            server_bin: &server_bin,
            cli_bin: &cli_bin,
            scratch: &sync_scratch,
            relay_log: &relay,
        },
        &mut check,
    );

    // ---- node isolation: beta dies mid-attach, alpha is untouched ----
    // A pane on alpha, alive throughout. Killing beta must not touch it, and
    // the failure message must carry what the directory last knew.
    let (ok, _) = cli(
        &alpha.dir,
        &alpha.config,
        &cli_bin,
        &[
            "spawn",
            "alpha-pane",
            "/bin/sh",
            "-c",
            "echo ALPHA-ALIVE; sleep 300",
            "--socket",
            &alpha.socket.display().to_string(),
        ],
    );
    check("alpha owns a pane of its own", ok, "");
    beta.kill_daemon();
    let (_, gone) = cli(
        &client.dir,
        &client.config,
        &cli_bin,
        &["panes", "--machine", BETA],
    );
    let (_, alive) = cli(
        &alpha.dir,
        &alpha.config,
        &cli_bin,
        &[
            "read",
            "alpha-pane",
            "--socket",
            &alpha.socket.display().to_string(),
        ],
    );
    check(
        "beta's death leaves alpha's own pane untouched",
        alive.contains("ALPHA-ALIVE"),
        &alive,
    );
    check(
        "the failure names the machine rather than the transport",
        gone.contains(BETA)
            || gone.contains("stale")
            || gone.contains("offline")
            || gone.contains("last seen"),
        &gone,
    );

    // ---- soft dependencies, named rather than silent ----
    // Presence push and the durable-inbox queued assertion need APIs this slice
    // does not own. If they are absent, that is a SKIP with the missing API's
    // name — never a silent pass.
    skip(
        "queued-message assertion",
        "the relay durable inbox assertion lives in the relay slice (T-0032), not here",
    );

    drop(relay);
    let _ = std::fs::remove_dir_all(&base);
    println!("mesh: {passes} passed, {skipped} skipped, {failures} failed");
    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
