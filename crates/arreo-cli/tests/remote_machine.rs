//! T-0045 acceptance tests: `arreo attach --machine <name>` reaches another
//! machine's pane, with no address anywhere in the command.
//!
//! Three real processes: a self-hosted `arreo-relay`, machine A's daemon (the
//! machine being reached), and the CLI running as a **different** machine, C —
//! with its own root key, its own device identity, and no configuration naming
//! anyone. C types a *name*; the account's directory resolves it to the key A's
//! daemon answers on; the session that follows is the same client a local attach
//! uses (T-0015), which is the "one code path, both roles" claim made concrete.
//!
//! The identity plumbing mirrors `crates/arreo-server/tests/relay_daemon.rs`,
//! which is the known-good two-daemon setup: keys and certificates written
//! directly, pins made through the product's own `devices issue`.
//!
//! **Run with `cargo test --workspace`** (or build the binaries first):
//! `cargo test -p arreo-cli` does not build `arreo-server`/`arreo-relay`.

use arreo_core::identity::{DeviceCert, DeviceKey, Role, RootKey, VerifyingKey};
use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

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

/// A running relay.
struct Relay {
    child: Child,
    addr: SocketAddr,
    state_dir: PathBuf,
    /// Set when a test intends to start another relay on the same state dir: the
    /// directory is the account's durable memory, and deleting it on drop would
    /// make "restart and look again" impossible.
    keep_state: bool,
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if !self.keep_state {
            let _ = std::fs::remove_dir_all(&self.state_dir);
        }
    }
}

impl Relay {
    fn start(tag: &str) -> Self {
        Self::start_with(tag, None, None)
    }

    /// Restart on an existing state dir with the clock moved far forward, so a row
    /// written earlier reads as stale. The seam is T-0055's.
    fn start_ahead(tag: &str, state_dir: PathBuf, addr: SocketAddr) -> Self {
        Self::start_with(tag, Some(state_dir), Some(addr))
    }

    fn start_with(tag: &str, reuse: Option<PathBuf>, addr: Option<SocketAddr>) -> Self {
        let ahead = reuse.is_some();
        let state_dir = reuse.unwrap_or_else(|| {
            let dir = std::env::temp_dir().join(format!(
                "arreo-remote-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            dir
        });
        std::fs::create_dir_all(&state_dir).expect("scratch");
        let listen = addr.map_or("127.0.0.1:0".to_string(), |addr| addr.to_string());
        let mut command = Command::new(binary("arreo-relay"));
        command.args([
            "serve",
            "--listen",
            &listen,
            "--state-dir",
            &state_dir.display().to_string(),
        ]);
        if ahead {
            // 31 days past everything written before: rows the first relay wrote
            // now read as machines nobody has seen for a month, without a test
            // that waits a month (the seam is T-0055's).
            command.env(
                "ARREO_CLOCK_OFFSET_MS",
                (31 * 24 * 60 * 60 * 1000u64).to_string(),
            );
        }
        let mut child = command
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the relay starts");
        let stderr = child.stderr.take().expect("stderr");
        let (ready_tx, ready_rx) = mpsc::channel();
        let log_path = state_dir.join("relay.log");
        std::thread::spawn(move || {
            let mut file = std::fs::File::create(&log_path).expect("relay log");
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = writeln!(file, "{line}");
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
            keep_state: false,
        }
    }

    /// Stop this relay, keeping its state dir for a later start.
    fn stop_keeping_state(mut self) -> (PathBuf, SocketAddr) {
        self.keep_state = true;
        let _ = self.child.kill();
        let _ = self.child.wait();
        (self.state_dir.clone(), self.addr)
    }

    /// What the relay saw: who authenticated, and what it did with each envelope.
    fn log(&self) -> String {
        std::fs::read_to_string(self.state_dir.join("relay.log")).unwrap_or_default()
    }

    fn register_account(&self, account: &str, root: &VerifyingKey) {
        let key: String = root.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        let out = Command::new(binary("arreo-relay"))
            .args([
                "account",
                "add",
                "--state-dir",
                &self.state_dir.display().to_string(),
                "--account",
                account,
                "--root-key",
                &key,
            ])
            .output()
            .expect("the account command runs");
        assert!(
            out.status.success(),
            "registering the account failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A machine: an identity directory, a socket to its daemon, and a config.
struct Machine {
    dir: PathBuf,
    socket: PathBuf,
    config: PathBuf,
    child: Option<Child>,
    /// Set when the daemon's stderr is captured, for a failure message worth
    /// reading.
    log: Option<PathBuf>,
}

impl Drop for Machine {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        // A machine that never started a daemon has no state to keep.
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Machine {
    fn new(tag: &str, relay: &Relay, account: &str, name: Option<&str>) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "arreo-remote-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("identity").join("devices")).expect("scratch");
        let config = dir.join("arreo.toml");
        let name = name.map_or(String::new(), |name| format!("name = \"{name}\"\n"));
        std::fs::write(
            &config,
            format!(
                "[relay]\nenabled = true\naddr = \"{}\"\naccount = \"{account}\"\n{name}",
                relay.addr
            ),
        )
        .expect("config");
        let socket = dir.join("arreo.sock");
        Self {
            dir,
            socket,
            config,
            child: None,
            log: None,
        }
    }

    fn identity(&self) -> PathBuf {
        self.dir.join("identity")
    }

    /// This machine's identity, written the way `arreo pair` leaves it: a root
    /// key (its directory identity), a device key, and the certificate the
    /// account issued for that device.
    fn install_identity(&self, root: &RootKey, device: &DeviceKey, cert: &DeviceCert) {
        root.save(&self.identity().join("root.key")).expect("root");
        device
            .save(&self.identity().join("device.key"))
            .expect("device key");
        let path = self
            .identity()
            .join("devices")
            .join(format!("{}.cert", cert.device().as_str()));
        std::fs::write(&path, cert.encode().expect("encode")).expect("certificate");
    }

    /// Run the CLI as this machine.
    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(binary("arreo"))
            .args(args)
            .env("ARREO_IDENTITY_DIR", &self.dir)
            .env("ARREO_CONFIG", &self.config)
            .output()
            .expect("the CLI runs");
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        (out.status.code().unwrap_or(-1), text)
    }

    /// Run the CLI against this machine's own daemon (its socket).
    fn run_local(&self, args: &[&str]) -> (i32, String) {
        let mut all: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        all.push("--socket".to_string());
        all.push(self.socket.display().to_string());
        let refs: Vec<&str> = all.iter().map(String::as_str).collect();
        self.run(&refs)
    }

    /// Start this machine's daemon and wait for its socket.
    fn start_daemon(&mut self) {
        let mut child = Command::new(binary("arreo-server"))
            .arg("--socket")
            .arg(&self.socket)
            .arg("--config")
            .arg(&self.config)
            .env("ARREO_IDENTITY_DIR", &self.dir)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the daemon starts");
        let stderr = child.stderr.take().expect("stderr");
        let log = self.dir.join("daemon.log");
        let sink = log.clone();
        std::thread::spawn(move || {
            let mut file = std::fs::File::create(sink).expect("log file");
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = writeln!(file, "{line}");
            }
        });
        self.log = Some(log);
        self.child = Some(child);
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&self.socket).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the daemon never served {}", self.socket.display());
    }

    fn daemon_log(&self) -> String {
        self.log
            .as_ref()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default()
    }
}

/// Read one machine's published dial key, as this machine's own device.
async fn publish_row(
    machine: &Machine,
    device: &DeviceKey,
    cert: &DeviceCert,
    name: &str,
) -> Option<String> {
    let settings = arreo_core::relay::config::load_config(&machine.config)
        .expect("config reads")
        .expect("relay enabled");
    let session = arreo_core::relay::session::RelaySession::dial(
        settings.addr,
        &settings.account,
        device,
        cert,
    )
    .await
    .expect("the observer registers");
    let reply = session.machines(true).await.expect("the relay answers");
    reply
        .machines
        .iter()
        .find(|row| row.name.as_str() == name)
        .and_then(|row| row.daemon_key.clone())
}

/// Wait until a machine's daemon has asserted its directory row, read from its
/// own log.
///
/// Deliberately not "poll the directory as that machine": a CLI session
/// authenticates with the daemon's own device key, and the relay keeps **one live
/// session per device** — so polling as A displaces A's daemon from the routing
/// table and the machine becomes unreachable by name. That is a real footgun for
/// operators (a `machines list` on a daemon-hosting box would do it), it is
/// recorded in the task file, and tests must not trip it.
fn await_registration(machine: &Machine) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if machine.daemon_log().contains("this machine is") {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "the daemon never asserted its directory row; its log was:\n{}",
        machine.daemon_log()
    );
}

/// Wait for a directory row, read by `observer` (which must not be the machine
/// being watched — see [`await_registration`]).
fn await_row(observer: &Machine, name: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last = String::new();
    while Instant::now() < deadline {
        let (code, out) = observer.run(&["machines", "list", "--json", "--all"]);
        if code == 0 {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(out.trim()) {
                if let Some(row) = value["machines"].as_array().and_then(|rows| {
                    rows.iter()
                        .find(|row| row["name"] == serde_json::json!(name))
                }) {
                    return row.clone();
                }
            }
        }
        last = out;
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("no directory row for {name:?} within 30s; last output:\n{last}");
}

/// The whole scenario: one command, a name, no address.
///
/// A is reached; C is a machine A has never heard of apart from the one thing
/// that matters — C's device was issued by A, which is what makes A trust it.
#[test]
fn attach_by_name_reaches_another_machines_pane() {
    let relay = Relay::start("byname");
    let account_root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &account_root.public());

    // ---- A: the machine being reached.
    let mut a = Machine::new("a", &relay, "acct-1", Some("workbox"));
    let a_device = DeviceKey::generate().expect("entropy");
    let a_cert = DeviceCert::issue(
        &account_root,
        &a_device.public(),
        "workbox",
        Role::Owner,
        1_000,
        1,
    );
    a.install_identity(&account_root, &a_device, &a_cert);
    a.start_daemon();
    await_registration(&a);
    // A pane to reach, on A.
    // The pane prints and ends. Attaching streams until the pane exits (the same
    // semantics locally and remotely), so a pane that slept would make this test
    // measure the sleep rather than the attach.
    let (code, out) = a.run_local(&[
        "spawn",
        "pane-a",
        "/bin/sh",
        "-c",
        "echo HELLO-FROM-WORKBOX",
    ]);
    assert_eq!(code, 0, "spawning on A failed: {out}");

    // ---- C: a different machine, in the same account.
    let c = Machine::new("c", &relay, "acct-1", None);
    let c_device = DeviceKey::generate().expect("entropy");
    let c_cert = DeviceCert::issue(
        &account_root,
        &c_device.public(),
        "laptop",
        Role::Owner,
        1_000,
        7,
    );
    c.install_identity(&RootKey::generate().expect("entropy"), &c_device, &c_cert);
    // A issues C's device — the one step a real deployment does by pairing, and
    // the step that both pins C on A and records A's grant to it (T-0046).
    let issued = Command::new(binary("arreo"))
        .args([
            "devices",
            "issue",
            "--socket",
            &a.socket.display().to_string(),
            "--name",
            "laptop",
            "--role",
            "operator",
            "--key",
            &c_device.public_hex(),
        ])
        .env("ARREO_IDENTITY_DIR", &a.dir)
        .output()
        .expect("the issue command runs");
    assert!(
        issued.status.success(),
        "A must be able to pin C: {}",
        String::from_utf8_lossy(&issued.stderr)
    );

    // C can see A — by name, with presence, and reachable.
    let row = await_row(&c, "workbox");
    assert_eq!(row["presence"], serde_json::json!("online"), "{row}");
    assert_eq!(row["reachable"], serde_json::json!(true), "{row}");

    // What identity did the daemon actually read? Printed because the answer to
    // "which key does this process use" is the question this test is about.
    // The row must carry exactly the key A's daemon answers on — the whole point
    // of the field. Read through the same door the CLI uses (a relay session as
    // C), so this asserts the directory's answer, not a value the test made up.
    {
        let published = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(publish_row(&c, &c_device, &c_cert, "workbox"));
        assert_eq!(
            published.as_deref(),
            Some(a_device.public_hex().as_str()),
            "the directory must publish the key A's daemon authenticates with"
        );
    }
    // **The scenario.** One command, a name, no address anywhere.
    let (code, out) = c.run(&["attach", "--machine", "workbox", "pane-a"]);
    assert!(
        out.contains("HELLO-FROM-WORKBOX"),
        "attaching by name must reach A's pane (exit {code}): {out}\nA's daemon said:\n{}\nrelay said:\n{}",
        a.daemon_log(),
        relay.log()
    );
}

/// The same machine is reachable by the daemon role too — "server-as-client"
/// (criterion 2). A's daemon attaches to C's daemon over the relay, using the
/// same client the CLI just used.
///
/// This is the claim that matters for §3.7: the VPS reaching the Pi is not a
/// special mode, it is one machine's daemon running the client it already has.
#[test]
fn a_daemon_reaches_another_machines_pane_through_the_same_client() {
    let relay = Relay::start("asclient");
    let account_root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &account_root.public());

    // B serves a pane.
    let mut b = Machine::new("asmc-b", &relay, "acct-1", Some("workbox"));
    let b_device = DeviceKey::generate().expect("entropy");
    let b_cert = DeviceCert::issue(
        &account_root,
        &b_device.public(),
        "workbox",
        Role::Owner,
        1_000,
        1,
    );
    // Its **own** root key: a machine's directory identity is its root (T-0043), and
    // two machines sharing one would be one machine with two names — which the
    // directory refuses, correctly.
    b.install_identity(&RootKey::generate().expect("entropy"), &b_device, &b_cert);
    b.start_daemon();
    await_registration(&b);
    let (code, out) = b.run_local(&[
        "spawn",
        "pane-b",
        "/bin/sh",
        "-c",
        "echo SERVED-BY-WORKBOX; sleep 60",
    ]);
    assert_eq!(code, 0, "{out}");

    // A is a second machine whose *daemon* is configured to probe B: the config's
    // `peer` is the device A's daemon opens a session to, which is the
    // server-as-client path that T-0051 built.
    let mut a = Machine::new("asmc-a", &relay, "acct-1", Some("vps"));
    let a_device = DeviceKey::generate().expect("entropy");
    let a_cert = DeviceCert::issue(
        &account_root,
        &a_device.public(),
        "vps",
        Role::Owner,
        1_000,
        2,
    );
    a.install_identity(&account_root, &a_device, &a_cert);
    // A holds the account root (it is the account's owner machine).
    // A pins B, and B pins A — the pairing a real deployment does once.
    for (machine, name, key) in [(&a, "workbox", &b_device), (&b, "vps", &a_device)] {
        let out = Command::new(binary("arreo"))
            .args([
                "devices",
                "issue",
                "--socket",
                &machine.socket.display().to_string(),
                "--name",
                name,
                "--role",
                "operator",
                "--key",
                &key.public_hex(),
            ])
            .env("ARREO_IDENTITY_DIR", &machine.dir)
            .output()
            .expect("the issue command runs");
        assert!(
            out.status.success(),
            "pinning {name} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // A's daemon probes B at boot (the `peer` key of its config).
    std::fs::write(
        &a.config,
        format!(
            "[relay]\nenabled = true\naddr = \"{}\"\naccount = \"acct-1\"\nname = \"vps\"\npeer = \"{}\"\n",
            relay.addr,
            b_cert.device().display_id()
        ),
    )
    .expect("config with a peer");
    a.start_daemon();

    // A's probe opens a real session to B and asks for its panes. A's own log
    // saying how many panes B reported *is* the exchange — the client path, run by
    // a daemon rather than by a CLI.
    //
    // The answer's count, not the pane's id: the daemon deliberately does not
    // print what a peer serves (T-0051's opacity rule), so the count is the
    // strongest honest evidence here, and B has exactly one pane.
    let deadline = Instant::now() + Duration::from_secs(40);
    let mut seen = String::new();
    while Instant::now() < deadline {
        let log = a.daemon_log();
        if log.contains("reports 1 pane") {
            seen = log;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        seen.contains("reports 1 pane"),
        "A's daemon must reach B's pane through the relay with the same client the CLI \
         uses.\nA's log:\n{}\nB's log:\n{}",
        a.daemon_log(),
        b.daemon_log()
    );
    // And B saw the session as a real peer: the same `serve_peer` path a CLI
    // attach takes.
    assert!(
        b.daemon_log().contains("relay peer") && b.daemon_log().contains("authenticated"),
        "B must have served A as a peer:\n{}",
        b.daemon_log()
    );
}

/// An unknown name is exit 3 and lists what the account does have: no dial, no
/// hang, and no address from argv.
#[test]
fn an_unknown_machine_is_refused_by_name() {
    let relay = Relay::start("unknown");
    let account_root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &account_root.public());

    let mut a = Machine::new("unknown-a", &relay, "acct-1", Some("workbox"));
    let a_device = DeviceKey::generate().expect("entropy");
    let a_cert = DeviceCert::issue(
        &account_root,
        &a_device.public(),
        "workbox",
        Role::Owner,
        1_000,
        1,
    );
    a.install_identity(&account_root, &a_device, &a_cert);
    a.start_daemon();
    await_registration(&a);

    let c = Machine::new("unknown-c", &relay, "acct-1", None);
    let c_device = DeviceKey::generate().expect("entropy");
    let c_cert = DeviceCert::issue(
        &account_root,
        &c_device.public(),
        "laptop",
        Role::Owner,
        1_000,
        8,
    );
    c.install_identity(&RootKey::generate().expect("entropy"), &c_device, &c_cert);

    let (code, out) = c.run(&["attach", "--machine", "nosuch"]);
    assert_eq!(code, 3, "an unknown machine is exit 3: {out}");
    assert!(
        out.contains("workbox"),
        "and says what the account does list: {out}"
    );
    // Nothing was dialed: the message names the directory, not a connection
    // attempt. That is the difference between "no such machine" and "it did not
    // answer", and the exit codes keep them apart.
    assert!(
        !out.contains("timed out"),
        "an unknown name must not be a dial timeout: {out}"
    );
}

/// A machine the directory calls offline is exit 4 **before** any dial, and the
/// message carries presence and age — T-0045's "fast and honest".
///
/// The staleness comes from the relay's clock seam (`ARREO_CLOCK_OFFSET_MS`,
/// T-0055) rather than from waiting: presence is a function of elapsed time, so a
/// relay restarted 31 days ahead sees the same row the way a real operator would
/// after a month away — and the test stays fast. The alternative, sleeping past
/// the 90-second online window, would test the same code path 90 seconds slower.
#[test]
fn an_offline_machine_is_refused_quickly_with_its_presence() {
    let relay = Relay::start("offline");
    let account_root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &account_root.public());

    let mut a = Machine::new("offline-a", &relay, "acct-1", Some("workbox"));
    let a_device = DeviceKey::generate().expect("entropy");
    let a_cert = DeviceCert::issue(
        &account_root,
        &a_device.public(),
        "workbox",
        Role::Owner,
        1_000,
        1,
    );
    a.install_identity(&account_root, &a_device, &a_cert);
    a.start_daemon();
    await_registration(&a);

    let c = Machine::new("offline-c", &relay, "acct-1", None);
    let c_device = DeviceKey::generate().expect("entropy");
    let c_cert = DeviceCert::issue(
        &account_root,
        &c_device.public(),
        "laptop",
        Role::Owner,
        1_000,
        8,
    );
    c.install_identity(&RootKey::generate().expect("entropy"), &c_device, &c_cert);

    // The machine goes away, and time passes: A's row is in the directory but
    // well past the window that means "recently seen".
    a.child.as_mut().expect("child").kill().expect("kill");
    a.child.as_mut().expect("child").wait().expect("wait");
    let (state_dir, addr) = relay.stop_keeping_state();
    let stale = Relay::start_ahead("offline-again", state_dir, addr);

    let started = Instant::now();
    let (code, out) = c.run(&["attach", "--machine", "workbox", "pane-a"]);
    let elapsed = started.elapsed();
    assert_eq!(
        code, 4,
        "a machine that is not there is a reachability failure: {out}"
    );
    assert!(
        out.contains("stale") || out.contains("offline"),
        "the message carries what the directory says: {out}"
    );
    assert!(
        out.contains("last seen ") && out.contains('T') && out.contains('Z'),
        "and when it was last seen, from the relay's own clock: {out}"
    );
    assert!(
        !out.contains("no answer"),
        "and nothing was dialed, because the directory already knew: {out}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "and it is fast — no dial, no handshake budget spent ({elapsed:?}): {out}"
    );
    drop(stale);
}
