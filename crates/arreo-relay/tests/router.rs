//! T-0029 acceptance tests: a real relay process, real QUIC, real certificates.
//!
//! Why a spawned binary and not an in-process router: the criteria that matter
//! here are about *behavior across a process boundary* — what the relay writes
//! to its state directory, what it logs, whether a session actually
//! authenticates — and T-0024 already taught this repo that an in-process test
//! can be green while the real thing cannot complete. So the tests below run
//! `arreo-relay serve` as a child process, register an account through the
//! binary's own `account add`, and speak the protocol over loopback QUIC.

use arreo_core::identity::{DeviceCert, DeviceId, DeviceKey, Role, RootKey, VerifyingKey};
use arreo_core::relay::{
    decode_message, encode_message, read_frame, write_frame, Auth, AuthReply, Hello, HelloReply,
    Incoming, Outcome, RelayClient, RelayError, RELAY_VERSION,
};
use arreo_core::transport::{client_endpoint, SERVER_NAME};
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

/// A running relay, its state directory, and everything it has logged.
struct Relay {
    child: Child,
    addr: SocketAddr,
    state_dir: PathBuf,
    log: std::sync::Arc<std::sync::Mutex<String>>,
}

impl Drop for Relay {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.state_dir);
    }
}

impl Relay {
    /// Start the real binary and wait until it reports the address it bound.
    fn start(tag: &str) -> Self {
        let state_dir = std::env::temp_dir().join(format!(
            "arreo-relay-router-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&state_dir);
        std::fs::create_dir_all(&state_dir).expect("scratch state dir");
        let mut child = Command::new(relay_binary())
            .args([
                "serve",
                "--listen",
                "127.0.0.1:0",
                "--state-dir",
                &state_dir.display().to_string(),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the relay starts");

        // Read stderr until the router announces its bound address. Everything
        // read is kept: the opacity test scans it.
        let stderr = child.stderr.take().expect("stderr is piped");
        let log = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let (ready_tx, ready_rx) = mpsc::channel();
        {
            let log = std::sync::Arc::clone(&log);
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if let Some(addr) = parse_router_addr(&line) {
                        let _ = ready_tx.send(addr);
                    }
                    let mut held = log.lock().expect("log");
                    held.push_str(&line);
                    held.push('\n');
                }
            });
        }
        let addr = ready_rx
            .recv_timeout(Duration::from_secs(20))
            .expect("the relay announces its address");
        Self {
            child,
            addr,
            state_dir,
            log,
        }
    }

    fn log_text(&self) -> String {
        self.log.lock().expect("log").clone()
    }

    /// Register an account through the binary's own admin command.
    fn register_account(&self, account_id: &str, root: &VerifyingKey) {
        let output = Command::new(relay_binary())
            .args([
                "account",
                "add",
                "--state-dir",
                &self.state_dir.display().to_string(),
                "--account",
                account_id,
                "--root-key",
                &hex(root.to_bytes().as_slice()),
            ])
            .output()
            .expect("the account command runs");
        assert!(
            output.status.success(),
            "registering an account failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn relay_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arreo-relay"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn parse_router_addr(line: &str) -> Option<SocketAddr> {
    let rest = line.split("router on ").nth(1)?;
    let addr = rest.split_whitespace().next()?;
    addr.parse().ok()
}

/// A device with its certificate, issued by `root`.
fn device(root: &RootKey, name: &str, serial: u64) -> (DeviceKey, DeviceCert) {
    let key = DeviceKey::generate().expect("entropy");
    let cert = DeviceCert::issue(root, &key.public(), name, Role::Owner, 1_000, serial);
    (key, cert)
}

fn payload(marker: &str) -> Vec<u8> {
    // Pane-shaped: escape sequences, a prompt, and a marker the scan looks for.
    format!("\x1b[32magent\x1b[0m $ {marker}\n> waiting for input\n").into_bytes()
}

#[tokio::test]
async fn two_devices_exchange_opaque_bytes_through_the_relay() {
    let relay = Relay::start("exchange");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());

    let (alice_key, alice_cert) = device(&root, "alice", 1);
    let (bob_key, bob_cert) = device(&root, "bob", 2);
    let bob_id = bob_cert.device().clone();

    // Bob connects first: he is the destination, so he must be known.
    let mut bob = RelayClient::connect(relay.addr, "acct-1", &bob_key, &bob_cert)
        .await
        .expect("bob authenticates");
    assert_eq!(bob.device_id(), &bob_id);

    let mut alice = RelayClient::connect(relay.addr, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice authenticates");

    let marker = "ARREO-OPAQUE-MARKER-9f3c1a";
    let sent = payload(marker);
    let seq = alice.send(&bob_id, &sent).await.expect("alice sends");

    // Bob receives it byte-identical.
    let received = bob.recv_envelope().await.expect("bob receives");
    assert_eq!(
        received.payload, sent,
        "the payload must survive the relay byte-identical"
    );
    assert_eq!(received.header.src_device, alice_cert.device().display_id());
    assert_eq!(received.header.dst, bob_id.display_id());

    // Alice is told it was delivered, on the sequence number she used.
    match alice.next().await.expect("alice is answered") {
        Incoming::Status {
            seq: reported,
            outcome,
        } => {
            assert_eq!(reported, seq);
            assert_eq!(outcome, Outcome::Delivered);
        }
        other => panic!("expected a delivery report, got {other:?}"),
    }

    // The relay must not have written or logged the content anywhere.
    let marker_bytes = marker.as_bytes();
    let mut scanned = Vec::new();
    for entry in std::fs::read_dir(&relay.state_dir).expect("state dir") {
        let path = entry.expect("entry").path();
        if path.is_file() {
            let bytes = std::fs::read(&path).expect("read state file");
            assert!(
                !contains(&bytes, marker_bytes),
                "the payload reached {} — the relay must not persist what it routes",
                path.display()
            );
            scanned.push(path);
        }
    }
    assert!(!scanned.is_empty(), "the relay keeps a state file to scan");
    let log = relay.log_text();
    assert!(
        !log.contains(marker),
        "the payload reached the relay's log:\n{log}"
    );
    // The log is not empty — otherwise the assertion above is vacuous.
    assert!(
        log.contains("authenticated"),
        "the relay logs its sessions:\n{log}"
    );
}

/// A destination that has never authenticated is a typed refusal, not a guess.
#[tokio::test]
async fn an_unknown_destination_is_refused_by_name() {
    let relay = Relay::start("unknown-dst");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let (alice_key, alice_cert) = device(&root, "alice", 1);
    let mut alice = RelayClient::connect(relay.addr, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice authenticates");

    let stranger = DeviceKey::generate().expect("entropy");
    let stranger_id = DeviceId::from_key(&stranger.public());
    alice.send(&stranger_id, b"hello?").await.expect("send");
    let answered = alice.next().await;
    let answered = match answered {
        Ok(answered) => answered,
        Err(e) => panic!(
            "the relay did not answer ({e}); its log was:\n{}",
            relay.log_text()
        ),
    };
    match answered {
        Incoming::Status { outcome, .. } => {
            assert_eq!(outcome, Outcome::NoSuchDevice, "an unknown device is named");
        }
        other => panic!("expected a status, got {other:?}"),
    }
}

/// A known device that is not connected is `Offline` — distinct from unknown,
/// because the sender's next move differs (retry later vs fix the address).
#[tokio::test]
async fn a_known_device_that_is_offline_is_reported_as_such() {
    let relay = Relay::start("offline");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let (bob_key, bob_cert) = device(&root, "bob", 1);
    let bob_id = bob_cert.device().clone();
    // Bob connects once so the relay knows him, then goes away.
    {
        let bob = RelayClient::connect(relay.addr, "acct-1", &bob_key, &bob_cert)
            .await
            .expect("bob authenticates");
        drop(bob);
    }
    // Give the relay a moment to notice the disconnect.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let (alice_key, alice_cert) = device(&root, "alice", 2);
    let mut alice = RelayClient::connect(relay.addr, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice authenticates");
    alice.send(&bob_id, b"anyone there?").await.expect("send");
    match alice.next().await.expect("answered") {
        Incoming::Status { outcome, .. } => {
            assert_eq!(
                outcome,
                Outcome::Offline,
                "a known device is offline, not unknown"
            );
        }
        other => panic!("expected a status, got {other:?}"),
    }
}

/// A device may not speak as another, and may not reach into another account.
#[tokio::test]
async fn a_spoofed_sender_and_a_foreign_account_are_refused() {
    let relay = Relay::start("spoof");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    relay.register_account("acct-2", &root.public());

    let (alice_key, alice_cert) = device(&root, "alice", 1);
    let (bob_key, bob_cert) = device(&root, "bob", 2);
    let bob_id = bob_cert.device().clone();
    let _bob = RelayClient::connect(relay.addr, "acct-1", &bob_key, &bob_cert)
        .await
        .expect("bob authenticates");

    // A session the test owns, so it can write frames the honest client never
    // would. (Adding a "send anything" method to the real client for the sake of
    // a test would put a forgery door in the shipped API.)
    let mut io = manual_session(&relay, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice authenticates");
    let mut buf = Vec::new();

    // Spoof: claim bob as the sender while holding alice's session.
    let forged = forged_envelope(
        "acct-1",
        &bob_id.display_id(), // not alice's id
        &bob_id.display_id(),
        b"spoofed",
    );
    write_frame(&mut io, &forged).await.expect("forged write");
    match read_outcome(&mut io, &mut buf).await {
        Outcome::Refused { reason } => {
            assert!(
                reason.contains("session is"),
                "the refusal names the problem: {reason}"
            );
        }
        other => panic!("a spoofed sender must be refused, got {other:?}"),
    }

    // Foreign account: the session is acct-1, so an acct-2 header is refused
    // before any routing decision is made.
    let cross = forged_envelope(
        "acct-2",
        &alice_cert.device().display_id(),
        &bob_id.display_id(),
        b"cross-account",
    );
    write_frame(&mut io, &cross).await.expect("cross write");
    match read_outcome(&mut io, &mut buf).await {
        Outcome::Refused { reason } => {
            assert!(
                reason.contains("account"),
                "the refusal names the account: {reason}"
            );
        }
        other => panic!("a foreign account must be refused, got {other:?}"),
    }
}

/// An unknown account is refused before any cryptography runs.
#[tokio::test]
async fn an_unknown_account_is_refused() {
    let relay = Relay::start("unknown-account");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let (key, cert) = device(&root, "alice", 1);
    let refused = RelayClient::connect(relay.addr, "acct-nope", &key, &cert).await;
    match refused {
        Err(arreo_core::relay::ClientError::Refused { reason }) => assert!(
            reason.contains("unknown account"),
            "the refusal must name the account: {reason}"
        ),
        other => panic!("an unknown account must be refused: {other:?}"),
    }
}

/// A certificate signed by a different root authorizes nothing.
#[tokio::test]
async fn a_foreign_certificate_is_refused() {
    let relay = Relay::start("foreign-cert");
    let root = RootKey::generate().expect("entropy");
    let other_root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let (key, cert) = device(&other_root, "stranger", 1);
    let refused = RelayClient::connect(relay.addr, "acct-1", &key, &cert).await;
    match refused {
        Err(arreo_core::relay::ClientError::Refused { reason }) => assert!(
            reason.contains("certificate"),
            "the refusal must name the certificate: {reason}"
        ),
        other => panic!("a foreign certificate must be refused: {other:?}"),
    }
}

/// A certificate is a public document: presenting it proves nothing, so a
/// handshake with no proof of possession is refused.
///
/// Its own relay, because every refusal here costs the peer a handshake attempt
/// and the relay budgets those per address — which the next test pins.
#[tokio::test]
async fn a_certificate_without_a_proof_is_refused() {
    let relay = Relay::start("no-proof");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let (_key, cert) = device(&root, "alice", 1);
    let refused = handshake_with(&relay, "acct-1", &cert, |_nonce| vec![0u8; 64]).await;
    match refused {
        Err(RelayError::Refused { reason }) => assert!(
            reason.contains("did not prove"),
            "the refusal must name the missing proof: {reason}"
        ),
        other => panic!("a certificate without a proof must be refused: {other:?}"),
    }
}

/// A proof over a *different* nonce is refused — the replay case, and the whole
/// point of the relay contributing a fresh nonce.
#[tokio::test]
async fn a_replayed_proof_is_refused() {
    let relay = Relay::start("replay");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let (key, cert) = device(&root, "alice", 1);
    let canonical = cert.device().as_str().to_string();
    let refused = handshake_with(&relay, "acct-1", &cert, |_nonce| {
        // Signed over a nonce from some earlier handshake, which is exactly what
        // a recorded transcript would contain. (The id is canonical, so the only
        // thing wrong with this proof is the nonce — which is the point.)
        key.sign(&arreo_core::relay::proof_payload(
            b"an-old-nonce",
            "acct-1",
            &canonical,
        ))
        .to_bytes()
        .to_vec()
    })
    .await;
    match refused {
        Err(RelayError::Refused { reason }) => assert!(
            reason.contains("did not prove"),
            "a replayed proof must be refused as a failed proof: {reason}"
        ),
        other => panic!("a replayed proof must be refused: {other:?}"),
    }

    // And the honest device still gets in, so none of the refusals above is
    // passing because the handshake is simply broken.
    let client = RelayClient::connect(relay.addr, "acct-1", &key, &cert)
        .await
        .expect("the honest device authenticates");
    assert_eq!(client.device_id(), cert.device());
}

/// The handshake budget is real: a peer that keeps failing is turned away, which
/// is what stops a hostile address from making the relay do unbounded work.
#[tokio::test]
async fn a_peer_that_keeps_failing_is_rate_limited() {
    let relay = Relay::start("rate-limit");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());

    // Three refusals are the budget; the fourth attempt never reaches the
    // handshake.
    for attempt in 1..=3 {
        let (_key, cert) = device(&root, "alice", attempt);
        let refused = handshake_with(&relay, "acct-1", &cert, |_nonce| vec![0u8; 64]).await;
        assert!(
            matches!(refused, Err(RelayError::Refused { .. })),
            "attempt {attempt} should be refused, not limited: {refused:?}"
        );
    }
    let (_key, cert) = device(&root, "alice", 4);
    let limited = handshake_with(&relay, "acct-1", &cert, |_nonce| vec![0u8; 64]).await;
    assert!(
        limited.is_err(),
        "the fourth attempt from one address must not be served: {limited:?}"
    );
}

/// The account registry is durable: a relay restart still knows the account,
/// which is what makes a self-hosted relay survive a reboot.
#[tokio::test]
async fn a_restart_keeps_the_account_and_still_admits_its_devices() {
    let mut relay = Relay::start("restart");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let (key, cert) = device(&root, "alice", 1);
    {
        let client = RelayClient::connect(relay.addr, "acct-1", &key, &cert)
            .await
            .expect("first session");
        drop(client);
    }

    // Restart the binary against the same state directory.
    let _ = relay.child.kill();
    let state_dir = relay.state_dir.clone();
    let mut restarted = Command::new(relay_binary())
        .args([
            "serve",
            "--listen",
            "127.0.0.1:0",
            "--state-dir",
            &state_dir.display().to_string(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the relay restarts");
    let stderr = restarted.stderr.take().expect("stderr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if let Some(addr) = parse_router_addr(&line) {
                let _ = tx.send(addr);
            }
        }
    });
    let addr = rx.recv_timeout(Duration::from_secs(20)).expect("restarted");

    let client = RelayClient::connect(addr, "acct-1", &key, &cert)
        .await
        .expect("the account survived the restart");
    assert_eq!(client.device_id(), cert.device());
    let _ = restarted.kill();
    let _ = restarted.wait();
    let _ = std::fs::remove_dir_all(&state_dir);
}

/// A quiet peer must not stop the relay from serving the next device.
#[tokio::test]
async fn a_stalled_peer_does_not_block_the_next_device() {
    let relay = Relay::start("stall");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());

    // Open a QUIC connection and say nothing at all.
    let endpoint = client_endpoint().expect("client endpoint");
    if let Ok(connecting) = endpoint.connect(relay.addr, SERVER_NAME) {
        let _ = tokio::time::timeout(Duration::from_millis(150), connecting).await;
    }

    let (key, cert) = device(&root, "alice", 1);
    let client = tokio::time::timeout(
        Duration::from_secs(5),
        RelayClient::connect(relay.addr, "acct-1", &key, &cert),
    )
    .await
    .expect("the relay still answers while one peer stalls")
    .expect("a live device authenticates");
    assert_eq!(client.device_id(), cert.device());
}

/// A device that reconnects must keep its *newer* session: the old connection's
/// cleanup must not deregister the new one, or a reconnect would silently make
/// the device unreachable.
#[tokio::test]
async fn a_reconnecting_device_keeps_its_newer_session() {
    let relay = Relay::start("reconnect");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());

    let (bob_key, bob_cert) = device(&root, "bob", 1);
    let bob_id = bob_cert.device().clone();
    let first = RelayClient::connect(relay.addr, "acct-1", &bob_key, &bob_cert)
        .await
        .expect("bob's first session");
    let mut second = RelayClient::connect(relay.addr, "acct-1", &bob_key, &bob_cert)
        .await
        .expect("bob's second session");

    // Drop the first while the second is live: this is the race the registration
    // token exists for.
    drop(first);
    tokio::time::sleep(Duration::from_millis(200)).await;

    let (alice_key, alice_cert) = device(&root, "alice", 2);
    let mut alice = RelayClient::connect(relay.addr, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice authenticates");
    alice.send(&bob_id, b"still there?").await.expect("send");
    let delivered = tokio::time::timeout(Duration::from_secs(3), second.recv_envelope())
        .await
        .expect("the newer session is still registered")
        .expect("bob receives");
    assert_eq!(delivered.payload, b"still there?");
}

/// A zero-length payload is a payload, not an error.
#[tokio::test]
async fn a_zero_length_payload_is_carried() {
    let relay = Relay::start("empty-payload");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let (bob_key, bob_cert) = device(&root, "bob", 1);
    let bob_id = bob_cert.device().clone();
    let mut bob = RelayClient::connect(relay.addr, "acct-1", &bob_key, &bob_cert)
        .await
        .expect("bob authenticates");
    let (alice_key, alice_cert) = device(&root, "alice", 2);
    let mut alice = RelayClient::connect(relay.addr, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice authenticates");

    alice.send(&bob_id, b"").await.expect("send empty");
    let received = bob.recv_envelope().await.expect("bob receives");
    assert!(received.payload.is_empty(), "an empty payload stays empty");
}

/// An envelope that claims to be larger than the cap is refused *before* the
/// relay buffers anything: the cap is what stops one sender from making the
/// relay allocate without bound.
#[tokio::test]
async fn an_oversized_envelope_ends_the_session_without_buffering_it() {
    let relay = Relay::start("oversized");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let (alice_key, alice_cert) = device(&root, "alice", 1);
    let mut io = manual_session(&relay, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice authenticates");

    // A length prefix far beyond the cap, with almost no body behind it: a relay
    // that trusted the prefix would sit waiting to allocate a gigabyte.
    let mut frame = ((arreo_core::relay::MAX_ENVELOPE_BYTES + 1) as u32)
        .to_le_bytes()
        .to_vec();
    frame.extend_from_slice(b"short");
    write_frame(&mut io, &frame)
        .await
        .expect("the bogus frame is written");

    // The relay ends the session instead of waiting for the rest.
    let mut buf = Vec::new();
    let outcome = tokio::time::timeout(
        Duration::from_secs(3),
        arreo_core::relay::read_envelope(&mut io, &mut buf),
    )
    .await
    .expect("the relay does not hang on an impossible length");
    assert!(
        outcome.is_err(),
        "an oversized envelope must end the session: {outcome:?}"
    );
}

// ---- helpers -----------------------------------------------------------------

/// Does `haystack` contain `needle`?
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Run the handshake by hand so a test can send a *wrong* proof.
///
/// The honest path is [`RelayClient`]; this is the deliberate-misuse path, and
/// it is what makes the refusals above real rather than simulated.
async fn handshake_with(
    relay: &Relay,
    account_id: &str,
    cert: &DeviceCert,
    sign: impl FnOnce(&[u8]) -> Vec<u8>,
) -> Result<DeviceId, RelayError> {
    let endpoint = client_endpoint().map_err(|e| RelayError::Transport(e.to_string()))?;
    let connection = endpoint
        .connect(relay.addr, SERVER_NAME)
        .map_err(|e| RelayError::Transport(e.to_string()))?
        .await
        .map_err(|e| RelayError::Transport(e.to_string()))?;
    let (mut send, mut recv) = connection
        .open_bi()
        .await
        .map_err(|e| RelayError::Transport(e.to_string()))?;

    let hello = Hello {
        v: RELAY_VERSION,
        account_id: account_id.to_string(),
        device_id: cert.device().display_id(),
    };
    write_frame(&mut send, &encode_message(&hello)?).await?;
    let mut buf = Vec::new();
    let body = read_frame(&mut recv, &mut buf, 16 * 1024).await?;
    let nonce = match decode_message::<HelloReply>(&body)? {
        HelloReply::Challenge { nonce, .. } => nonce,
        HelloReply::Refused { reason, .. } => {
            return Err(RelayError::Refused { reason });
        }
    };
    let auth = Auth {
        v: RELAY_VERSION,
        cert: cert
            .encode()
            .map_err(|e| RelayError::Frame(e.to_string()))?,
        signature: sign(&nonce),
    };
    write_frame(&mut send, &encode_message(&auth)?).await?;
    let body = read_frame(&mut recv, &mut buf, 16 * 1024).await?;
    match decode_message::<AuthReply>(&body)? {
        AuthReply::Welcome { device_id, .. } => {
            DeviceId::parse(&device_id).map_err(|e| RelayError::Cert(e.to_string()))
        }
        AuthReply::Refused { reason, .. } => Err(RelayError::Refused { reason }),
    }
}

/// Build a raw envelope to write into an established session, bypassing
/// [`RelayClient`] so a test can forge what the client would never send.
fn forged_envelope(account_id: &str, src: &str, dst: &str, payload: &[u8]) -> Vec<u8> {
    use arreo_core::relay::{RelayEnvelope, RelayHeader, RelayKind};
    RelayEnvelope {
        header: RelayHeader {
            v: RELAY_VERSION,
            account_id: account_id.to_string(),
            src_device: src.to_string(),
            dst: dst.to_string(),
            seq: 99,
            kind: RelayKind::Frame,
        },
        payload: payload.to_vec(),
    }
    .encode()
    .expect("encode")
}

/// Complete the handshake by hand and hand back the raw stream, so a test can
/// write frames [`RelayClient`] would never produce.
async fn manual_session(
    relay: &Relay,
    account_id: &str,
    device_key: &DeviceKey,
    cert: &DeviceCert,
) -> Result<impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin, RelayError> {
    let endpoint = client_endpoint().map_err(|e| RelayError::Transport(e.to_string()))?;
    let connection = endpoint
        .connect(relay.addr, SERVER_NAME)
        .map_err(|e| RelayError::Transport(e.to_string()))?
        .await
        .map_err(|e| RelayError::Transport(e.to_string()))?;
    let (send, recv) = connection
        .open_bi()
        .await
        .map_err(|e| RelayError::Transport(e.to_string()))?;
    let mut io = tokio::io::join(recv, send);

    let hello = Hello {
        v: RELAY_VERSION,
        account_id: account_id.to_string(),
        device_id: cert.device().display_id(),
    };
    write_frame(&mut io, &encode_message(&hello)?).await?;
    let mut buf = Vec::new();
    let body = read_frame(&mut io, &mut buf, 16 * 1024).await?;
    let nonce = match decode_message::<HelloReply>(&body)? {
        HelloReply::Challenge { nonce, .. } => nonce,
        HelloReply::Refused { reason, .. } => return Err(RelayError::Refused { reason }),
    };
    let auth = Auth {
        v: RELAY_VERSION,
        cert: cert
            .encode()
            .map_err(|e| RelayError::Frame(e.to_string()))?,
        // Canonical id, not the announced spelling (see the protocol's §3.6).
        signature: device_key
            .sign(&arreo_core::relay::proof_payload(
                &nonce,
                account_id,
                cert.device().as_str(),
            ))
            .to_bytes()
            .to_vec(),
    };
    write_frame(&mut io, &encode_message(&auth)?).await?;
    let body = read_frame(&mut io, &mut buf, 16 * 1024).await?;
    match decode_message::<AuthReply>(&body)? {
        AuthReply::Welcome { .. } => Ok(io),
        AuthReply::Refused { reason, .. } => Err(RelayError::Refused { reason }),
    }
}

/// Read one envelope and return the outcome it carries.
async fn read_outcome<S>(io: &mut S, buf: &mut Vec<u8>) -> Outcome
where
    S: tokio::io::AsyncRead + Unpin,
{
    // `read_envelope`, not `read_frame`: the envelope carries its own length
    // prefix, so the generic frame reader would strip it and hand `decode` a
    // body it reads as a size.
    let envelope = arreo_core::relay::read_envelope(io, buf)
        .await
        .expect("the relay answers");
    arreo_core::relay::decode_payload(&envelope.payload).expect("an outcome")
}
