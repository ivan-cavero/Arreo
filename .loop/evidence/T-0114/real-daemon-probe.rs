//! T-0114's real-daemon retest: the exported metrics read, twice, against a real
//! `arreo-server` behind a real `arreo-relay`.
//!
//! The contract test's daemon is a fixture built out of the shipped vocabulary,
//! because `arreo-server` is AGPL and the FFI crate may not link it. A fixture is
//! a model of a daemon, though, and T-0114's p1 was exactly a case where the
//! model and the daemon disagreed: the fixture used to close its session after
//! every answer, while the real one keeps it open. So the fix is retested here,
//! where nothing is modelled.
//!
//! What it drives, and through which door:
//!
//! - the relay and the machine's daemon are the product's own binaries, spawned
//!   as processes (`arreo-relay serve`, `arreo-server`), never linked;
//! - the machine's identity files are written the way `arreo pair` leaves them,
//!   and its certificate is issued through the **boundary** (`device_cert_issue`);
//! - the phone is the exported surface: `relay_session_dial`, `machines`, then
//!   `metrics_history` twice on one session, then once with a wrong-but-valid
//!   pinned key.
//!
//! The two reads are the point. A real daemon answers the first and keeps the
//! session open; a client that handshakes per call fails the second (this probe
//! prints exactly that when the cache is reverted — see the task's report).
//!
//! Run: `cargo run --release` in this directory, with the repo's
//! `cargo build -p arreo-cli -p arreo-server -p arreo-relay` done first.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arreo_core_ffi::identity::{
    device_cert_issue, device_key_from_seed, fingerprint_of_public_key, root_key_from_seed,
    DeviceCertHandle, DeviceKeyHandle, FfiRole,
};
use arreo_core_ffi::relay::{relay_peer_parse, relay_session_dial, RelaySessionHandle};

const ACCOUNT: &str = "acct-t0114";
/// The account's root key: registers the account with the relay, and issues both
/// certificates. Also this machine's *directory* root, as `arreo pair` leaves it.
const ACCOUNT_SEED: [u8; 32] = [0x11; 32];
/// The machine's device key — what its daemon authenticates with.
const MACHINE_SEED: [u8; 32] = [0x44; 32];
/// The phone: this probe's device key.
const PHONE_SEED: [u8; 32] = [0x22; 32];
/// Some other device's key, for the wrong-but-valid pin.
const STRANGER_SEED: [u8; 32] = [0x33; 32];
const MACHINE_NAME: &str = "workbox";
const PANE: &str = "pane-a";

fn main() {
    let mut world = World::new();
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    let outcome = runtime.block_on(async {
        let (device, cert, device_id) = world.phone()?;
        println!("probe: phone {device_id}");

        let session = relay_session_dial(
            world.relay_addr.to_string(),
            ACCOUNT.to_string(),
            device.clone(),
            cert,
        )
        .await
        .map_err(|e| format!("dialling the relay failed: {e}"))?;
        println!(
            "probe: session device_id={} account={}",
            session.device_id(),
            session.account()
        );

        // The two facts a phone has after a directory read — and the read is the
        // exported one, so this is the real directory, not a value made up here.
        let (peer, daemon_key) = world.await_row(&session).await?;
        println!("probe: directory row {MACHINE_NAME} peer={} key={daemon_key}", peer.device_id());

        // The pane is sampled every 10 s by the machine's metrics writer; give it
        // two intervals so the first read has a row to return.
        std::thread::sleep(Duration::from_secs(15));
        let now = now_ms();

        let first = session
            .metrics_history(
                peer.clone(),
                daemon_key.clone(),
                PANE.to_string(),
                now.saturating_sub(300_000),
                u64::MAX,
                0,
            )
            .await
            .map_err(|e| format!("the FIRST read failed: {e}"))?;
        println!(
            "probe: read #1 ok step_ms={} downshifted={} rows={}",
            first.step_ms,
            first.downshifted,
            first.rows.len()
        );

        // **The p1.** A real daemon is still holding the conversation the first
        // read opened; a client that opens a new one now fails here.
        let second = session
            .metrics_history(
                peer.clone(),
                daemon_key.clone(),
                PANE.to_string(),
                now.saturating_sub(60_000),
                u64::MAX,
                1_000,
            )
            .await
            .map_err(|e| format!("the SECOND read failed: {e}"))?;
        println!(
            "probe: read #2 ok step_ms={} downshifted={} rows={}",
            second.step_ms,
            second.downshifted,
            second.rows.len()
        );

        // A *valid* key that is not this machine's: the pin, falsified.
        let stranger = device_key_from_seed(STRANGER_SEED.to_vec())
            .map_err(|e| format!("a key from a seed: {e}"))?;
        match session
            .metrics_history(
                peer.clone(),
                stranger.public_hex(),
                PANE.to_string(),
                now.saturating_sub(60_000),
                u64::MAX,
                0,
            )
            .await
        {
            Ok(series) => Err(format!(
                "a wrong-but-valid pinned key was SERVED (rows={}): the pin is advisory",
                series.rows.len()
            )),
            Err(e) => {
                println!("probe: wrong key refused: {e}");
                Ok(())
            }
        }
    });
    world.shutdown();
    match outcome {
        Ok(()) => println!("probe: PASS — two reads on one conversation, and the wrong key refused"),
        Err(e) => {
            eprintln!("probe: FAIL — {e}");
            std::process::exit(1);
        }
    }
}

/// The world the probe drives: paths, processes, and the identities it wrote.
struct World {
    target: PathBuf,
    root: PathBuf,
    relay_addr: SocketAddr,
    children: Vec<Child>,
}

impl World {
    fn new() -> Self {
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let target = manifest
            .join("../../..")
            .canonicalize()
            .expect("the repo's target directory");
        let root = manifest.join("run");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("scratch");
        let mut world = Self {
            target,
            root,
            relay_addr: "127.0.0.1:0".parse().expect("a literal address"),
            children: Vec::new(),
        };
        world.relay_addr = world.start_relay();
        world.register_account();
        world
    }

    fn binary(&self, name: &str) -> PathBuf {
        let path = self.target.join("debug").join(name);
        assert!(
            path.exists(),
            "{} is missing — run `cargo build -p arreo-cli -p arreo-server -p arreo-relay`",
            path.display()
        );
        path
    }

    /// A running `arreo-relay`, on the address it announces.
    fn start_relay(&mut self) -> SocketAddr {
        let state = self.root.join("relay");
        fs::create_dir_all(&state).expect("relay state");
        let mut child = Command::new(self.binary("arreo-relay"))
            .args([
                "serve",
                "--listen",
                "127.0.0.1:0",
                "--state-dir",
                &state.display().to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the relay starts");
        let stderr = child.stderr.take().expect("stderr");
        let (ready_tx, ready_rx) = mpsc::channel();
        let log = state.join("relay.log");
        std::thread::spawn(move || {
            let mut file = fs::File::create(&log).expect("relay log");
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
        self.children.push(child);
        addr
    }

    fn register_account(&self) {
        // The account's *public* root key, read through the boundary: the secret
        // never leaves the seed this probe built the handle from.
        let root = root_key_from_seed(ACCOUNT_SEED.to_vec()).expect("a root from a seed");
        let out = Command::new(self.binary("arreo-relay"))
            .args([
                "account",
                "add",
                "--state-dir",
                &self.root.join("relay").display().to_string(),
                "--account",
                ACCOUNT,
                "--root-key",
                &root.public_hex(),
            ])
            .output()
            .expect("the account command runs");
        assert!(
            out.status.success(),
            "registering the account failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The machine's identity directory, written the way `arreo pair` leaves it.
    fn install_machine(&self, machine_public_hex: &str, cert: &DeviceCertHandle) {
        let identity = self.root.join("machine").join("identity");
        fs::create_dir_all(identity.join("devices")).expect("identity dir");
        write_private(&identity.join("root.key"), hex(&ACCOUNT_SEED).as_bytes());
        write_private(&identity.join("device.key"), hex(&MACHINE_SEED).as_bytes());
        let fingerprint =
            fingerprint_of_public_key(machine_public_hex.to_string()).expect("a fingerprint");
        write_private(
            &identity.join("devices").join(format!("{fingerprint}.cert")),
            &cert.encode().expect("the certificate encodes"),
        );
        fs::write(
            self.root.join("machine").join("arreo.toml"),
            format!(
                "[relay]\nenabled = true\naddr = \"{}\"\naccount = \"{ACCOUNT}\"\nname = \"{MACHINE_NAME}\"\n",
                self.relay_addr
            ),
        )
        .expect("config");
    }

    fn machine_dir(&self) -> PathBuf {
        self.root.join("machine")
    }

    /// Start the machine's daemon and wait for its socket.
    fn start_daemon(&mut self) {
        let dir = self.machine_dir();
        let socket = dir.join("arreo.sock");
        let log = fs::File::create(dir.join("daemon.log")).expect("daemon log");
        let child = Command::new(self.binary("arreo-server"))
            .arg("--socket")
            .arg(&socket)
            .arg("--config")
            .arg(dir.join("arreo.toml"))
            .env("ARREO_IDENTITY_DIR", &dir)
            .stdout(Stdio::null())
            .stderr(Stdio::from(log))
            .spawn()
            .expect("the daemon starts");
        self.children.push(child);
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the daemon never served {}", socket.display());
    }

    fn cli(&self, args: &[&str]) -> String {
        let dir = self.machine_dir();
        let out = Command::new(self.binary("arreo"))
            .args(args)
            .env("ARREO_IDENTITY_DIR", &dir)
            .env("ARREO_CONFIG", dir.join("arreo.toml"))
            .output()
            .expect("the CLI runs");
        let mut text = String::from_utf8_lossy(&out.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        assert!(
            out.status.success(),
            "`arreo {}` failed ({:?}): {text}",
            args.join(" "),
            out.status.code()
        );
        text
    }

    /// The machine's identity, its daemon, a pane, and the phone's pin.
    fn machine(&mut self, phone_public_hex: &str) -> Result<(), String> {
        let root = root_key_from_seed(ACCOUNT_SEED.to_vec())
            .map_err(|e| format!("a root from a seed: {e}"))?;
        let machine = device_key_from_seed(MACHINE_SEED.to_vec())
            .map_err(|e| format!("a key from a seed: {e}"))?;
        let cert = device_cert_issue(
            root,
            machine.public_hex(),
            MACHINE_NAME.to_string(),
            FfiRole::Owner,
            1_000,
            1,
        )
        .map_err(|e| format!("issuing the machine's certificate: {e}"))?;
        self.install_machine(&machine.public_hex(), &cert);
        self.start_daemon();
        let socket = self.machine_dir().join("arreo.sock");
        let socket = socket.display().to_string();
        self.cli(&["spawn", PANE, "/bin/sh", "-c", "sleep 600", "--socket", &socket]);
        println!("probe: machine up, pane {PANE} spawned");
        // The one step a real deployment does by pairing: the machine pins the
        // phone — as a **viewer**, which is what a phone is, and which holds the
        // `Observe` capability a metrics read needs.
        self.cli(&[
            "devices",
            "issue",
            "--socket",
            &socket,
            "--name",
            "pixel-7",
            "--role",
            "viewer",
            "--key",
            phone_public_hex,
        ]);
        println!("probe: machine pinned the phone as a viewer");
        Ok(())
    }

    fn phone(&mut self) -> Result<(std::sync::Arc<DeviceKeyHandle>, std::sync::Arc<DeviceCertHandle>, String), String> {
        let root = root_key_from_seed(ACCOUNT_SEED.to_vec())
            .map_err(|e| format!("a root from a seed: {e}"))?;
        let device = device_key_from_seed(PHONE_SEED.to_vec())
            .map_err(|e| format!("a key from a seed: {e}"))?;
        let public = device.public_hex();
        let cert = device_cert_issue(
            root,
            public.clone(),
            "pixel-7".to_string(),
            FfiRole::Viewer,
            1_000,
            2,
        )
        .map_err(|e| format!("issuing the phone's certificate: {e}"))?;
        // The machine must exist before the phone can read its directory.
        self.machine(&public)?;
        let device_id = format!(
            "dev_{}",
            fingerprint_of_public_key(public).map_err(|e| format!("a fingerprint: {e}"))?
        );
        Ok((device, cert, device_id))
    }

    /// Wait for the machine's row, read through the exported directory door.
    async fn await_row(
        &self,
        session: &RelaySessionHandle,
    ) -> Result<(std::sync::Arc<arreo_core_ffi::relay::RelayPeerHandle>, String), String> {
        let deadline = Instant::now() + Duration::from_secs(40);
        let mut last = String::new();
        while Instant::now() < deadline {
            let reply = session
                .machines(false)
                .await
                .map_err(|e| format!("the directory read failed: {e}"))?;
            if let Some(row) = reply.machines.iter().find(|row| row.name == MACHINE_NAME) {
                if let Some(key) = row.daemon_key.clone() {
                    // **The peer is the device id of the key the row publishes**,
                    // not the row's `machine_id`: the relay routes by the id the
                    // daemon *dialed* with, while `machine_id` is the machine's
                    // directory identity (its root key, T-0043 — the thing that
                    // outlives re-pairing). The core's own resolver does exactly
                    // this: `DeviceId::from_key(&server_key)`.
                    let fingerprint =
                        fingerprint_of_public_key(key.clone()).map_err(|e| format!("{e}"))?;
                    let peer = relay_peer_parse(fingerprint)
                        .map_err(|e| format!("parsing the machine's id: {e}"))?;
                    return Ok((peer, key));
                }
                last = format!("row without a daemon_key: {row:?}");
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        Err(format!(
            "no directory row for {MACHINE_NAME} within 40s; last: {last}"
        ))
    }

    fn shutdown(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Write a file the way the identity store does: owner-only, parent dir too.
///
/// The key files are 64 hex characters and the certificate is its encoded bytes;
/// both are written owner-only because `DeviceKey::load` refuses a file anyone
/// else can read.
fn write_private(path: &Path, bytes: &[u8]) {
    fs::write(path, bytes).expect("the file writes");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        if let Some(parent) = path.parent() {
            let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
