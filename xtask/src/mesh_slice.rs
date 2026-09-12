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

use crate::harness::bins;
use arreo_core::identity::{DeviceCert, DeviceKey, Role, RootKey};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// The two machines' directory names. People-names, not letters: the transcript
/// reads as a story about two machines rather than a puzzle about which of A
/// and B is which.
const ALPHA: &str = "alpha-machine";
const BETA: &str = "beta-machine";

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

/// Run the CLI with `ARREO_IDENTITY_DIR` pointed at `dir` and `ARREO_CONFIG`
/// pointed at `config`, bounded by [`CLI_DEADLINE`]. Returns
/// (success, combined output); a timeout is a failure whose output says so.
fn cli(dir: &Path, config: &Path, cli_bin: &Path, args: &[&str]) -> (bool, String) {
    cli_with_deadline(CLI_DEADLINE, dir, config, cli_bin, args)
}

/// The same, with a caller-chosen deadline.
fn cli_with_deadline(
    deadline: Duration,
    dir: &Path,
    config: &Path,
    cli_bin: &Path,
    args: &[&str],
) -> (bool, String) {
    let mut child = Command::new(cli_bin)
        .args(args)
        .env("ARREO_IDENTITY_DIR", dir)
        .env("ARREO_CONFIG", config)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the CLI starts");
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
        Self {
            dir,
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
            .env("ARREO_IDENTITY_DIR", &self.dir);
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
        Self {
            dir,
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

    fn kill_daemon(&mut self) {
        if let Some(daemon) = self.daemon.take() {
            drop(daemon);
        }
    }
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

    let (server_bin, cli_bin, _tui_bin) = bins();
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
