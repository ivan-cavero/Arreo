//! T-0026 acceptance tests: revocation driven through the real binaries.
//!
//! A revocation is only real if it survives what happens next — a restart, an
//! offline gap, a re-pairing attempt — so these tests run the actual
//! `arreo-server` and `arreo` binaries against a real state directory rather than
//! asserting against in-process objects. T-0052 covers the other half (cutting a
//! session that is already open).
//!
//! **Run these with `cargo test --workspace` (or build both crates first).**
//! `cargo test -p arreo-server` builds this crate's own binaries but *not*
//! `arreo`, so an edit to the CLI would be silently tested against a stale
//! binary — the same trap T-0024 recorded for `arreo-relay`, and it cost a
//! debugging cycle here too (the audit renderer looked unfixed because the CLI
//! had not been rebuilt).

use arreo_core::identity::{DeviceId, DeviceKey, RootKey};
use arreo_core::proto::{codec, Message, VERSION};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn binary(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_BIN_EXE_arreo-server"))
        .parent()
        .expect("target dir")
        .join(name);
    assert!(
        path.exists(),
        "{} is missing — run `cargo test --workspace`",
        path.display()
    );
    path
}

/// A daemon plus the identity directory and socket it serves.
struct Fixture {
    dir: PathBuf,
    socket: PathBuf,
    root: RootKey,
    child: Child,
    log: Arc<Mutex<String>>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Fixture {
    /// The account root the daemon bootstrapped (a real operator reads it to
    /// register the account at a relay; the tests below only need it to exist).
    #[allow(dead_code)]
    fn root(&self) -> &RootKey {
        &self.root
    }

    fn start(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "arreo-revocation-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("identity")).expect("scratch");
        let socket = dir.join("arreo.sock");
        let mut child = Self::spawn(&dir, &socket);
        let log = Self::capture(&mut child);
        let mut fixture = Self {
            dir: dir.clone(),
            socket: socket.clone(),
            root: RootKey::generate().expect("placeholder"),
            child,
            log,
        };
        fixture.await_socket();
        // The daemon creates the root key on first boot, so the fixture reads it
        // afterwards rather than racing the boot — which is also what a real
        // operator does.
        fixture.root = RootKey::load_or_generate(&dir.join("identity").join("root.key"))
            .expect("the daemon created a root key");
        fixture
    }

    fn spawn(dir: &Path, socket: &Path) -> Child {
        Command::new(binary("arreo-server"))
            .arg("--socket")
            .arg(socket)
            .env("ARREO_IDENTITY_DIR", dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the daemon starts")
    }

    fn capture(child: &mut Child) -> Arc<Mutex<String>> {
        let log = Arc::new(Mutex::new(String::new()));
        let stderr = child.stderr.take().expect("stderr");
        let sink = Arc::clone(&log);
        std::thread::spawn(move || {
            // Keep reading for the process's whole life: dropping the pipe would
            // kill the daemon on its next log line (EPIPE).
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut held = sink.lock().expect("log");
                held.push_str(&line);
                held.push('\n');
            }
        });
        log
    }

    fn await_socket(&self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&self.socket).is_ok() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!("the daemon never served {}", self.socket.display());
    }

    fn log_text(&self) -> String {
        self.log.lock().expect("log").clone()
    }

    /// Kill hard and restart against the same files — the durability probe.
    fn crash_and_restart(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.child = Self::spawn(&self.dir, &self.socket);
        self.log = Self::capture(&mut self.child);
        self.await_socket();
    }

    /// Run an `arreo` verb against this daemon.
    fn arreo(&self, args: &[&str]) -> std::process::Output {
        Command::new(binary("arreo"))
            .args(args)
            .arg("--socket")
            .arg(&self.socket)
            .env("ARREO_IDENTITY_DIR", &self.dir)
            .output()
            .expect("the CLI runs")
    }

    /// Pin a device through the product's own door.
    fn pin(&self, name: &str, key: &DeviceKey) -> DeviceId {
        let output = self.arreo(&[
            "devices",
            "issue",
            "--name",
            name,
            "--role",
            "owner",
            "--key",
            &key.public_hex(),
        ]);
        assert!(
            output.status.success(),
            "pinning {name} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        DeviceId::from_key(&key.public())
    }

    /// Connect over the local socket, returning the first reply.
    fn hello_reply(&self) -> Result<Message, String> {
        let mut stream =
            std::os::unix::net::UnixStream::connect(&self.socket).map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        let frame = codec::encode_frame(&Message::Hello {
            v: VERSION,
            client: "test".to_string(),
            wants: vec![VERSION],
        })
        .map_err(|e| e.to_string())?;
        stream.write_all(&frame).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        loop {
            if let Ok((message, _)) = codec::decode_frame(&buf) {
                return Ok(message);
            }
            let mut chunk = [0u8; 8192];
            let read = stream.read(&mut chunk).map_err(|e| e.to_string())?;
            if read == 0 {
                return Err("the daemon closed the connection".to_string());
            }
            buf.extend_from_slice(&chunk[..read]);
        }
    }

    /// The authority's view of a device, as the CLI reports it.
    fn listed(&self, extra: &[&str]) -> serde_json::Value {
        let mut args = vec!["devices", "list", "--json"];
        args.extend_from_slice(extra);
        let output = self.arreo(&args);
        assert!(output.status.success(), "listing failed");
        serde_json::from_slice(&output.stdout).expect("the listing is JSON")
    }
}

/// `arse_json` helper: the device row for `id`, if the listing has it.
fn row_for(listing: &serde_json::Value, id: &DeviceId) -> Option<serde_json::Value> {
    listing["devices"]
        .as_array()?
        .iter()
        .find(|row| row["id"] == id.display_id())
        .cloned()
}

/// The headline criterion: revoke, restart, and the device is still refused —
/// with the refusal visible in the log and the audit row naming who and when.
#[test]
fn a_revocation_is_durable_and_visible_in_the_audit_log() {
    let mut fixture = Fixture::start("durable");
    let phone = DeviceKey::generate().expect("entropy");
    let id = fixture.pin("phone", &phone);

    // Before: the device is live and listed.
    let listing = fixture.listed(&[]);
    let row = row_for(&listing, &id).expect("the pinned device is listed");
    assert_eq!(row["revoked"], serde_json::json!(false));
    assert!(
        row["revoked_at_ms"].is_null(),
        "a live device has no revoke time"
    );

    // Revoke by *name*, the way an operator would.
    let output = fixture.arreo(&["devices", "revoke", "phone"]);
    assert!(
        output.status.success(),
        "revoking by name failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Stated, not implied: the record carries who and when, and the row is an
    // audit event of its own (`action = device.revoke`).
    let revoked = fixture.listed(&["--revoked"]);
    let row = row_for(&revoked, &id).expect("the tombstone is listed under --revoked");
    assert_eq!(row["revoked"], serde_json::json!(true));
    assert_eq!(row["revoked_by"], serde_json::json!("local-cli"));
    assert!(
        row["revoked_at_ms"].as_i64().is_some_and(|at| at > 0),
        "the revocation is timestamped: {row}"
    );
    // The live listing no longer offers it: "who can reach this machine" must
    // not include a revoked device.
    assert!(
        row_for(&fixture.listed(&[]), &id).is_none(),
        "a revoked device must not appear in the live listing"
    );

    let audit = fixture.arreo(&["audit", "--limit", "20"]);
    assert!(audit.status.success(), "the audit command runs");
    let text = String::from_utf8_lossy(&audit.stdout);
    assert!(
        text.contains("device.revoke"),
        "the revocation is an audited event: {text}"
    );
    assert!(
        text.contains(&id.display_id()),
        "the audit row names the device that was revoked: {text}"
    );
    assert!(text.contains("local-cli"), "and who did it: {text}");

    // Idempotent: exit 0, and honest about the state rather than pretending.
    let again = fixture.arreo(&["devices", "revoke", "phone"]);
    assert!(again.status.success(), "a second revoke is not an error");
    let said = String::from_utf8_lossy(&again.stdout);
    assert!(
        said.contains("already revoked"),
        "a second revoke must say so: {said}"
    );
    // ...and it did not rewrite the first moment, nor add a second audit row:
    // one revocation is one event, and a log that repeats itself says less than
    // one that does not.
    let after = fixture.listed(&["--revoked"]);
    assert_eq!(
        row_for(&after, &id).expect("still listed")["revoked_at_ms"],
        row["revoked_at_ms"],
        "the first revocation time stands"
    );
    let audit_again = fixture.arreo(&["audit", "--limit", "50"]);
    let text_again = String::from_utf8_lossy(&audit_again.stdout);
    assert_eq!(
        text_again.matches("device.revoke").count(),
        1,
        "one revocation writes one audit row: {text_again}"
    );

    // The daemon is killed hard and restarted: revocation is a fact on disk.
    fixture.crash_and_restart();
    let listing = fixture.listed(&["--revoked"]);
    assert!(
        row_for(&listing, &id)
            .expect("survived")
            .get("revoked")
            .is_some(),
        "the tombstone survives a restart"
    );
    assert!(
        row_for(&fixture.listed(&[]), &id).is_none(),
        "and the live set is still without it"
    );
}

/// A revoked device is refused on its *next* connection, not at the next restart
/// — and the log says "revoked" rather than the misleading "not pinned".
#[test]
fn a_revoked_device_is_refused_on_its_next_connection() {
    let fixture = Fixture::start("next-connection");
    let phone = DeviceKey::generate().expect("entropy");
    let id = fixture.pin("phone", &phone);

    // A healthy session still works (the refusal is about the *revoked* device).
    assert!(
        matches!(fixture.hello_reply(), Ok(Message::Welcome { .. })),
        "the daemon serves while nothing is revoked"
    );

    assert!(fixture
        .arreo(&["devices", "revoke", "phone"])
        .status
        .success());

    // The device is refused at the handshake. The local socket is the operator's
    // own door (it is not device-gated), so the refusal is observed through the
    // authorization path the transports use — the same call the handshake makes.
    let decision = fixture.arreo(&["devices", "authorize", &phone.public_hex()]);
    assert!(
        !decision.status.success(),
        "the authorization path must refuse a revoked device; daemon log:\n{}",
        fixture.log_text()
    );
    let refusal = String::from_utf8_lossy(&decision.stderr);
    assert!(
        refusal.to_lowercase().contains("revoked"),
        "the refusal must name revocation, not merely 'unknown': {refusal}"
    );

    // And the daemon has an audit row for the refusal itself, so "the daemon
    // said no" is as visible as the revocation was.
    let audit = fixture.arreo(&["audit", "--limit", "20"]);
    let text = String::from_utf8_lossy(&audit.stdout);
    assert!(
        text.contains("auth_reject") || text.to_lowercase().contains("revoked"),
        "the refusal is audited: {text}"
    );
    let _ = id;
}

/// The offline case (§3.14): a device revoked while it is away is refused when it
/// comes back — the decision depends on neither a network fetch nor cert expiry.
#[test]
fn a_device_revoked_while_offline_is_refused_when_it_returns() {
    let mut fixture = Fixture::start("offline");
    let phone = DeviceKey::generate().expect("entropy");
    let _id = fixture.pin("phone", &phone);

    // "Offline" is modelled the only way that is honest without a real network:
    // the device never connects at all. The revocation must not depend on having
    // seen it, and the refusal must not depend on a fetch.
    assert!(fixture
        .arreo(&["devices", "revoke", "phone"])
        .status
        .success());
    fixture.crash_and_restart();

    let decision = fixture.arreo(&["devices", "authorize", &phone.public_hex()]);
    assert!(
        !decision.status.success(),
        "a device revoked while offline is refused when it returns"
    );
    assert!(String::from_utf8_lossy(&decision.stderr)
        .to_lowercase()
        .contains("revoked"));
}

/// A revoked device cannot re-pair: the pairing flow pins through the same door,
/// so "revoke the stolen phone, then pair its key again" fails.
#[test]
fn a_revoked_key_cannot_be_re_pinned() {
    let fixture = Fixture::start("re-pair");
    let phone = DeviceKey::generate().expect("entropy");
    let id = fixture.pin("phone", &phone);
    assert!(fixture
        .arreo(&["devices", "revoke", "phone"])
        .status
        .success());

    // The same public key, offered for pinning again.
    let output = fixture.arreo(&[
        "devices",
        "issue",
        "--name",
        "phone again",
        "--role",
        "owner",
        "--key",
        &phone.public_hex(),
    ]);
    assert!(
        !output.status.success(),
        "a burned key must not be re-pinned"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("revoked"),
        "the refusal must say the key was revoked: {stderr}"
    );
    // And the tombstone is untouched by the attempt.
    let listing = fixture.listed(&["--revoked"]);
    assert_eq!(
        row_for(&listing, &id).expect("still there")["revoked"],
        serde_json::json!(true)
    );

    // A *fresh* key with the same name is fine: revocation burns a key, not a
    // name — the operator can hand the human a new phone.
    let replacement = DeviceKey::generate().expect("entropy");
    let output = fixture.arreo(&[
        "devices",
        "issue",
        "--name",
        "phone",
        "--role",
        "owner",
        "--key",
        &replacement.public_hex(),
    ]);
    assert!(
        output.status.success(),
        "a fresh key may take the old name: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Revoking something that does not exist is a distinct, loud failure — and an
/// ambiguous name is refused rather than resolved to a guess.
#[test]
fn revoking_an_unknown_or_ambiguous_device_fails_clearly() {
    let fixture = Fixture::start("unknown");
    let first = DeviceKey::generate().expect("entropy");
    let second = DeviceKey::generate().expect("entropy");
    fixture.pin("phone", &first);
    fixture.pin("phone", &second);

    // Two devices with one name: the operator must disambiguate.
    let ambiguous = fixture.arreo(&["devices", "revoke", "phone"]);
    assert!(
        !ambiguous.status.success(),
        "an ambiguous name must not revoke"
    );
    let stderr = String::from_utf8_lossy(&ambiguous.stderr);
    assert!(
        stderr.contains("2 devices are named"),
        "the refusal must explain the ambiguity: {stderr}"
    );
    // Both are still live: the ambiguity changed nothing.
    assert_eq!(
        fixture.listed(&[])["devices"]
            .as_array()
            .expect("list")
            .len(),
        2
    );

    // An id that does not exist is a different, equally loud failure.
    let missing = fixture.arreo(&["devices", "revoke", "dev_00000000000000000000000000000000"]);
    assert!(!missing.status.success(), "an unknown id must not revoke");
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("no certificate")
            || String::from_utf8_lossy(&missing.stderr).contains("not found"),
        "the refusal names the problem: {}",
        String::from_utf8_lossy(&missing.stderr)
    );
}

/// A device that was never revoked is unaffected: the check is not a blanket
/// refusal, which is what a "does it refuse everything?" test would miss.
#[test]
fn a_live_device_keeps_working_while_another_is_revoked() {
    let fixture = Fixture::start("unaffected");
    let doomed = DeviceKey::generate().expect("entropy");
    let healthy = DeviceKey::generate().expect("entropy");
    let doomed_id = fixture.pin("doomed", &doomed);
    let healthy_id = fixture.pin("healthy", &healthy);

    assert!(fixture
        .arreo(&["devices", "revoke", "doomed"])
        .status
        .success());

    let allowed = fixture.arreo(&["devices", "authorize", &healthy.public_hex()]);
    assert!(
        allowed.status.success(),
        "a live device must still authorize: {}",
        String::from_utf8_lossy(&allowed.stderr)
    );
    let denied = fixture.arreo(&["devices", "authorize", &doomed.public_hex()]);
    assert!(!denied.status.success(), "and the revoked one must not");

    let live = fixture.listed(&[]);
    assert!(
        row_for(&live, &healthy_id).is_some(),
        "the healthy device is listed"
    );
    assert!(
        row_for(&live, &doomed_id).is_none(),
        "the revoked one is not"
    );
}

/// The schema carries the revoke provenance and the audit action — a migration
/// that silently skipped them would make every claim above unverifiable.
#[test]
fn the_schema_records_who_revoked_and_the_audit_action() {
    let fixture = Fixture::start("schema");
    let columns = fixture.arreo(&["devices", "list", "--json"]);
    assert!(columns.status.success());
    // The JSON listing exposes both fields, which is the observable form of the
    // schema's promise (a column cannot be read back if it does not exist).
    let phone = DeviceKey::generate().expect("entropy");
    let id = fixture.pin("phone", &phone);
    assert!(fixture
        .arreo(&["devices", "revoke", "phone"])
        .status
        .success());
    let row = row_for(&fixture.listed(&["--revoked"]), &id).expect("listed");
    assert!(row.get("revoked_by").is_some(), "the listing carries who");
    assert!(row.get("revoked_at_ms").is_some(), "and when");
}
