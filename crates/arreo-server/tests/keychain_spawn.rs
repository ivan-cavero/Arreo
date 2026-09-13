//! T-0087: the keychain bridge reaches the **pane**, not just the sync engine.
//!
//! T-0083 proved the mechanism — `keychain::plan(...).environment()` is the exact
//! environment a PTY spawn must apply, exercised at a real pty in the sync slice
//! — but nothing in the product *applied* it: the daemon's `Pane::spawn` took no
//! environment, so a synced `$VBK_PROD_KEY` resolved only if the variable
//! happened to be in the daemon's own environment. §3.8's promise is that the
//! machine's own store supplies the value, which is what makes a daemon started
//! by systemd (no shell, no exports) able to run a synced harness config at all.
//!
//! ## What this file proves, and how
//!
//! A real daemon, an isolated root (own `$XDG_CONFIG_HOME`, own
//! `$PI_CODING_AGENT_DIR`, own identity), a synced `models.json` that names a
//! variable, and a **fake `pi`** on the daemon's `PATH` — a three-line script
//! that writes the variable's value to a file. The child writes, the test reads:
//! no protocol plumbing between the assertion and the fact.
//!
//! The three cases are the criterion's three, and the third is the one that
//! decides the rule: with the variable **also** in the daemon's own environment,
//! the child must see the daemon's value. The injection is additive — it can
//! supply what is missing, never change what the operator set for the daemon —
//! which is what keeps this feature from altering any existing deployment.

use arreo_core::proto::{codec, Message, VERSION};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The variable the synced file references. A dummy name and a dummy value: no
/// real key is involved anywhere in this test.
const KEY: &str = "VBK_PROD_KEY";
const FROM_KEYCHAIN: &str = "value-from-the-keychain-9d21";
const FROM_DAEMON_ENV: &str = "value-from-the-daemon-env-4c07";

/// A second name the store holds and **no synced file references**. It is the
/// instrument for the leak this feature must not become: injecting "the
/// keychain" rather than "the names the files name" would hand every harness
/// pane every key this machine holds, and this name is how that shows up.
const UNREFERENCED: &str = "SOME_OTHER_PROVIDER_KEY";

/// The daemon binary, beside this test's own executable.
fn server_binary() -> PathBuf {
    std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("deps dir")
        .parent()
        .expect("debug dir")
        .join("arreo-server")
}

/// A scratch root that removes itself, even when a test fails, and **on the
/// build tree's filesystem** (`/tmp` is a tmpfs here and the store lives beside
/// the socket).
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::current_exe()
            .expect("test exe")
            .parent()
            .expect("deps dir")
            .parent()
            .expect("debug dir")
            .parent()
            .expect("target dir")
            .join("test-scratch")
            .join(format!(
                "arreo-keychain-spawn-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

/// A daemon child that is killed when the test ends, so a failing assertion
/// cannot leave a daemon (and its panes) behind.
///
/// Its stderr is drained into a buffer by a reader thread — **drained**, not
/// read at the end: a pipe nobody reads fills and blocks the daemon, which would
/// turn a log assertion into a hang. The buffer is what lets a test assert the
/// one thing a secret must never do, which is appear in a log.
struct Daemon {
    child: std::process::Child,
    stderr: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Daemon {
    /// SIGKILL and reap — the crash a restore is for.
    fn kill9(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Everything the daemon has written to stderr so far.
    fn stderr(&self) -> String {
        self.stderr
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// The isolated machine: a bin directory holding the fake harness, a config
/// root, a pi agent directory, an identity directory.
struct Machine {
    scratch: Scratch,
}

impl Machine {
    fn new(tag: &str) -> Self {
        let scratch = Scratch::new(tag);
        for dir in ["bin", "cfg", "pi-agent", "identity"] {
            std::fs::create_dir_all(scratch.path().join(dir)).expect("machine dir");
        }
        Self { scratch }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.scratch.path().join(rel)
    }

    /// The fake `pi`: a script that writes the variable's value where the test
    /// can read it. It is on the daemon's `PATH` under the harness's own name,
    /// which is what makes the adapter registry select pi's preset for it.
    fn install_fake_pi(&self, probe: &Path) {
        // The probe writes both names: the referenced one and the unreferenced
        // one, so a single spawn answers both questions.
        let script = format!(
            "#!/bin/sh\nprintf '%s|%s' \"${{{KEY}-UNSET}}\" \"${{{UNREFERENCED}-UNSET}}\" \
             > '{}'\n",
            probe.display()
        );
        let path = self.path("bin/pi");
        std::fs::write(&path, script).expect("write fake pi");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("mode");
        }
    }

    /// The synced file: pi's `models.json`, whose `apiKey` is a reference. This
    /// is what §3.8 tells the operator to write.
    fn write_synced_models(&self) {
        let path = self.path("pi-agent/models.json");
        std::fs::write(
            &path,
            format!(
                "{{\n  \"providers\": {{\n    \"verboo\": {{\n      \"baseUrl\": \
                 \"https://code.verboo.ai/router/v1\",\n      \"apiKey\": \"${KEY}\"\n    }}\n  \
                 }}\n}}\n"
            ),
        )
        .expect("write models.json");
    }

    /// This machine's keychain store — what `arreo sync secret set` writes.
    fn set_secret(&self, value: &str) {
        let path = self.path("cfg/arreo/secrets.json");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("store dir");
        std::fs::write(
            &path,
            format!("{{\"{KEY}\": \"{value}\", \"{UNREFERENCED}\": \"never-referenced\"}}\n"),
        )
        .expect("write store");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("mode");
        }
    }

    fn socket(&self) -> PathBuf {
        self.path("a.sock")
    }

    /// Start the daemon with the machine's roots and, optionally, the variable
    /// already in the daemon's own environment.
    fn start_daemon(&self, key_in_daemon_env: Option<&str>) -> Daemon {
        let socket = self.socket();
        let mut command = std::process::Command::new(server_binary());
        command
            .arg("--socket")
            .arg(&socket)
            .env("ARREO_IDENTITY_DIR", self.path("identity"))
            .env("XDG_CONFIG_HOME", self.path("cfg"))
            .env("PI_CODING_AGENT_DIR", self.path("pi-agent"))
            .env(
                "PATH",
                format!(
                    "{}:{}",
                    self.path("bin").display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            )
            // The test's own environment must not decide the result: the daemon
            // is what the keychain falls back to, so an inherited value would
            // silently satisfy the "no injection" case.
            .env_remove(KEY)
            .stdout(std::process::Stdio::null());
        if let Some(value) = key_in_daemon_env {
            command.env(KEY, value);
        }
        command.stderr(std::process::Stdio::piped());
        let mut child = command.spawn().expect("arreo-server runs");
        let stderr = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        if let Some(mut pipe) = child.stderr.take() {
            let sink = std::sync::Arc::clone(&stderr);
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = pipe.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    if let Ok(mut sink) = sink.lock() {
                        sink.push_str(&String::from_utf8_lossy(&buf[..n]));
                    }
                }
            });
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while UnixStream::connect(&socket).is_err() {
            if let Ok(Some(status)) = child.try_wait() {
                panic!("daemon exited instead of serving: {status}");
            }
            assert!(std::time::Instant::now() < deadline, "daemon never bound");
            std::thread::sleep(Duration::from_millis(50));
        }
        Daemon { child, stderr }
    }
}

/// Hello→Welcome, one request, one reply.
fn raw_request(socket: &Path, message: &Message) -> Message {
    let mut stream = UnixStream::connect(socket).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let hello = Message::Hello {
        v: VERSION,
        client: "keychain-spawn-test".to_string(),
        wants: vec![VERSION],
    };
    stream
        .write_all(&codec::encode_frame(&hello).expect("hello"))
        .expect("write");
    stream.flush().expect("flush");
    let mut acc = Vec::new();
    let mut chunk = [0u8; 8192];
    let welcome = loop {
        let n: usize = stream.read(&mut chunk).expect("read");
        assert!(n > 0, "handshake closed");
        acc.extend_from_slice(&chunk[..n]);
        if let Ok((message, consumed)) = codec::decode_frame(&acc) {
            acc.drain(..consumed);
            break message;
        }
    };
    assert!(
        matches!(welcome, Message::Welcome { .. }),
        "handshake: {welcome:?}"
    );
    stream
        .write_all(&codec::encode_frame(message).expect("encode"))
        .expect("write");
    stream.flush().expect("flush");
    loop {
        let n: usize = stream.read(&mut chunk).expect("read");
        assert!(n > 0, "reply closed");
        acc.extend_from_slice(&chunk[..n]);
        if let Ok((message, _)) = codec::decode_frame(&acc) {
            return message;
        }
    }
}

fn spawn(socket: &Path, id: &str, program: &str, args: &[&str]) -> Message {
    raw_request(
        socket,
        &Message::Spawn {
            v: VERSION,
            id: id.to_string(),
            program: program.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cols: 80,
            rows: 24,
            memory_max: None,
            pids_max: None,
            kill_on_breach: false,
        },
    )
}

/// Wait until the daemon's store holds a record for `id`, so a test that kills
/// the daemon is killing one whose pane is recoverable.
fn wait_for_record(socket: &Path, id: &str) {
    let db = arreo_server::persist::db_path_for(socket);
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if let Ok(store) = arreo_core::store::SessionStore::open(&db) {
            if let Ok(panes) = store.load_topology() {
                if panes.iter().any(|pane| pane.id == id) {
                    return;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("pane {id:?} never reached the store at {}", db.display());
}

/// Wait for `path` to hold `expected`, or panic naming what it held instead.
fn wait_for_probe(path: &Path, expected: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut seen = String::from("(no file)");
    while std::time::Instant::now() < deadline {
        if let Ok(text) = std::fs::read_to_string(path) {
            if !text.is_empty() {
                if text == expected {
                    return;
                }
                seen = text;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "the pane's child never saw {expected:?}; it wrote {seen:?} to {}",
        path.display()
    );
}

/// **The harness pane gets the keychain's value; a pane with no synced config
/// does not.**
#[test]
fn a_synced_reference_reaches_the_harness_child_and_nothing_else() {
    let machine = Machine::new("injected");
    machine.install_fake_pi(&machine.path("pi-probe.txt"));
    machine.write_synced_models();
    machine.set_secret(FROM_KEYCHAIN);
    let daemon = machine.start_daemon(None);
    let socket = machine.socket();

    // The harness pane: `pi` is the program the adapter registry claims, so the
    // pi preset's synced `models.json` is read for the names this pane needs.
    assert!(
        matches!(
            spawn(&socket, "harness", "pi", &["--mode", "json"]),
            Message::Ok { .. }
        ),
        "the harness pane spawns"
    );
    wait_for_probe(
        &machine.path("pi-probe.txt"),
        &format!("{FROM_KEYCHAIN}|UNSET"),
    );

    // The pane no preset claims: `/bin/sh` has no synced files, so it gets no
    // injection at all. This is the half that keeps the feature from being a
    // "give every child every key" leak.
    let plain_probe = machine.path("plain-probe.txt");
    let reply = spawn(
        &socket,
        "plain",
        "/bin/sh",
        &[
            "-c",
            &format!(
                "printf '%s' \"${{{KEY}:-UNSET}}\" > '{}'",
                plain_probe.display()
            ),
        ],
    );
    assert!(matches!(reply, Message::Ok { .. }), "the plain pane spawns");
    wait_for_probe(&plain_probe, "UNSET");

    drop(daemon);
}

/// **A restored pane gets it too.** The reason the bridge exists is a *resumed*
/// harness session: T-0072 brings a pi session back across a crash, and if the
/// restored child did not get the key, the resume the whole feature is for would
/// die on the provider's 401. This is the path the spawn-signature change had to
/// touch (`persist::restore`), and it is asserted the only way that means
/// anything: the daemon is SIGKILLed, a second daemon starts on the same socket
/// with the probe file deleted, and the probe must come back holding the
/// keychain's value.
#[test]
fn a_restored_pane_gets_the_keychain_value_too() {
    let machine = Machine::new("restored");
    let probe = machine.path("pi-probe.txt");
    machine.install_fake_pi(&probe);
    machine.write_synced_models();
    machine.set_secret(FROM_KEYCHAIN);
    let socket = machine.socket();

    {
        let daemon = machine.start_daemon(None);
        assert!(
            matches!(spawn(&socket, "harness", "pi", &[]), Message::Ok { .. }),
            "the harness pane spawns"
        );
        wait_for_probe(&probe, &format!("{FROM_KEYCHAIN}|UNSET"));
        // **The record has to be on disk before the kill.** The snapshot is a
        // detached task (T-0072 gated its ordering, not its latency), so killing
        // the moment the probe appears races the write and the restore would
        // have nothing to read — a flake in the test, not a bug in the product.
        wait_for_record(&socket, "harness");
        // SIGKILL, not a clean stop: the record on disk is what the restore
        // reads, which is the crash the persistence story is about.
        daemon.kill9();
    }
    std::fs::remove_file(&probe).expect("clear the probe");

    let daemon = machine.start_daemon(None);
    wait_for_probe(&probe, &format!("{FROM_KEYCHAIN}|UNSET"));
    drop(daemon);
}

/// **The daemon's own environment wins.** The injection is additive: a name the
/// daemon already carries is left exactly as the operator set it, so landing
/// this feature cannot change the behaviour of any existing deployment.
#[test]
fn a_variable_the_daemon_already_has_is_not_overridden() {
    let machine = Machine::new("env-wins");
    machine.install_fake_pi(&machine.path("pi-probe.txt"));
    machine.write_synced_models();
    machine.set_secret(FROM_KEYCHAIN);
    let daemon = machine.start_daemon(Some(FROM_DAEMON_ENV));
    let socket = machine.socket();

    assert!(
        matches!(spawn(&socket, "harness", "pi", &[]), Message::Ok { .. }),
        "the harness pane spawns"
    );
    wait_for_probe(
        &machine.path("pi-probe.txt"),
        &format!("{FROM_DAEMON_ENV}|UNSET"),
    );

    drop(daemon);
}

/// **An inherited value is respected even when it is empty.** The boundary of
/// the rule above, pinned rather than left to whichever way the code happens to
/// fall: "the daemon carries this name" is a statement about *presence*, so an
/// operator who exported an empty string gets an empty string. The alternative —
/// treating empty as unset and preferring the keychain — would silently override
/// an explicit environment, which is the property the rule exists to prevent.
#[test]
fn an_inherited_but_empty_value_is_not_replaced_by_the_keychain() {
    let machine = Machine::new("empty-env");
    machine.install_fake_pi(&machine.path("pi-probe.txt"));
    machine.write_synced_models();
    machine.set_secret(FROM_KEYCHAIN);
    let daemon = machine.start_daemon(Some(""));
    let socket = machine.socket();

    assert!(
        matches!(spawn(&socket, "harness", "pi", &[]), Message::Ok { .. }),
        "the harness pane spawns"
    );
    wait_for_probe(&machine.path("pi-probe.txt"), "|UNSET");

    drop(daemon);
}

/// **No keychain, no key, and the pane still runs.** A machine that has the
/// synced file but never set the variable must not have its spawn fail: the
/// harness is what reports the missing credential (a provider 401), and the
/// daemon's job is to say which name it could not supply.
#[test]
fn a_machine_without_the_secret_still_spawns_the_pane() {
    let machine = Machine::new("missing");
    machine.install_fake_pi(&machine.path("pi-probe.txt"));
    machine.write_synced_models();
    // Deliberately no `set_secret`.
    let daemon = machine.start_daemon(None);
    let socket = machine.socket();

    assert!(
        matches!(spawn(&socket, "harness", "pi", &[]), Message::Ok { .. }),
        "a missing secret is not a spawn failure"
    );
    wait_for_probe(&machine.path("pi-probe.txt"), "UNSET|UNSET");

    // The machine that lacks the secret is the case that logs, so it is the case
    // that can leak: the notice names the variable and never its value, and the
    // store's other key is not named either (it is not what the file needs).
    let log = daemon.stderr();
    assert!(
        log.contains(KEY),
        "the missing name is reported, so the check below is not vacuous: {log}"
    );
    assert!(
        !log.contains("never-referenced"),
        "no value reaches the daemon's log: {log}"
    );
    assert!(
        !log.contains(UNREFERENCED),
        "and an unreferenced name is not even mentioned: {log}"
    );

    drop(daemon);
}
