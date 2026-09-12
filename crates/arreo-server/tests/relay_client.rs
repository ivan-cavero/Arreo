//! T-0050 acceptance tests: an encrypted byte stream between two devices
//! through a real relay.
//!
//! Nothing is mocked. A real `arreo-relay` process serves a real QUIC listener
//! with a real SQLite state directory; two real relay sessions authenticate to
//! it with real device certificates; and T-0023's Noise channel runs over the
//! stream this task built, so what the relay handles is ciphertext.
//!
//! The relay is exercised as a *binary* rather than a linked library on purpose:
//! that keeps the AGPL `arreo-relay` crate out of this crate's dependency graph
//! (§7/T-0035).

use arreo_core::identity::{DeviceCert, DeviceId, DeviceKey, Role, RootKey, VerifyingKey};
use arreo_core::transport::noise::{FlightGuard, SecureChannel};
use arreo_server::relay_client::{
    backoff_delay, retry_delay, RelaySession, BACKOFF_BASE, BACKOFF_CEILING,
};
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The relay binary, built into the same target directory as this test's own
/// binaries. Missing means the workspace was not built: a loud failure, because
/// a silently skipped security test is worse than a red one.
fn relay_binary() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_BIN_EXE_arreo-server"))
        .parent()
        .expect("target dir")
        .to_path_buf();
    let relay = dir.join("arreo-relay");
    assert!(
        relay.exists(),
        "{} is missing — run `cargo build -p arreo-relay` (or `cargo test --workspace`, \
         which builds every binary) before this test",
        relay.display()
    );
    relay
}

/// A running relay, its state directory, and everything it has logged.
struct Relay {
    child: Child,
    addr: SocketAddr,
    state_dir: PathBuf,
    log: Arc<Mutex<String>>,
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
        let state_dir = std::env::temp_dir().join(format!(
            "arreo-relay-client-{tag}-{}-{:?}",
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

        let stderr = child.stderr.take().expect("stderr is piped");
        let log = Arc::new(Mutex::new(String::new()));
        let (ready_tx, ready_rx) = mpsc::channel();
        {
            let log = Arc::clone(&log);
            std::thread::spawn(move || {
                // Keep reading for the process's whole life: dropping the pipe
                // would make the relay die on its next log line (EPIPE).
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if let Some(rest) = line.split("router on ").nth(1) {
                        if let Some(addr) = rest.split_whitespace().next() {
                            if let Ok(addr) = addr.parse::<SocketAddr>() {
                                let _ = ready_tx.send(addr);
                            }
                        }
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
                &root
                    .to_bytes()
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
            ])
            .output()
            .expect("the account command runs");
        assert!(
            output.status.success(),
            "registering an account failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Every file the relay keeps, for the opacity scan.
    fn state_bytes(&self) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.state_dir).expect("state dir") {
            let path = entry.expect("entry").path();
            if path.is_file() {
                out.push((path.clone(), std::fs::read(&path).expect("read")));
            }
        }
        out
    }
}

/// A device with a certificate issued by `root`.
fn device(root: &RootKey, name: &str, serial: u64) -> (DeviceKey, DeviceCert) {
    let key = DeviceKey::generate().expect("entropy");
    let cert = DeviceCert::issue(root, &key.public(), name, Role::Owner, 1_000, serial);
    (key, cert)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// **A refusal does not get the quick first retry** (T-0069).
///
/// The transport schedule starts at 250 ms so a momentary blip is invisible —
/// and that is exactly wrong for a registration the relay *answered and refused*,
/// because the reason (no such account, a certificate this account does not
/// accept) cannot change in the next quarter second. Retrying that fast is what
/// exhausts the relay's per-address handshake budget, and an exhausted budget
/// replaces the reason with a transport error: the operator loses the sentence
/// that would have told them what to fix.
#[test]
fn a_refused_registration_waits_at_the_ceiling_not_on_the_ramp() {
    // The property that matters, at the attempt that matters: the *first* retry
    // after a refusal already waits the ceiling.
    assert_eq!(
        retry_delay(true, 0, 0.0),
        BACKOFF_CEILING,
        "a refusal must not get the 250 ms first retry"
    );
    // Far above the ramp's start, and still jittered so a fleet refused for the
    // same reason does not attempt in lockstep.
    assert_eq!(retry_delay(true, 5, 0.0), BACKOFF_CEILING);
    let quiet = retry_delay(true, 0, 0.0);
    let loud = retry_delay(true, 0, 1.0);
    assert!(loud > quiet && loud <= quiet.mul_f64(1.25), "{loud:?}");

    // A transport failure keeps the ramp: recovery from a blip must stay quick.
    assert_eq!(retry_delay(false, 0, 0.0), BACKOFF_BASE);
    assert_eq!(retry_delay(false, 3, 0.0), backoff_delay(3, 0.0));
}

/// The headline criterion: two devices hold an encrypted byte stream through the
/// relay, and the relay never sees the plaintext.
#[tokio::test]
async fn two_devices_hold_a_noise_session_through_the_relay() {
    let relay = Relay::start("noise");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());

    let (alice_key, alice_cert) = device(&root, "alice", 1);
    let (bob_key, bob_cert) = device(&root, "bob", 2);
    let alice_id = alice_cert.device().clone();
    let bob_id = bob_cert.device().clone();

    // Both devices dial the relay: the destination must be known to the relay
    // before an envelope addressed to it will be accepted.
    let alice_session = RelaySession::dial(relay.addr, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice registers");
    let mut bob_session = RelaySession::dial(relay.addr, "acct-1", &bob_key, &bob_cert)
        .await
        .expect("bob registers");

    // Alice initiates: her Noise handshake starts the moment she writes.
    let alice_stream = alice_session.stream_to(&bob_id);
    let alice_static = alice_key.noise_static();
    let bob_public = bob_key.public();
    // `connect`'s third argument is the id *we* announce — Alice announces
    // herself, and pins Bob's key. Getting this backwards is how a handshake
    // fails with `UnknownPeer` while looking like a key problem.
    let announced = alice_id.display_id();
    let initiator = tokio::spawn(async move {
        SecureChannel::connect(alice_stream, &alice_static, &announced, &bob_public).await
    });

    // Bob learns a peer is talking to him, opens the matching stream, and
    // completes the handshake as the responder.
    let peer = tokio::time::timeout(Duration::from_secs(10), bob_session.next_peer())
        .await
        .expect("bob is told about the peer")
        .expect("a peer arrived");
    assert_eq!(peer, alice_id, "the announced peer is alice");

    let bob_stream = bob_session.stream_to(&peer);
    let bob_static = bob_key.noise_static();
    let alice_public = alice_key.public();
    let guard = FlightGuard::default();
    let expected = alice_id.clone();
    let responder = SecureChannel::accept(bob_stream, &bob_static, &guard, move |hint| {
        (*hint == expected).then_some(alice_public)
    });

    let (mut alice, (mut bob, learned)) = tokio::join!(
        async {
            tokio::time::timeout(Duration::from_secs(20), initiator)
                .await
                .expect("alice's handshake completes")
                .expect("alice's task")
                .expect("alice's handshake")
        },
        async {
            tokio::time::timeout(Duration::from_secs(20), responder)
                .await
                .expect("bob's handshake completes")
                .expect("bob's handshake")
        }
    );
    // The responder learns which device it authenticated — the id the daemon
    // needs for its audit row and its per-verb policy.
    assert_eq!(learned, alice_id, "bob authenticated alice");

    // Each end authenticated the *other's* pinned key: that is the property the
    // relay in the middle cannot fake.
    assert_eq!(alice.remote_static(), bob_key.noise_static().public());
    assert_eq!(bob.remote_static(), alice_key.noise_static().public());

    // Data both ways, with a marker the relay must never hold.
    let marker = "ARREO-RELAY-PLAINTEXT-MARKER-7d21";
    let message = format!("\x1b[32magent\x1b[0m $ {marker}\n> waiting\n").into_bytes();
    alice.write_all(&message).await.expect("alice writes");
    alice.flush().await.expect("flush");
    let mut got = vec![0u8; message.len()];
    tokio::time::timeout(Duration::from_secs(10), bob.read_exact(&mut got))
        .await
        .expect("bob reads within the timeout")
        .expect("bob reads");
    assert_eq!(got, message, "the payload survives byte-identical");

    // ...and back the other way, so the stream is proven bidirectional.
    let reply = b"pong-through-the-relay".to_vec();
    bob.write_all(&reply).await.expect("bob writes");
    bob.flush().await.expect("flush");
    let mut back = vec![0u8; reply.len()];
    tokio::time::timeout(Duration::from_secs(10), alice.read_exact(&mut back))
        .await
        .expect("alice reads within the timeout")
        .expect("alice reads");
    assert_eq!(back, reply);

    // A payload larger than one envelope exercises the chunking: the stream is a
    // byte stream, so a write of any size must arrive whole, in order, with the
    // envelope boundaries invisible above it.
    let large: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
    let expected_large = large.clone();
    alice.write_all(&large).await.expect("a large write");
    alice.flush().await.expect("flush");
    let mut received = vec![0u8; expected_large.len()];
    tokio::time::timeout(Duration::from_secs(20), bob.read_exact(&mut received))
        .await
        .expect("the large payload arrives within the timeout")
        .expect("the large payload arrives");
    assert_eq!(
        received, expected_large,
        "chunking must be invisible above the stream"
    );

    // A zero-length write is a no-op, not an envelope: the stream stays healthy.
    alice.write_all(b"").await.expect("an empty write");
    alice.flush().await.expect("flush");
    alice
        .write_all(b"still here")
        .await
        .expect("write after empty");
    alice.flush().await.expect("flush");
    let mut tail = vec![0u8; b"still here".len()];
    tokio::time::timeout(Duration::from_secs(10), bob.read_exact(&mut tail))
        .await
        .expect("the stream survives an empty write")
        .expect("read");
    assert_eq!(tail, b"still here");

    // The relay carried ciphertext: the marker is nowhere in its state or logs.
    for (path, bytes) in relay.state_bytes() {
        assert!(
            !contains(&bytes, marker.as_bytes()),
            "the plaintext reached {} — the relay must not hold what it routes",
            path.display()
        );
    }
    let log = relay.log_text();
    assert!(
        !log.contains(marker),
        "the plaintext reached the relay's log:\n{log}"
    );
    // The log is not empty, so the assertion above is not vacuous.
    assert!(
        log.contains("authenticated"),
        "the relay logs its sessions:\n{log}"
    );
}

/// A refusal from the relay is surfaced with the relay's own reason, and is not
/// something a caller should retry in a tight loop.
#[tokio::test]
async fn a_refused_registration_carries_the_relays_reason() {
    let relay = Relay::start("refused");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let (key, cert) = device(&root, "alice", 1);

    let refused = RelaySession::dial(relay.addr, "acct-not-registered", &key, &cert).await;
    match refused {
        Err(arreo_server::SessionError::Client(arreo_core::relay::ClientError::Refused {
            reason,
        })) => assert!(
            reason.contains("unknown account"),
            "the relay's own reason must reach the caller: {reason}"
        ),
        other => panic!("an unknown account must be refused: {other:?}"),
    }
}

/// A stream whose bytes the relay could not deliver fails loudly instead of
/// silently losing a chunk.
#[tokio::test]
async fn a_delivery_failure_ends_the_stream() {
    let relay = Relay::start("undeliverable");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let (alice_key, alice_cert) = device(&root, "alice", 1);
    let session = RelaySession::dial(relay.addr, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice registers");

    // A destination the relay has never seen: the envelope cannot be delivered
    // and cannot be queued, so the relay says so.
    let stranger = DeviceKey::generate().expect("entropy");
    let stranger_id = DeviceId::from_key(&stranger.public());
    let mut stream = session.stream_to(&stranger_id);

    stream
        .write_all(b"into the void")
        .await
        .expect("buffered write");
    stream.flush().await.expect("flush");

    let mut buf = [0u8; 16];
    let outcome = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buf))
        .await
        .expect("the failure arrives promptly");
    let error = outcome.expect_err("a stream whose bytes were not delivered must fail");
    assert_eq!(
        error.kind(),
        std::io::ErrorKind::ConnectionAborted,
        "{error}"
    );
    let text = error.to_string();
    assert!(
        text.contains("does not know that device") || text.contains("offline"),
        "the failure must say why: {text}"
    );
}

/// The reconnect policy is a pure function, so its shape is asserted directly
/// rather than by sleeping.
#[test]
fn the_backoff_is_exponential_capped_and_jittered() {
    // Without jitter, exact doubling from the base.
    assert_eq!(backoff_delay(0, 0.0), BACKOFF_BASE);
    assert_eq!(backoff_delay(1, 0.0), BACKOFF_BASE * 2);
    assert_eq!(backoff_delay(2, 0.0), BACKOFF_BASE * 4);
    assert_eq!(backoff_delay(3, 0.0), BACKOFF_BASE * 8);

    // It is capped: a relay that is down for an hour costs one retry per
    // ceiling, not a spin.
    assert_eq!(backoff_delay(20, 0.0), BACKOFF_CEILING);
    assert_eq!(backoff_delay(64, 0.0), BACKOFF_CEILING);
    assert!(backoff_delay(64, 1.0) <= BACKOFF_CEILING.mul_f64(1.25));

    // Jitter spreads a fleet: the same attempt never waits exactly the same
    // time, and the spread is bounded so a caller can still reason about it.
    let quiet = backoff_delay(4, 0.0);
    let loud = backoff_delay(4, 1.0);
    assert!(loud > quiet, "jitter must add time: {loud:?} vs {quiet:?}");
    assert!(
        loud <= quiet.mul_f64(1.25),
        "jitter must stay bounded: {loud:?} vs {quiet:?}"
    );
    // And the very first retry is quick, so a momentary blip is invisible.
    assert!(backoff_delay(0, 0.0) <= Duration::from_millis(500));
}

/// A session whose relay goes away reports that it closed, so a caller can
/// reconnect instead of writing into a dead connection.
#[tokio::test]
async fn a_session_reports_when_its_relay_disappears() {
    let mut relay = Relay::start("gone");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());
    let (key, cert) = device(&root, "alice", 1);
    let session = RelaySession::dial(relay.addr, "acct-1", &key, &cert)
        .await
        .expect("alice registers");

    let _ = relay.child.kill();
    let _ = relay.child.wait();

    // Bounded, and bounded *quickly*: the client's idle timeout is 15 s, so a
    // relay that simply vanished is noticed in seconds rather than half a
    // minute — which is what lets a reconnect loop start promptly.
    let start = std::time::Instant::now();
    tokio::time::timeout(Duration::from_secs(30), session.closed())
        .await
        .expect("the session notices its relay is gone within a bounded time");
    assert!(
        start.elapsed() < Duration::from_secs(25),
        "a vanished relay must be noticed promptly, took {:?}",
        start.elapsed()
    );
}

/// A device that leaves ends the streams its peers hold for it (T-0054).
///
/// The relay does not track who holds a stream to whom, so the notice is
/// account-wide: every other live device hears that the departed one is gone, and
/// a receiver with no stream for it ignores the news. The behaviour that matters
/// is what the *holder* observes — its stream to the departed peer ends with a
/// reason, rather than accepting writes that can never be delivered or waiting
/// for a read to fail on its own. That wait is what made a reconnect take over a
/// minute before this landed.
#[tokio::test]
async fn a_departing_device_ends_the_streams_its_peers_hold() {
    let relay = Relay::start("peergone");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());

    let (alice_key, alice_cert) = device(&root, "alice", 1);
    let (bob_key, bob_cert) = device(&root, "bob", 2);
    let bob_id = bob_cert.device().clone();

    let alice_session = RelaySession::dial(relay.addr, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice registers");
    let bob_session = RelaySession::dial(relay.addr, "acct-1", &bob_key, &bob_cert)
        .await
        .expect("bob registers");

    // Alice holds a stream to Bob and has written into it, so the stream is
    // established in both directions rather than merely allocated.
    let mut alice_stream = alice_session.stream_to(&bob_id);
    alice_stream.write_all(b"before").await.expect("write");
    alice_stream.flush().await.expect("flush");
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // Bob leaves the way a crash does: the session is dropped, which closes the
    // connection — and is why `RelaySession` aborts its pumps on drop.
    drop(bob_session);

    // Alice's stream ends, promptly and with a reason. A read is what observes
    // it: the relay's notice reaches the session's reader, which ends the stream
    // for that peer.
    let mut buf = [0u8; 16];
    let outcome = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        alice_stream.read(&mut buf),
    )
    .await
    .expect("the departure ends the stream instead of leaving it hanging");
    match outcome {
        // End of stream, or the typed error the session recorded. Either is an
        // end the layer above can act on; what must not happen is a read that
        // waits forever, or a write that appears to succeed.
        Ok(0) => {}
        Ok(n) => panic!("read {n} bytes from a stream whose peer left: {buf:?}"),
        Err(e) => {
            let text = e.to_string();
            assert!(
                text.contains("offline") || text.contains("closed") || text.contains("ended"),
                "the end must say why: {text}"
            );
        }
    }

    // The point of the notice: the peer comes back and is reachable at once. A
    // fresh stream to the same device id carries data again, which is what makes
    // a reconnect a reconnect rather than a wait. (A write into the *old* stream
    // is not asserted to fail: the relay queues for an offline device by design
    // (T-0030), so bytes written after the departure are durably queued rather
    // than lost — the stream ending is about the reader learning promptly, which
    // is the assertion above.)
    let mut bob_again = RelaySession::dial(relay.addr, "acct-1", &bob_key, &bob_cert)
        .await
        .expect("bob re-registers");

    let mut fresh = alice_session.stream_to(&bob_id);
    fresh.write_all(b"after").await.expect("write");
    fresh.flush().await.expect("flush");

    let arrival = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        let peer = bob_again.next_peer().await;
        peer
    })
    .await
    .expect("the re-registered peer is reachable again");
    let alice_id = alice_cert.device().clone();
    assert_eq!(
        arrival,
        Some(alice_id.clone()),
        "the arriving peer is alice"
    );

    let mut bob_stream = bob_again.stream_to(&alice_id);
    let mut buf = [0u8; 8];
    let read = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        bob_stream.read(&mut buf),
    )
    .await
    .expect("the bytes arrive")
    .expect("read");
    assert_eq!(
        &buf[..read],
        b"after",
        "the fresh stream carries data again"
    );
}

/// **T-0065: a frame written immediately before closing must reach the peer.**
///
/// This is the shape every "refuse and hang up" path has: the daemon writes its
/// answer (a refusal, an error, a final status), shuts the stream down, and
/// returns. Over a local socket the bytes are in a duplex the same process owns
/// and always arrive — which is why T-0046's tests, all local, never caught this.
/// Over the relay the write direction is a `DuplexStream` plus **one forwarding
/// task**, and that task is what actually hands the bytes to the session.
///
/// The assertion is the whole point: the peer must read the frame. A stream that
/// is dropped before its forwarding task has drained loses whatever was written
/// last — and "whatever was written last" is, on every refusal path, the reason.
#[tokio::test]
async fn a_frame_written_immediately_before_closing_reaches_the_peer() {
    let relay = Relay::start("final-frame");
    let root = RootKey::generate().expect("entropy");
    relay.register_account("acct-1", &root.public());

    let (alice_key, alice_cert) = device(&root, "alice", 1);
    let (bob_key, bob_cert) = device(&root, "bob", 2);
    let alice_id = alice_cert.device().clone();
    let bob_id = bob_cert.device().clone();

    let alice_session = RelaySession::dial(relay.addr, "acct-1", &alice_key, &alice_cert)
        .await
        .expect("alice registers");
    let mut bob_session = RelaySession::dial(relay.addr, "acct-1", &bob_key, &bob_cert)
        .await
        .expect("bob registers");

    // Alice's side: write the frame, shut the stream down, and drop it — the
    // exact sequence a daemon performs when it refuses a peer.
    const FINAL: &[u8] = b"the reason you were refused";
    let alice_stream = alice_session.stream_to(&bob_id);
    let writer = tokio::spawn(async move {
        let mut stream = alice_stream;
        stream.write_all(FINAL).await.expect("write");
        stream.flush().await.expect("flush");
        // `shutdown` then drop: what `serve_session`'s wrapper does after the
        // grace sleep.
        AsyncWriteExt::shutdown(&mut stream).await.ok();
        drop(stream);
    });

    // Bob's side: open the matching stream and read what arrives.
    let peer = tokio::time::timeout(Duration::from_secs(10), bob_session.next_peer())
        .await
        .expect("bob is told about the peer")
        .expect("a peer arrived");
    assert_eq!(peer, alice_id, "the announced peer is alice");

    let mut bob_stream = bob_session.stream_to(&peer);
    let mut got = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(10), async {
        let mut buf = [0u8; 256];
        loop {
            match bob_stream.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    got.extend_from_slice(&buf[..n]);
                    if got.len() >= FINAL.len() {
                        break;
                    }
                }
            }
        }
    })
    .await;
    assert!(
        read.is_ok(),
        "the final frame must arrive within the timeout, got {} byte(s)",
        got.len()
    );
    assert_eq!(
        got, FINAL,
        "the frame written immediately before closing must reach the peer"
    );
    writer.await.expect("the writer task finishes");
}
