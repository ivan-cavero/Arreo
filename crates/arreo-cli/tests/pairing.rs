//! T-0024 pairing, driven across three real processes: the relay binary, the
//! server-side `arreo pair`, and the phone-side `arreo pair`.
//!
//! Nothing here is mocked: a real `arreo-relay` serves the mailbox on a real
//! socket, two real `arreo` processes run the SPAKE2 exchange, and the
//! assertions are about what a user and an operator would observe — the code
//! the server prints, the certificate the phone stores, the pin the server
//! keeps, and the audit row a failure leaves behind.
//!
//! Note on location: these tests live in `arreo-cli` (not `arreo-core`) because
//! only a package's own test target gets `CARGO_BIN_EXE_<name>`, and the thing
//! under test is the CLI. The relay is exercised as a *binary* rather than a
//! linked library on purpose: that keeps the AGPL `arreo-relay` crate out of
//! this crate's dependency graph entirely (T-0035 owns that boundary).

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The `arreo` binary under test.
fn arreo() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arreo"))
}

/// The relay binary, built into the same target directory. Missing means the
/// workspace was not built; that is a loud failure, because a silently skipped
/// security test is worse than a red one.
fn relay_binary() -> PathBuf {
    let dir = arreo().parent().expect("target dir").to_path_buf();
    let relay = dir.join("arreo-relay");
    assert!(
        relay.exists(),
        "{} is missing — run `cargo build -p arreo-relay` (or `cargo test --workspace`, \
         which builds every binary) before this test",
        relay.display()
    );
    relay
}

/// One scenario's directory: relay socket, server identity, phone identity.
struct Scenario {
    root: PathBuf,
}

impl Scenario {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "arreo-pairing-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scenario dir");
        Self { root }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn relay_socket(&self) -> PathBuf {
        self.path("relay.sock")
    }

    fn daemon_socket(&self) -> PathBuf {
        self.path("daemon.sock")
    }

    fn server_identity(&self) -> PathBuf {
        self.path("server")
    }

    fn phone_identity(&self) -> PathBuf {
        self.path("phone")
    }

    /// Start the relay and wait until its socket accepts a connection.
    fn start_relay(&self) -> Guard {
        let socket = self.relay_socket();
        let child = Command::new(relay_binary())
            .arg("--pairing-socket")
            .arg(&socket)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("relay starts");
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(
                Instant::now() < deadline,
                "the relay never bound {}",
                socket.display()
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        Guard::new(child)
    }

    /// `--mailbox` and its value, ready to append to `arreo pair` args.
    fn mailbox_args(&self) -> [String; 2] {
        [
            "--mailbox".to_string(),
            self.relay_socket().display().to_string(),
        ]
    }
}

impl Drop for Scenario {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A spawned process that gets killed if the test panics.
struct Guard(Child);

impl Guard {
    fn new(child: Child) -> Self {
        Self(child)
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A server-side `arreo pair` with its stdout open for line-by-line reading.
///
/// The reader is kept alive on purpose: dropping the read end of a child's
/// stdout pipe makes the child's next `println!` fail. A test that closed the
/// pipe would be measuring its own harness instead of the product.
struct PairServer {
    child: Guard,
    lines: BufReader<std::process::ChildStdout>,
}

impl PairServer {
    fn start(scenario: &Scenario, extra: &[&str]) -> Self {
        let mut command = Command::new(arreo());
        command
            .arg("pair")
            .args(["--json", "--ttl-secs", "30"])
            .args(extra)
            .args(scenario.mailbox_args())
            .arg("--socket")
            .arg(scenario.daemon_socket())
            .env("ARREO_IDENTITY_DIR", scenario.server_identity())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = Guard::new(command.spawn().expect("server pair starts"));
        let stdout = child.0.stdout.take().expect("server stdout is piped");
        Self {
            child,
            lines: BufReader::new(stdout),
        }
    }

    /// The JSON invite the server prints before it blocks.
    fn invite(&mut self) -> serde_json::Value {
        let line = self.next_line();
        serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("invite is not JSON ({e}): {line}"))
    }

    /// The JSON result the server prints once the phone is paired.
    fn result(&mut self) -> serde_json::Value {
        let line = self.next_line();
        serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("result is not JSON ({e}): {line}"))
    }

    fn next_line(&mut self) -> String {
        let mut line = String::new();
        self.lines
            .read_line(&mut line)
            .expect("the server keeps talking");
        assert!(!line.is_empty(), "the server stopped printing early");
        line
    }

    fn wait(&mut self) -> std::process::ExitStatus {
        self.child.0.wait().expect("server exits")
    }
}

/// Everything under `dir`, as (relative path, bytes) — compared directly, so
/// "byte-identical before and after" is literal rather than a hash of a hash.
fn snapshot_tree(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                out.push((path.strip_prefix(dir).unwrap_or(&path).to_path_buf(), bytes));
            }
        }
    }
    out.sort();
    out
}

fn json_of(output: &std::process::Output) -> Option<serde_json::Value> {
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .next_back()
}

fn text_of(output: &std::process::Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    text
}

/// Run the phone side of a pairing with its own identity directory.
fn run_phone(scenario: &Scenario, code: &str, uri: &str, name: &str) -> std::process::Output {
    Command::new(arreo())
        .args([
            "pair", "--join", code, "--uri", uri, "--json", "--name", name,
        ])
        .env("ARREO_IDENTITY_DIR", scenario.phone_identity())
        .output()
        .expect("phone pair runs")
}

/// `arreo devices list --json` against the server's authority.
fn list_devices(scenario: &Scenario) -> serde_json::Value {
    let output = Command::new(arreo())
        .args(["devices", "list", "--json", "--socket"])
        .arg(scenario.daemon_socket())
        .env("ARREO_IDENTITY_DIR", scenario.server_identity())
        .output()
        .expect("devices list runs");
    json_of(&output).unwrap_or_else(|| panic!("list is not JSON: {}", text_of(&output)))
}

/// The audit log the server's authority wrote.
fn audit_text(scenario: &Scenario) -> String {
    let output = Command::new(arreo())
        .args(["audit", "--limit", "20", "--socket"])
        .arg(scenario.daemon_socket())
        .env("ARREO_IDENTITY_DIR", scenario.server_identity())
        .output()
        .expect("audit runs");
    text_of(&output)
}

#[test]
fn two_real_processes_pair_with_a_four_word_code() {
    let scenario = Scenario::new("happy");
    let _relay = scenario.start_relay();

    let mut server = PairServer::start(&scenario, &["--name", "laptop", "--role", "owner"]);
    let invite = server.invite();

    // The code the human reads out: four words from the committed list.
    let code = invite["code"].as_str().expect("code").to_string();
    assert_eq!(code.split(' ').count(), 4, "{code:?}");
    let uri = invite["uri"].as_str().expect("uri").to_string();
    assert!(uri.starts_with("arreo://pair?v=1&"), "{uri}");
    // The code is typed by a human and never travels in the invite.
    for word in code.split(' ') {
        assert!(!uri.contains(word), "{word} leaked into the invite URI");
    }

    // The phone: a second real process with its own identity directory.
    let phone = run_phone(&scenario, &code, &uri, "pixel");
    let phone_json = match json_of(&phone) {
        Some(value) => value,
        None => panic!("the phone reported no JSON: {}", text_of(&phone)),
    };
    assert_eq!(
        phone_json["paired"],
        serde_json::json!(true),
        "{}",
        text_of(&phone)
    );
    assert_eq!(phone_json["role"], serde_json::json!("owner"));

    // The server reports the same device and exits cleanly.
    let server_result = server.result();
    assert_eq!(server_result["paired"], serde_json::json!(true));
    assert_eq!(server_result["role"], serde_json::json!("owner"));
    assert_eq!(server_result["name"], serde_json::json!("laptop"));
    let status = server.wait();
    assert!(status.success(), "the server exited {status:?}");

    let device = phone_json["device"]
        .as_str()
        .expect("device id")
        .to_string();
    assert!(device.starts_with("dev_"), "{device}");
    assert_eq!(server_result["device"], serde_json::json!(device));

    // The server pinned exactly that device, in the database and on disk.
    let listed = list_devices(&scenario);
    let pinned = &listed["devices"][0];
    assert_eq!(pinned["id"], serde_json::json!(device));
    assert_eq!(pinned["role"], serde_json::json!("owner"));
    assert_eq!(pinned["name"], serde_json::json!("laptop"));
    assert_eq!(pinned["revoked"], serde_json::json!(false));

    // The phone stored the certificate the server signed, the keypair it
    // belongs to, and the server key it must pin to verify future certificates.
    let identity = scenario.phone_identity().join("identity");
    let cert_files: Vec<_> = std::fs::read_dir(identity.join("devices"))
        .expect("the phone stored certificates")
        .flatten()
        .map(|entry| entry.path())
        .collect();
    assert_eq!(cert_files.len(), 1, "{cert_files:?}");
    assert!(
        cert_files[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains(&device[4..]),
        "{cert_files:?} does not name {device}"
    );
    assert!(
        identity.join("server.key").exists(),
        "the server key was not pinned"
    );
    assert!(
        identity.join("device.key").exists(),
        "the device key was not saved"
    );

    // The session is spent: the same invite cannot be replayed, even by its
    // rightful owner with the right code. This is what makes a captured
    // transcript worthless.
    let replay = run_phone(&scenario, &code, &uri, "pixel");
    assert!(
        !replay.status.success(),
        "a completed pairing was replayed: {}",
        text_of(&replay)
    );
}

#[test]
fn a_wrong_code_leaves_no_trace_on_the_phone_and_burns_the_session() {
    let scenario = Scenario::new("wrongcode");
    let _relay = scenario.start_relay();
    // The phone already has an identity (a failed re-pair must not disturb it).
    let before = snapshot_tree(&scenario.phone_identity());

    let mut server = PairServer::start(&scenario, &[]);
    let invite = server.invite();
    let uri = invite["uri"].as_str().expect("uri").to_string();
    // Right shape, wrong words: exactly what a mistyped code looks like.
    let wrong = "amber anchor apple arrow";

    let phone = run_phone(&scenario, wrong, &uri, "pixel");
    assert!(
        !phone.status.success(),
        "a wrong code must not pair: {}",
        text_of(&phone)
    );

    // The server also failed: it was the side that could see the mismatch.
    let status = server.wait();
    assert!(
        !status.success(),
        "the server reported success on a wrong code"
    );

    // Nothing was written on the phone: no key, no certificate, no pin.
    assert_eq!(
        snapshot_tree(&scenario.phone_identity()),
        before,
        "a failed pairing left something behind on the phone"
    );
    assert!(
        list_devices(&scenario)["devices"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "a failed pairing pinned a device"
    );

    // The failure is an auditable event: exactly one `pairing.failed` row, and it
    // says what went wrong. (The action name, not the older coarse kind: T-0033
    // made `action` the column an operator greps for, and `pairing.failed` is the
    // same event under its finer name.)
    let audit = audit_text(&scenario);
    assert!(
        audit.contains("pairing.failed"),
        "no pairing failure in the audit log: {audit}"
    );
    assert!(
        audit.contains("did not match"),
        "the audit row does not name the cause: {audit}"
    );
    assert_eq!(
        audit.matches("pairing.failed").count(),
        1,
        "a wrong code wrote the wrong number of rows: {audit}"
    );

    // The session is burned, so a second attempt cannot start over.
    let replay = run_phone(&scenario, wrong, &uri, "pixel");
    assert!(
        !replay.status.success(),
        "a burned session was reused: {}",
        text_of(&replay)
    );
}

#[test]
fn an_unanswered_pairing_expires_instead_of_hanging() {
    let scenario = Scenario::new("expiry");
    let _relay = scenario.start_relay();
    // A 1 s window: the deadline is proven without sleeping for the real
    // 5-minute default.
    let started = Instant::now();
    let server = Command::new(arreo())
        .args(["pair", "--json", "--ttl-secs", "1"])
        .args(scenario.mailbox_args())
        .arg("--socket")
        .arg(scenario.daemon_socket())
        .env("ARREO_IDENTITY_DIR", scenario.server_identity())
        .output()
        .expect("server pair runs");
    let elapsed = started.elapsed();
    assert!(!server.status.success(), "an unanswered pairing must fail");
    assert!(
        elapsed < Duration::from_secs(20),
        "the window did not close in time ({elapsed:?})"
    );
    let text = text_of(&server);
    assert!(
        text.contains("window") || text.contains("expired"),
        "{text}"
    );

    // An operator can see the attempt, as a pairing event rather than a
    // generic refusal, and nothing was pinned by it.
    let audit = audit_text(&scenario);
    assert!(
        audit.contains("pairing.failed"),
        "the expired attempt was not audited as a pairing failure: {audit}"
    );
    assert!(
        audit.contains("window") || audit.contains("expired"),
        "the audit row does not say why: {audit}"
    );
    assert_eq!(
        audit.matches("pairing.failed").count(),
        1,
        "an expired pairing wrote the wrong number of rows: {audit}"
    );
    assert!(
        list_devices(&scenario)["devices"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "an expired pairing pinned a device"
    );
}

#[test]
fn pairing_reports_useful_errors_for_bad_arguments() {
    let scenario = Scenario::new("args");
    let stub_uri = "arreo://pair?v=1&mb=/tmp/x.sock&s=s&k=k";

    // A code with the wrong number of words never reaches the network.
    let few = Command::new(arreo())
        .args(["pair", "--join", "amber anchor apple", "--uri", stub_uri])
        .env("ARREO_IDENTITY_DIR", scenario.phone_identity())
        .output()
        .expect("runs");
    assert!(!few.status.success());
    assert!(text_of(&few).contains("4 words"), "{}", text_of(&few));

    // A typo suggests the nearest word rather than failing silently.
    let typo = Command::new(arreo())
        .args([
            "pair",
            "--join",
            "bambu anchor apple arrow",
            "--uri",
            stub_uri,
        ])
        .env("ARREO_IDENTITY_DIR", scenario.phone_identity())
        .output()
        .expect("runs");
    assert!(!typo.status.success());
    assert!(text_of(&typo).contains("bamboo"), "{}", text_of(&typo));

    // `--join` without `--uri` is refused: the invite carries the mailbox,
    // session and server key.
    let no_uri = Command::new(arreo())
        .args(["pair", "--join", "amber anchor apple arrow"])
        .env("ARREO_IDENTITY_DIR", scenario.phone_identity())
        .output()
        .expect("runs");
    assert!(!no_uri.status.success());
    assert!(text_of(&no_uri).contains("--uri"), "{}", text_of(&no_uri));

    // An invite whose mailbox is unreachable fails loudly instead of pretending
    // to wait for a phone that can never arrive.
    let no_relay = Command::new(arreo())
        .args(["pair", "--json", "--ttl-secs", "1", "--mailbox"])
        .arg(scenario.path("nothing-here.sock"))
        .arg("--socket")
        .arg(scenario.daemon_socket())
        .env("ARREO_IDENTITY_DIR", scenario.server_identity())
        .output()
        .expect("runs");
    assert!(!no_relay.status.success());
    assert!(
        text_of(&no_relay).contains("cannot reach the pairing mailbox"),
        "{}",
        text_of(&no_relay)
    );
    // Nothing was pinned by the failed attempt. (The server does bootstrap its
    // own root key on first run — that is its identity, not a device — so the
    // check is about devices, not about the directory being empty.)
    assert!(
        list_devices(&scenario)["devices"]
            .as_array()
            .is_some_and(Vec::is_empty),
        "a failed pairing pinned a device"
    );
    let cert_dir = scenario.server_identity().join("identity").join("devices");
    let certs = std::fs::read_dir(&cert_dir)
        .map(|read| read.count())
        .unwrap_or(0);
    assert_eq!(certs, 0, "a failed pairing wrote a certificate");
}

#[test]
fn a_phone_that_arrives_after_the_window_is_told_so() {
    let scenario = Scenario::new("late");
    let _relay = scenario.start_relay();
    // A session that never existed (the server above already closed one).
    let uri = format!(
        "arreo://pair?v=1&mb={}&s=deadbeefdeadbeefdeadbeefdeadbeef&k={}&ttl=1",
        scenario.relay_socket().display(),
        "0".repeat(64)
    );
    let phone = run_phone(&scenario, "amber anchor apple arrow", &uri, "pixel");
    assert!(
        !phone.status.success(),
        "a phone joined a session that never existed: {}",
        text_of(&phone)
    );
    assert!(
        text_of(&phone).contains("no such session"),
        "{}",
        text_of(&phone)
    );
}

/// The relay is a shared mailbox, not a private channel: a second pairing can
/// run while the first is waiting, and the sessions do not interfere.
#[test]
fn sessions_are_isolated_from_each_other() {
    let scenario = Scenario::new("isolation");
    let _relay = scenario.start_relay();

    let mut first = PairServer::start(&scenario, &[]);
    let first_invite = first.invite();

    // A second server gets a *different* session id and its own window.
    let second = Command::new(arreo())
        .args(["pair", "--json", "--ttl-secs", "1"])
        .args(scenario.mailbox_args())
        .arg("--socket")
        .arg(scenario.path("daemon2.sock"))
        .env("ARREO_IDENTITY_DIR", scenario.path("server2"))
        .output()
        .expect("second server runs");
    let second_json = json_of(&second).expect("second server reports JSON");
    assert_ne!(
        second_json["session"], first_invite["session"],
        "two pairings shared a session id"
    );

    // The first session is unaffected: its phone pairs normally afterwards.
    let code = first_invite["code"].as_str().expect("code");
    let uri = first_invite["uri"].as_str().expect("uri");
    let phone = run_phone(&scenario, code, uri, "pixel");
    assert!(phone.status.success(), "{}", text_of(&phone));
    let status = first.wait();
    assert!(status.success(), "the waiting server exited {status:?}");
}

/// **T-0067: a machine that admits itself must get a certificate the account
/// root actually signs.**
///
/// This is the first-machine bootstrap — the only way a new account gets its
/// first member — and it was found broken by exercising ROADMAP §6's exit
/// criterion. Every other pairing test here runs the relay in **mailbox-only**
/// mode, which registers no account and therefore never verifies a certificate
/// against a root; and the mesh slice's certificates are issued *in process* by
/// `DeviceCert::issue`. So the certificate this path produces was checked
/// against nothing, anywhere, until now.
///
/// The assertion is deliberately the narrowest possible: load the certificate
/// that was written, load the root key, and ask the product's own verifier
/// whether they match. If this fails, no amount of index loading or naming can
/// save it.
#[test]
fn a_self_admitted_machine_gets_a_certificate_the_root_signs() {
    let scenario = Scenario::new("self-admit");

    // The machine: no identity at all yet, just a directory.
    let machine = scenario.path("machine");
    std::fs::create_dir_all(&machine).expect("machine dir");

    // Its root key, created by the product's own tool, and the *public* half
    // registered as the account root — the same two steps the exit-criterion
    // script performs by hand.
    let listed = Command::new(arreo())
        .args(["devices", "list", "--json"])
        .env("ARREO_IDENTITY_DIR", &machine)
        .output()
        .expect("devices list runs");
    assert!(listed.status.success(), "devices list failed");
    let root_hex = serde_json::from_slice::<serde_json::Value>(&listed.stdout)
        .expect("devices list --json")["root"]
        .as_str()
        .expect("a root key")
        .to_string();

    // A real relay with a real account, so the certificate has something to be
    // verified against. The account is registered **before** the relay starts:
    // the relay holds the database, and two writers racing it made this fixture
    // flaky.
    let state_dir = scenario.path("relay-state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let registered = Command::new(relay_binary())
        .args([
            "account",
            "add",
            "--state-dir",
            &state_dir.display().to_string(),
            "--account",
            "acct-self",
            "--root-key",
            &root_hex,
        ])
        .output()
        .expect("account add runs");
    assert!(
        registered.status.success(),
        "registering the account failed: {}",
        String::from_utf8_lossy(&registered.stderr)
    );
    let mut relay = Command::new(relay_binary())
        .args([
            "serve",
            "--listen",
            "127.0.0.1:0",
            "--state-dir",
            &state_dir.display().to_string(),
            "--pairing-socket",
            &scenario.relay_socket().display().to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the relay starts");
    // The relay's bound address, for the daemon's configuration below. Read from
    // its own announcement rather than assumed: `--listen 127.0.0.1:0` means the
    // kernel chose the port.
    // The relay's stderr must be drained for its whole life, not just until the
    // address arrives: a full pipe blocks the relay, which then stops answering
    // and looks exactly like a transport failure. (The same trap T-0024 hit.)
    let relay_addr = {
        let stderr = relay.stderr.take().expect("relay stderr");
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if let Some(rest) = line.split("router on ").nth(1) {
                    if let Some(addr) = rest.split_whitespace().next() {
                        let _ = addr_tx.send(addr.to_string());
                    }
                }
            }
        });
        addr_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the relay announces the address it bound")
    };
    let _guard = Guard::new(relay);
    // The mailbox socket must exist before `arreo pair` dials it.
    let deadline = Instant::now() + Duration::from_secs(10);
    while std::os::unix::net::UnixStream::connect(scenario.relay_socket()).is_err() {
        assert!(
            Instant::now() < deadline,
            "the relay never bound its pairing socket"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // Step 1: the machine prints a code for itself.
    let mut server = Command::new(arreo())
        .args(["pair", "--mailbox"])
        .arg(scenario.relay_socket())
        .arg("--json")
        .env("ARREO_IDENTITY_DIR", &machine)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("pair starts");
    // The invite is printed before the server blocks waiting for a joiner.
    let stdout = server.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    reader.read_line(&mut line).expect("invite line");
    let invite: serde_json::Value = serde_json::from_str(&line).expect("invite json");
    let code = invite["code"].as_str().expect("code").to_string();
    let uri = invite["uri"].as_str().expect("uri").to_string();

    // Step 2: the same machine joins with its own code.
    let joined = Command::new(arreo())
        .args(["pair", "--join", &code, "--uri", &uri, "--json"])
        .env("ARREO_IDENTITY_DIR", &machine)
        .output()
        .expect("pair --join runs");
    assert!(
        joined.status.success(),
        "the self-join failed: {}",
        String::from_utf8_lossy(&joined.stderr)
    );
    let _ = server.kill();
    let _ = server.wait();

    // The certificate on disk must be verifiable by the account root that this
    // machine registered with. This is the assertion the whole task is about.
    let identity = machine.join("identity");
    // `load_or_generate` is the only loader (the daemon uses it too); the
    // assertion below makes a missing file a loud failure rather than a silent
    // fresh key that would make this test meaningless.
    let root = arreo_core::identity::RootKey::load_or_generate(&identity.join("root.key"))
        .expect("the machine's root key");
    assert_eq!(
        root.public_hex(),
        root_hex,
        "the root key on disk must be the one the account was registered with"
    );
    let key = arreo_core::identity::DeviceKey::load(&identity.join("device.key"))
        .expect("the machine's device key");
    let device = arreo_core::identity::DeviceId::from_key(&key.public());
    let cert_path = identity
        .join("devices")
        .join(format!("{}.cert", device.as_str()));
    let cert = arreo_core::identity::DeviceCert::load(&cert_path).unwrap_or_else(|e| {
        panic!(
            "no certificate for this machine's own device key at {}: {e}\n\
             the certificates present are: {:?}",
            cert_path.display(),
            std::fs::read_dir(identity.join("devices"))
                .map(|entries| entries
                    .flatten()
                    .map(|entry| entry.file_name())
                    .collect::<Vec<_>>())
                .unwrap_or_default()
        )
    });
    assert_eq!(
        cert.device(),
        &device,
        "the certificate must name the key this machine holds"
    );
    cert.verify(&root.public(), &key.public())
        .unwrap_or_else(|e| panic!("a self-admitted certificate must verify under the root that signed it: {e}"));

    // **And the half the unit check above cannot see: the daemon must be able to
    // register with the relay using that certificate.** That is where the
    // criterion actually failed, and a certificate that verifies in isolation but
    // is not the one the daemon presents would pass the check above and still
    // leave the machine invisible to its account.
    let config = machine.join("arreo.toml");
    std::fs::write(
        &config,
        format!(
            "[relay]\nenabled = true\naddr = \"{relay_addr}\"\naccount = \"acct-self\"\nname = \"self-admitted\"\n"
        ),
    )
    .expect("config");
    let socket = machine.join("daemon.sock");
    let server_daemon = Command::new(
        arreo()
            .parent()
            .expect("target dir")
            .join("arreo-server"),
    )
    .arg("--socket")
    .arg(&socket)
    .arg("--config")
    .arg(&config)
    .env("ARREO_IDENTITY_DIR", &machine)
    .stdout(Stdio::null())
    .stderr(Stdio::piped())
    .spawn()
    .expect("the daemon starts");
    let mut daemon = Guard::new(server_daemon);
    let daemon_stderr = daemon.0.stderr.take().expect("daemon stderr");
    let mut daemon_reader = BufReader::new(daemon_stderr);
    let mut saw_directory_row = false;
    let mut log = String::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline && !saw_directory_row {
        let mut line = String::new();
        if daemon_reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if line.contains("this machine is") {
            saw_directory_row = true;
        }
        log.push_str(&line);
    }
    assert!(
        saw_directory_row,
        "a self-admitted machine must register with its account's relay — the daemon said:\n{log}"
    );
}
