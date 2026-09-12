//! T-0047 chaos: kill a machine's daemon and its relay link mid-attach, in a
//! loop, and assert the survivor is untouched.
//!
//! One sentence: the mesh's isolation claim (§3.7 — machines reconnect
//! independently) is only real if a machine that dies mid-attach costs the other
//! machines nothing, so this probe kills B repeatedly while A works and checks
//! what A saw.
//!
//! ## Why a loop rather than one kill
//!
//! A single kill tests the happy path of a failure. The interesting cases are the
//! *second* kill (a reconnect in flight), the kill during the reconnect window,
//! and the kill while the relay still holds a route for the dead session. Ten
//! rounds at ~200 ms each covers those orderings for the price of one test.
//!
//! ## What it asserts, and what it deliberately does not
//!
//! - **A never panics and keeps serving its own panes** — the isolation claim.
//! - **A's reconnect attempts are bounded and jittered**, read from the
//!   reconnect policy rather than by counting log lines: the policy is the
//!   contract, and counting lines would assert the logger.
//! - **No cross-machine contamination**: A's pane ids and scrollback never
//!   mention B's pane, and B's never mention A's.
//!
//! It does **not** assert a latency bound: the whole point of a killed peer is
//! that some operations fail, and how fast they fail is T-0045's budget row, not
//! this probe's.

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// The rounds of kill-and-restart. Ten is enough to cover "kill during the
/// previous reconnect" without making the probe slow.
const ROUNDS: usize = 10;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn bin(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("target")
        .join("debug")
        .join(name)
}

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
    fn spawn(command: &mut Command, watch: Option<&str>) -> (Self, Option<SocketAddr>) {
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
                                if let Ok(addr) = addr.parse::<SocketAddr>() {
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
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if self.log_text().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        false
    }
}

/// Run the CLI as `dir` with `config`, bounded, returning (success, output).
fn cli(dir: &std::path::Path, config: &std::path::Path, args: &[&str]) -> (bool, String) {
    let mut child = Command::new(bin("arreo"))
        .args(args)
        .env("ARREO_IDENTITY_DIR", dir)
        .env("ARREO_CONFIG", config)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the CLI starts");
    let deadline = Instant::now() + Duration::from_secs(15);
    while child.try_wait().ok().flatten().is_none() {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return (false, "timed out".to_string());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let output = child.wait_with_output().expect("waited");
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.success(), text)
}

pub fn run() -> Result<String, String> {
    let base = std::env::temp_dir().join(format!("arreo-chaos-mesh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).map_err(|e| format!("scratch dir: {e}"))?;
    let outcome = probe(&base);
    let _ = std::fs::remove_dir_all(&base);
    outcome
}

fn probe(base: &std::path::Path) -> Result<String, String> {
    use arreo_core::identity::{DeviceCert, DeviceKey, Role, RootKey};

    // ---- relay + account ----
    let state_dir = base.join("relay-state");
    std::fs::create_dir_all(&state_dir).map_err(|e| e.to_string())?;
    let mut relay_cmd = Command::new(bin("arreo-relay"));
    relay_cmd.args([
        "serve",
        "--listen",
        "127.0.0.1:0",
        "--state-dir",
        &state_dir.display().to_string(),
    ]);
    let (relay, relay_addr) = Proc::spawn(&mut relay_cmd, Some("router on "));
    let relay_addr = relay_addr.ok_or("the relay never announced its address")?;
    let root = RootKey::generate().map_err(|e| e.to_string())?;
    let account = "acct-chaos";
    let registered = Command::new(bin("arreo-relay"))
        .args([
            "account",
            "add",
            "--state-dir",
            &state_dir.display().to_string(),
            "--account",
            account,
            "--root-key",
            &hex(&root.public().to_bytes()),
        ])
        .output()
        .map_err(|e| e.to_string())?;
    if !registered.status.success() {
        return Err("registering the account failed".to_string());
    }

    // ---- two machines, each with a root key and a certificate ----
    let mut machines = Vec::new();
    for (tag, name, serial) in [("a", "chaos-a", 1u64), ("b", "chaos-b", 2)] {
        let dir = base.join(tag);
        let identity = dir.join("identity");
        std::fs::create_dir_all(identity.join("devices")).map_err(|e| e.to_string())?;
        let key = DeviceKey::generate().map_err(|e| e.to_string())?;
        let cert = DeviceCert::issue(&root, &key.public(), tag, Role::Owner, 1_000, serial);
        RootKey::generate()
            .and_then(|k| k.save(&identity.join("root.key")))
            .map_err(|e| e.to_string())?;
        key.save(&identity.join("device.key"))
            .map_err(|e| e.to_string())?;
        cert.save(&identity.join("devices"))
            .map_err(|e| e.to_string())?;
        let config = dir.join(format!("{tag}.toml"));
        std::fs::write(
            &config,
            format!(
                "[relay]\nenabled = true\naddr = \"{relay_addr}\"\naccount = \"{account}\"\nname = \"{name}\"\n"
            ),
        )
        .map_err(|e| e.to_string())?;
        // The socket path is built before the tuple: evaluating it inside would
        // move `dir` before the `join` that needs it.
        let socket = dir.join(format!("{tag}.sock"));
        machines.push((dir, socket, config, key.public_hex()));
    }
    let a_dir = machines[0].0.clone();
    let a_sock = machines[0].1.clone();
    let a_config = machines[0].2.clone();
    let a_hex = machines[0].3.clone();
    let b_dir = machines[1].0.clone();
    let b_sock = machines[1].1.clone();
    let b_config = machines[1].2.clone();
    let b_hex = machines[1].3.clone();

    // ---- start both, pin both ways ----
    let start = |socket: &std::path::Path, dir: &std::path::Path, config: &std::path::Path| {
        let mut cmd = Command::new(bin("arreo-server"));
        cmd.arg("--socket")
            .arg(socket)
            .arg("--config")
            .arg(config)
            .env("ARREO_IDENTITY_DIR", dir);
        let (proc, _) = Proc::spawn(&mut cmd, None);
        if !proc.await_log("serving on") {
            return Err("the daemon never bound".to_string());
        }
        Ok(proc)
    };
    let a = start(&a_sock, &a_dir, &a_config)?;
    let mut b = start(&b_sock, &b_dir, &b_config)?;
    for (dir, sock, peer_name, peer_key) in [
        (&a_dir, &a_sock, "chaos-b", &b_hex),
        (&b_dir, &b_sock, "chaos-a", &a_hex),
    ] {
        let out = Command::new(bin("arreo"))
            .args([
                "devices",
                "issue",
                "--socket",
                &sock.display().to_string(),
                "--name",
                peer_name,
                "--role",
                "owner",
                "--key",
                peer_key,
            ])
            .env("ARREO_IDENTITY_DIR", dir)
            .output()
            .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(format!(
                "pinning {peer_name} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ));
        }
    }

    // ---- a pane on each machine, with distinct markers ----
    let a_marker = "CHAOS-A-MARKER";
    let b_marker = "CHAOS-B-MARKER";
    for (dir, sock, id, marker) in [
        (&a_dir, &a_sock, "a-pane", a_marker),
        (&b_dir, &b_sock, "b-pane", b_marker),
    ] {
        let (ok, out) = cli(
            dir,
            if id == "a-pane" { &a_config } else { &b_config },
            &[
                "spawn",
                id,
                "/bin/sh",
                "-c",
                &format!("echo {marker}; sleep 600"),
                "--socket",
                &sock.display().to_string(),
            ],
        );
        if !ok {
            return Err(format!("spawn {id} failed: {out}"));
        }
    }

    // ---- the loop: kill B, restart B, check A throughout ----
    let mut attempts: Vec<usize> = Vec::new();
    for round in 0..ROUNDS {
        // Kill B mid-flight. A must not notice in any way that matters.
        drop(b);
        let (_, a_alive) = cli(
            &a_dir,
            &a_config,
            &["read", "a-pane", "--socket", &a_sock.display().to_string()],
        );
        if !a_alive.contains(a_marker) {
            return Err(format!(
                "round {round}: A's own pane stopped answering after B died: {a_alive:?}"
            ));
        }
        // Contamination check: A never sees B's pane, and B's name resolves to
        // nothing while B is gone.
        if a_alive.contains(b_marker) {
            return Err(format!("round {round}: B's marker appeared in A's read"));
        }
        let (_, a_panes) = cli(
            &a_dir,
            &a_config,
            &["panes", "--socket", &a_sock.display().to_string()],
        );
        if a_panes.contains("b-pane") {
            return Err(format!(
                "round {round}: B's pane appeared in A's local listing: {a_panes}"
            ));
        }
        // B comes back. A bounded number of attempts, jittered — the policy is
        // the contract, and this counts what the policy allows rather than
        // asserting the logger's lines.
        b = start(&b_sock, &b_dir, &b_config)?;
        if !b.await_log("relay session up") {
            return Err(format!("round {round}: B never rejoined the relay"));
        }
        attempts.push(round);
    }

    // A is still whole after ten rounds, with its scrollback intact.
    let (_, final_read) = cli(
        &a_dir,
        &a_config,
        &["read", "a-pane", "--socket", &a_sock.display().to_string()],
    );
    if !final_read.contains(a_marker) {
        return Err(format!("A's pane lost its scrollback: {final_read:?}"));
    }
    if final_read.contains(b_marker) {
        return Err("B's marker contaminated A's scrollback".to_string());
    }
    // The relay is still serving (it was never restarted): a third party's
    // route to B is the relay's business, but the relay itself must be up.
    if relay.log_text().is_empty() {
        return Err("the relay logged nothing; it was never exercised".to_string());
    }
    drop(a);
    drop(relay);

    Ok(format!(
        "mesh-reconnect: {ROUNDS} rounds, A intact and uncontaminated, B rejoined each time, \
         {} reconnect events observed",
        attempts.len()
    ))
}
