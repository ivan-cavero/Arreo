//! T-0104's contract test: pairing and a session driven **through the exported
//! FFI surface**.
//!
//! One sentence: everything below calls the items `#[uniffi::export]` marks —
//! `arreo_core_ffi::pairing::*`, `::identity::*`, `::relay::*`, `::codec::*`,
//! `::theme::*` — exactly as the generated Swift and Kotlin call them, so a
//! boundary that cannot actually be crossed fails here rather than in an Xcode
//! build.
//!
//! ## Why `arreo-core` appears at all, and why that is not cheating
//!
//! The test needs two *peers* to talk to: a mailbox and a relay. Those are
//! servers, they live in `arreo-relay` (AGPL, which this Apache crate may not
//! link), and a phone has neither. So the test stands up its own, and it does so
//! **out of the shipped wire vocabulary** — `MailboxRequest`/`MailboxResponse`
//! for the mailbox, and `Hello`/`Challenge`/`Auth`/`Welcome` plus
//! `verify_auth` and `RelayEnvelope` for the relay. That is the point rather
//! than a shortcut: the peers the FFI surface talks to here are built from the
//! protocol the product defines, so a test that passes proves the exported
//! surface speaks *that* protocol, not a private arrangement with itself.
//!
//! What is never done is calling the client API underneath: nothing below
//! touches `arreo_core::pairing::flow::PairingServer`, `RelaySession::dial` or
//! `arreo_core::proto::codec` directly. The only `arreo_core` imports are the
//! *server-side* types and the raw frame helpers a peer needs.
//!
//! ## The ordering rule the relay test pins
//!
//! A device learns a peer has written to it by waiting in `next_peer()`, and
//! only then opens the stream with `stream_to()`. The reverse order is not a
//! race the test tolerates but a different protocol: `stream_for` replaces the
//! live peer when it is called before the first envelope arrives, and the core's
//! read pump only announces a peer it created itself. `docs/mobile.md` states
//! the rule; `bytes_cross_the_boundary_between_two_devices` is where it is
//! executed.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use arreo_core::mesh::{MachineId, MachineRow, Name, Presence};
use arreo_core::pairing::{MailboxRequest, MailboxResponse, Slot};
use arreo_core::relay::{
    decode_message, encode_message, encode_payload, read_envelope, read_frame, verify_auth,
    write_frame, Auth, AuthReply, DirectoryReply, DrainReport, Hello, HelloReply, Outcome,
    RelayEnvelope, RelayHeader, RelayKind, MAX_HANDSHAKE_BYTES, RELAY_SENDER, RELAY_VERSION,
};

use arreo_core_ffi::codec::{
    codec_decode, codec_encode, codec_encode_frame, codec_protocol_version, WireAgentState,
    WireCursor, WireMessage,
};
use arreo_core_ffi::directory::{directory_cache_new, FfiPresence, MachineRowInfo};
use arreo_core_ffi::errors::{CodecFfiError, PairingFfiError};
use arreo_core_ffi::identity::{
    device_cert_issue, device_key_from_seed, fingerprint_of_public_key, identity_verify, role_word,
    root_key_from_seed, FfiRole,
};
use arreo_core_ffi::pairing::{
    pairing_code_phrase, pairing_code_random, pairing_invite_parse, pairing_invite_uri,
    pairing_phone_join, pairing_server_begin,
};
use arreo_core_ffi::relay::{relay_peer_parse, relay_session_dial};
use arreo_core_ffi::theme::{theme_builtin, FfiColor, FfiDepth, FfiVariant};

// ---------------------------------------------------------------------------
// A mailbox that speaks the shipped pairing protocol.
// ---------------------------------------------------------------------------

/// The four write-once slots, single-use sessions, and a TTL — the rules the
/// relay's mailbox enforces, in the test's own twenty lines.
///
/// A TCP listener rather than a unix socket, and that is the *phone's* shape:
/// `MailboxAddr::parse` accepts `host:port` for a relay across a network, which
/// is what a QR carries and the only address kind a phone can use at all.
struct TestMailbox {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl TestMailbox {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("the mailbox binds");
        let addr = listener.local_addr().expect("a bound address");
        let stop = Arc::new(AtomicBool::new(false));
        let sessions: Arc<Mutex<HashMap<String, Session>>> = Arc::new(Mutex::new(HashMap::new()));
        let handle = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(stream) = stream else { continue };
                    serve_mailbox(stream, &sessions);
                }
            })
        };
        Self {
            addr,
            stop,
            handle: Some(handle),
        }
    }

    fn address(&self) -> String {
        self.addr.to_string()
    }
}

impl Drop for TestMailbox {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Unblock `incoming()` with one connection, then join.
        let _ = TcpStream::connect(self.addr);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

struct Session {
    slots: [Option<Vec<u8>>; 4],
    expires_at: std::time::Instant,
}

/// One request, one reply — the ADR 0007 convention, so a dead mailbox cannot
/// leave a pairing holding a stale socket.
fn serve_mailbox(stream: TcpStream, sessions: &Arc<Mutex<HashMap<String, Session>>>) {
    let mut writer = stream.try_clone().expect("the mailbox stream clones");
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let reply = match serde_json::from_str::<MailboxRequest>(line.trim()) {
        Ok(request) => handle_request(request, sessions),
        Err(e) => MailboxResponse::error(format!("malformed request: {e}")),
    };
    let body = serde_json::to_string(&reply).expect("a reply serializes");
    let _ = writer.write_all(body.as_bytes());
    let _ = writer.write_all(b"\n");
    let _ = writer.flush();
}

fn handle_request(
    request: MailboxRequest,
    sessions: &Arc<Mutex<HashMap<String, Session>>>,
) -> MailboxResponse {
    let mut held = sessions.lock().expect("mailbox state");
    // Sweep, exactly as the relay does: an expired id is remembered as used.
    held.retain(|_, session| session.expires_at > std::time::Instant::now());
    match request {
        MailboxRequest::Open { session, ttl_secs } => {
            if held.contains_key(&session) {
                return MailboxResponse::error("that session id is already live");
            }
            held.insert(
                session,
                Session {
                    slots: [None, None, None, None],
                    expires_at: std::time::Instant::now() + Duration::from_secs(ttl_secs.max(1)),
                },
            );
            MailboxResponse::ok()
        }
        MailboxRequest::Put {
            session,
            slot,
            payload,
        } => {
            let Some(entry) = held.get_mut(&session) else {
                return MailboxResponse::error("no such session");
            };
            let index = match slot {
                Slot::A => 0,
                Slot::B => 1,
                Slot::C => 2,
                Slot::D => 3,
            };
            if entry.slots[index].is_some() {
                return MailboxResponse::error("that slot already holds a flight");
            }
            entry.slots[index] =
                Some(arreo_core::pairing::wire::decode_payload(&payload).unwrap_or_default());
            MailboxResponse::ok()
        }
        MailboxRequest::Get { session, slot } => {
            let Some(entry) = held.get(&session) else {
                return MailboxResponse::error("no such session");
            };
            let index = match slot {
                Slot::A => 0,
                Slot::B => 1,
                Slot::C => 2,
                Slot::D => 3,
            };
            match &entry.slots[index] {
                Some(bytes) => MailboxResponse::with_payload(Some(
                    arreo_core::pairing::wire::encode_payload(bytes),
                )),
                None => MailboxResponse::with_payload(None),
            }
        }
        MailboxRequest::Burn { session } => {
            held.remove(&session);
            MailboxResponse::ok()
        }
        MailboxRequest::Status { session } => match held.get(&session) {
            Some(entry) => {
                let left = entry
                    .expires_at
                    .saturating_duration_since(std::time::Instant::now());
                MailboxResponse::with_ttl(left.as_millis() as u64)
            }
            None => MailboxResponse::error("no such session"),
        },
    }
}

// ---------------------------------------------------------------------------
// A relay that speaks the shipped protocol.
// ---------------------------------------------------------------------------

/// A running relay: a QUIC endpoint, the account's root key, and the routing
/// table from device id to that device's outbound channel.
struct TestRelay {
    addr: SocketAddr,
    routes: Arc<Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
    task: tokio::task::JoinHandle<()>,
}

impl TestRelay {
    /// Bind a relay for `account`, whose devices are certified by `root`.
    ///
    /// The trust decision is the product's own `verify_auth`: the certificate
    /// the FFI surface produced must verify under this root, name the announced
    /// device, and carry a proof of possession over the relay's fresh nonce. A
    /// test that accepted anything would prove nothing about the credentials
    /// crossing the boundary.
    async fn start(
        account: String,
        root: Arc<arreo_core::identity::RootKey>,
        rows: Vec<MachineRow>,
    ) -> Self {
        let endpoint = arreo_core::transport::server_endpoint("127.0.0.1:0".parse().unwrap())
            .expect("the relay binds");
        let addr = endpoint.local_addr().expect("a bound address");
        let routes: Arc<Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let rows = Arc::new(Mutex::new(rows));
        let task = tokio::spawn({
            let routes = Arc::clone(&routes);
            let rows = Arc::clone(&rows);
            async move {
                while let Some(incoming) = endpoint.accept().await {
                    let Ok(connection) = incoming.await else {
                        continue;
                    };
                    let routes = Arc::clone(&routes);
                    let rows = Arc::clone(&rows);
                    let root = Arc::clone(&root);
                    let account = account.clone();
                    tokio::spawn(async move {
                        serve_relay_connection(connection, account, root, routes, rows).await;
                    });
                }
            }
        });
        Self { addr, routes, task }
    }

    fn address(&self) -> String {
        self.addr.to_string()
    }

    /// Wait until `device` has a live route, so a test can send without racing
    /// the handshake it just awaited.
    async fn await_route(&self, device: &str) {
        for _ in 0..200 {
            if self.routes.lock().expect("routes").contains_key(device) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{device} never registered a route with the relay");
    }
}

impl Drop for TestRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The relay's half of one connection: Hello → Challenge → Auth → Welcome, then
/// route envelopes until the peer goes away.
async fn serve_relay_connection(
    connection: arreo_core::transport::Connection,
    account: String,
    root: Arc<arreo_core::identity::RootKey>,
    routes: Arc<Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
    rows: Arc<Mutex<Vec<MachineRow>>>,
) {
    let Ok((mut send, mut recv)) = connection.accept_bi().await else {
        return;
    };
    let mut buf = Vec::new();

    let Ok(body) = read_frame(&mut recv, &mut buf, MAX_HANDSHAKE_BYTES).await else {
        return;
    };
    let Ok(hello) = decode_message::<Hello>(&body) else {
        return;
    };
    if hello.account_id != account {
        let _ = write_frame(
            &mut send,
            &encode_message(&HelloReply::Refused {
                v: RELAY_VERSION,
                reason: "unknown account".to_string(),
            })
            .unwrap_or_default(),
        )
        .await;
        return;
    }

    let nonce = arreo_core::relay::fresh_nonce().expect("entropy");
    let challenge = HelloReply::Challenge {
        v: RELAY_VERSION,
        nonce: nonce.to_vec(),
    };
    if write_frame(&mut send, &encode_message(&challenge).expect("encode"))
        .await
        .is_err()
    {
        return;
    }

    let Ok(body) = read_frame(&mut recv, &mut buf, MAX_HANDSHAKE_BYTES).await else {
        return;
    };
    let Ok(auth) = decode_message::<Auth>(&body) else {
        return;
    };
    let device = match verify_auth(
        &hello.account_id,
        &hello.device_id,
        &root.public(),
        &nonce,
        &auth,
    ) {
        Ok(device) => device,
        Err(e) => {
            let _ = write_frame(
                &mut send,
                &encode_message(&AuthReply::Refused {
                    v: RELAY_VERSION,
                    reason: e.to_string(),
                })
                .unwrap_or_default(),
            )
            .await;
            return;
        }
    };
    let key = device.device_id.as_str().to_string();
    let welcome = AuthReply::Welcome {
        v: RELAY_VERSION,
        account_id: hello.account_id.clone(),
        device_id: device.device_id.display_id(),
    };
    if write_frame(&mut send, &encode_message(&welcome).expect("encode"))
        .await
        .is_err()
    {
        return;
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    routes.lock().expect("routes").insert(key.clone(), tx);
    let writer = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if write_frame(&mut send, &frame).await.is_err() {
                break;
            }
        }
    });

    loop {
        let Ok(envelope) = read_envelope(&mut recv, &mut buf).await else {
            break;
        };
        let dst = envelope.header.dst.clone();
        let reply = match envelope.header.kind {
            RelayKind::Frame => {
                // Route to the destination, or report it offline — the relay
                // never decodes the payload, and neither does this.
                let target = arreo_core::identity::DeviceId::parse(&dst)
                    .ok()
                    .and_then(|id| routes.lock().expect("routes").get(id.as_str()).cloned());
                match target {
                    Some(target) => {
                        if let Ok(frame) = envelope.encode() {
                            let _ = target.send(frame);
                        }
                        None
                    }
                    None => Some((
                        RelayKind::Status,
                        encode_payload(&Outcome::Offline).unwrap_or_default(),
                    )),
                }
            }
            // A drain is answered with a report that says exactly what it did,
            // zeroes included: the core's read pump classifies a Status payload
            // as a drain report only when `next_seq` is non-zero.
            RelayKind::Drain => Some((
                RelayKind::Status,
                encode_payload(&DrainReport {
                    v: RELAY_VERSION,
                    delivered: 2,
                    dropped: 0,
                    expired: 0,
                    queued: 0,
                    next_seq: 3,
                })
                .unwrap_or_default(),
            )),
            RelayKind::Ack => None,
            RelayKind::Machines => {
                let reply = DirectoryReply {
                    v: RELAY_VERSION,
                    seq: envelope.header.seq,
                    granted: None,
                    machines: rows.lock().expect("rows").clone(),
                    refused: None,
                };
                Some((
                    RelayKind::Directory,
                    encode_payload(&reply).unwrap_or_default(),
                ))
            }
            _ => None,
        };
        if let Some((kind, payload)) = reply {
            let frame = RelayEnvelope {
                header: RelayHeader {
                    v: RELAY_VERSION,
                    account_id: envelope.header.account_id.clone(),
                    src_device: RELAY_SENDER.to_string(),
                    dst: envelope.header.src_device.clone(),
                    seq: envelope.header.seq,
                    kind,
                },
                payload,
            };
            if let Ok(frame) = frame.encode() {
                if let Some(target) = routes.lock().expect("routes").get(&key) {
                    let _ = target.send(frame);
                }
            }
        }
    }
    routes.lock().expect("routes").remove(&key);
    writer.abort();
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const ACCOUNT: &str = "acct-contract-test";
const SERVER_SEED: [u8; 32] = [0x11; 32];
const PHONE_SEED: [u8; 32] = [0x22; 32];
const OTHER_DEVICE_SEED: [u8; 32] = [0x33; 32];

fn row_for(seed: &[u8; 32], name: &str, presence: Presence) -> MachineRow {
    let key = device_key_from_seed(seed.to_vec()).expect("a key from a 32-byte seed");
    let verifying = arreo_core::identity::verifying_key_from_hex(&key.public_hex()).expect("hex");
    MachineRow {
        machine_id: MachineId::from_key(&verifying),
        name: Name::parse(name).expect("a valid name"),
        name_conflict: false,
        presence,
        last_seen_ms: 1_760_000_000_000,
        proto_version: codec_protocol_version(),
        tombstone_until_ms: None,
        daemon_key: Some(key.public_hex()),
    }
}

// ---------------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------------

/// Pairing, both sides, entirely through the exported surface.
#[test]
fn pairing_crosses_the_boundary_both_ways() {
    let mailbox = TestMailbox::start();
    let root = root_key_from_seed(SERVER_SEED.to_vec()).expect("a root from a seed");
    let root_hex = root.public_hex();

    let server = pairing_server_begin(Arc::clone(&root), mailbox.address(), 30, None, None)
        .expect("the admitting side begins");
    let invite = server.invite().expect("an invite");
    let uri = pairing_invite_uri(invite.clone()).expect("an invite URI");
    let code = server.code().expect("a code");

    // The invite survives the round trip a QR forces: render → parse → the same
    // fields, including the pin.
    let reparsed = pairing_invite_parse(uri.clone()).expect("the URI parses back");
    assert_eq!(reparsed, invite);
    assert_eq!(reparsed.server_key, root_hex);
    assert_eq!(reparsed.mailbox, mailbox.address());
    assert!(
        !uri.contains(&code),
        "the code is typed by a human and must not travel in the URI"
    );

    // The phone half runs on its own thread: both `receive` and `await_cert` are
    // blocking, which is the property `docs/mobile.md` warns a UI about.
    let server_thread = {
        std::thread::spawn(move || {
            let request = server.receive().expect("the phone's hello verifies");
            let cert = device_cert_issue(
                root,
                request.public_key.clone(),
                request.name.clone(),
                FfiRole::Owner,
                1_760_000_000_000,
                1,
            )
            .expect("the certificate issues");
            let cert_hex = cert.fingerprint();
            server.complete(cert).expect("the certificate goes back");
            (request, cert_hex)
        })
    };

    let phone = pairing_phone_join(uri, code, PHONE_SEED.to_vec(), "pixel-7".to_string())
        .expect("the phone joins");
    let paired = phone
        .await_cert()
        .expect("the certificate arrives and verifies");

    let (request, cert_fingerprint) = server_thread.join().expect("the admitting side finishes");
    assert_eq!(request.name, "pixel-7");
    assert_eq!(paired.name, "pixel-7");
    assert_eq!(paired.role, FfiRole::Owner);
    assert_eq!(role_word(paired.role), "owner");

    // The identity is one fact in three spellings, and the boundary keeps them
    // consistent: the phone's key, the certificate, and the fingerprint.
    let phone_key = device_key_from_seed(PHONE_SEED.to_vec()).expect("the phone's key");
    assert_eq!(paired.fingerprint, phone_key.fingerprint());
    assert_eq!(paired.device_id, format!("dev_{}", paired.fingerprint));
    assert_eq!(paired.device_id, format!("dev_{cert_fingerprint}"));
    assert_eq!(request.public_key, phone_key.public_hex());
    assert_eq!(
        fingerprint_of_public_key(phone_key.public_hex()).expect("the fingerprint derives"),
        paired.fingerprint
    );

    // The certificate verifies through the exported surface, against the pinned
    // root and the presented key — and *not* against a different device's.
    paired
        .cert
        .verify(root_hex.clone(), phone_key.public_hex())
        .expect("the certificate verifies for the phone");
    let other = device_key_from_seed(OTHER_DEVICE_SEED.to_vec()).expect("another key");
    assert!(
        paired.cert.verify(root_hex, other.public_hex()).is_err(),
        "a certificate must not authorize a key it was not issued for"
    );

    // The signature the phone made over the handshake proof verifies.
    let payload = b"a payload the phone signed".to_vec();
    let signature = phone_key.sign(payload.clone());
    assert!(
        identity_verify(phone_key.public_hex(), payload.clone(), signature.clone())
            .expect("a real key"),
        "the phone's own signature verifies"
    );
    assert!(
        !identity_verify(phone_key.public_hex(), b"other".to_vec(), signature).expect("a real key"),
        "a signature over different bytes does not"
    );

    // The certificate's bytes are the certificate: what a phone writes to its
    // keystore decodes back to the same device.
    let encoded = paired.cert.encode().expect("the certificate encodes");
    let decoded = arreo_core_ffi::identity::device_cert_decode(encoded).expect("and decodes again");
    assert_eq!(decoded.device_id(), paired.device_id);
    assert_eq!(decoded.fingerprint(), paired.fingerprint);
}

/// A wrong code is refused as a wrong code — the sentence the CLI prints, not a
/// hang and not "mailbox unreachable".
#[test]
fn a_wrong_code_is_refused_with_the_cli_sentence() {
    let mailbox = TestMailbox::start();
    let root = root_key_from_seed(SERVER_SEED.to_vec()).expect("a root from a seed");
    let server =
        pairing_server_begin(Arc::clone(&root), mailbox.address(), 30, None, None).expect("begin");
    let uri = pairing_invite_uri(server.invite().expect("an invite")).expect("a URI");

    // A different *valid* code: the exchange runs to completion and the
    // confirmation MAC is what fails, which is the case the sentence is for.
    let wrong = pairing_code_random().expect("a code");
    let right = server.code().expect("the real code");
    assert_ne!(wrong, right, "two random codes must differ");

    let server_thread =
        std::thread::spawn(move || server.receive().expect_err("a wrong code cannot verify"));

    let phone = pairing_phone_join(uri, wrong, PHONE_SEED.to_vec(), "pixel-7".to_string())
        .expect("the phone publishes its flight");
    drop(phone);

    let error = server_thread.join().expect("the admitting side finishes");
    assert_eq!(
        error,
        PairingFfiError::CodeMismatch(
            "the code did not match — this pairing session is now burned".to_string()
        ),
        "a wrong code must be a CodeMismatch carrying the core's sentence, not a generic failure"
    );

    // The typed error carries the same sentence the CLI prefixes with its verb.
    assert_eq!(
        format!("pairing failed: {error}"),
        "pairing failed: the code did not match — this pairing session is now burned"
    );
}

/// A malformed code is refused with the core's shape rule, and a good one
/// canonicalizes.
#[test]
fn code_errors_carry_the_core_sentence() {
    let error = pairing_code_phrase("only three words".to_string()).expect_err("too short");
    assert_eq!(
        error,
        PairingFfiError::CodeShape("a code is 4 words, got 3".to_string())
    );

    let error = pairing_code_phrase("zzzz zzzz zzzz zzzz".to_string()).expect_err("not a word");
    match error {
        PairingFfiError::UnknownWord(sentence) => {
            assert!(
                sentence.starts_with("unknown code word \"zzzz\""),
                "the sentence must name the word: {sentence}"
            );
        }
        other => panic!("expected an UnknownWord, got {other:?}"),
    }

    let random = pairing_code_random().expect("a code");
    assert_eq!(
        pairing_code_phrase(random.clone()).expect("its own code parses"),
        random,
        "a code this build produced must parse back to itself"
    );
}

/// A seed that is not 32 bytes is refused by name, before any key exists.
#[test]
fn a_seed_of_the_wrong_length_is_refused() {
    let error = device_key_from_seed(vec![0u8; 31]).expect_err("31 bytes is not a seed");
    assert_eq!(
        format!("{error}"),
        "a device seed is exactly 32 bytes, got 31"
    );
    let error = device_key_from_seed(Vec::new()).expect_err("an empty seed is not a seed");
    assert_eq!(
        format!("{error}"),
        "a device seed is exactly 32 bytes, got 0"
    );
}

/// The relay session: dial through the boundary, then drain, ack, read the
/// directory, and read the identity the relay confirmed.
#[test]
fn the_session_dials_drains_acks_and_reads_the_directory() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    runtime.block_on(async {
        let root = Arc::new(arreo_core::identity::RootKey::from_seed(SERVER_SEED));
        let rows = vec![
            row_for(&SERVER_SEED, "server-box", Presence::Online),
            row_for(&OTHER_DEVICE_SEED, "other-box", Presence::Stale),
        ];
        let relay = TestRelay::start(ACCOUNT.to_string(), Arc::clone(&root), rows).await;

        let device = device_key_from_seed(PHONE_SEED.to_vec()).expect("a key");
        let cert = device_cert_issue(
            root_key_from_seed(SERVER_SEED.to_vec()).expect("a root"),
            device.public_hex(),
            "pixel-7".to_string(),
            FfiRole::Viewer,
            1_760_000_000_000,
            1,
        )
        .expect("a certificate");

        let session = relay_session_dial(
            relay.address(),
            ACCOUNT.to_string(),
            Arc::clone(&device),
            Arc::clone(&cert),
        )
        .await
        .expect("the relay accepts the session");

        // The identity is what the *relay* confirmed, not what was announced.
        assert_eq!(session.device_id(), format!("dev_{}", device.fingerprint()));
        assert_eq!(session.account(), ACCOUNT);
        assert_eq!(
            session.nonce().len(),
            32,
            "the session's challenge is the 32 bytes the relay minted"
        );
        relay.await_route(&device.fingerprint()).await;

        // Drain and ack are control-plane: they complete, and the relay answers.
        session.drain(1).await.expect("the drain is accepted");
        session.ack(2).await.expect("the ack is accepted");
        session
            .heartbeat()
            .await
            .expect("the heartbeat is accepted");

        // The directory read is a request/response over the session, routed by
        // the sequence number the write pump reserved.
        let reply = session
            .machines(false)
            .await
            .expect("the directory answers");
        assert_eq!(reply.refused, None, "the relay did not refuse");
        assert_eq!(reply.machines.len(), 2);
        let names: Vec<&str> = reply.machines.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["server-box", "other-box"],
            "the rows arrive in the order the relay listed them"
        );
        let other = reply
            .machines
            .iter()
            .find(|m| m.name == "other-box")
            .expect("the stale row");
        assert_eq!(other.presence, FfiPresence::Stale);
        assert_eq!(
            other.daemon_key.as_deref(),
            Some(
                device_key_from_seed(OTHER_DEVICE_SEED.to_vec())
                    .unwrap()
                    .public_hex()
                    .as_str()
            )
        );
        let server_row = reply
            .machines
            .iter()
            .find(|m| m.name == "server-box")
            .expect("the online row");
        assert_eq!(server_row.presence, FfiPresence::Online);
        assert_eq!(
            server_row.machine_id,
            row_for(&SERVER_SEED, "server-box", Presence::Online)
                .machine_id
                .as_str()
        );

        // The same rows, mirrored into the client's own cache, resolve by name
        // and by id — and the cache carries no dial key, by its own rule.
        let cache = directory_cache_new();
        cache.mirror(reply.machines.clone(), 1_760_000_000_000);
        assert_eq!(cache.len(), 2);
        assert_eq!(cache.as_of_ms(), 1_760_000_000_000);
        let cached = cache
            .lookup("other-box".to_string())
            .expect("a valid name")
            .expect("the row is cached");
        assert_eq!(cached.presence, FfiPresence::Stale);
        assert_eq!(cached.daemon_key, None);
        assert_eq!(
            cache
                .resolve_known(server_row.machine_id.clone())
                .expect("a valid id")
                .expect("the row is known")
                .name,
            "server-box"
        );
        assert!(
            cache.lookup("Not A Name".to_string()).is_err(),
            "a name that cannot exist is refused by the directory's own rule"
        );
    });
}

/// Bytes cross the boundary between two devices, in the accept-then-open order
/// the core requires.
#[test]
fn bytes_cross_the_boundary_between_two_devices() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    runtime.block_on(async {
        let root = Arc::new(arreo_core::identity::RootKey::from_seed(SERVER_SEED));
        let relay = TestRelay::start(ACCOUNT.to_string(), Arc::clone(&root), Vec::new()).await;

        let root_handle = root_key_from_seed(SERVER_SEED.to_vec()).expect("a root");
        let mut sessions = Vec::new();
        for seed in [PHONE_SEED, OTHER_DEVICE_SEED] {
            let device = device_key_from_seed(seed.to_vec()).expect("a key");
            let cert = device_cert_issue(
                Arc::clone(&root_handle),
                device.public_hex(),
                "a-device".to_string(),
                FfiRole::Owner,
                1_760_000_000_000,
                1,
            )
            .expect("a certificate");
            let session = relay_session_dial(
                relay.address(),
                ACCOUNT.to_string(),
                Arc::clone(&device),
                cert,
            )
            .await
            .expect("the relay accepts the session");
            sessions.push((device, session));
        }
        let (device_a, session_a) = &sessions[0];
        let (device_b, session_b) = &sessions[1];
        relay.await_route(&device_a.fingerprint()).await;
        relay.await_route(&device_b.fingerprint()).await;

        // B parks in the accept door: it can only learn that A wrote by waiting
        // here, and only then may it open the stream the bytes are queued on.
        let peer_b = Arc::clone(session_b);
        let accept = tokio::spawn(async move { peer_b.next_peer().await });

        // A opens the stream to B by id — the door a phone uses when it knows
        // which machine it is talking to (a directory row's `daemon_key`).
        let peer = relay_peer_parse(device_b.display_id()).expect("a valid device id");
        assert_eq!(peer.fingerprint(), device_b.fingerprint());
        let stream = session_a.stream_to(peer);
        assert_eq!(stream.peer(), device_b.display_id());
        stream
            .write(b"hello from the phone".to_vec())
            .await
            .expect("the bytes leave");

        let arrived = tokio::time::timeout(Duration::from_secs(10), accept)
            .await
            .expect("the accept door answers")
            .expect("the accept task finishes")
            .expect("the accept door succeeds")
            .expect("a peer wrote to B");
        assert_eq!(
            arrived.fingerprint(),
            device_a.fingerprint(),
            "the peer B is told about is the device that wrote"
        );

        let inbound = session_b.stream_to(arrived);
        let bytes = tokio::time::timeout(Duration::from_secs(10), inbound.read(1024))
            .await
            .expect("the read answers")
            .expect("the read succeeds");
        assert_eq!(bytes, b"hello from the phone");
        inbound.close().await.expect("the stream closes");
    });
}

/// The codec: a message built through the boundary encodes, frames, and decodes
/// back to the same value — including the two shapes UniFFI has no native form
/// for (the cursor tuple, and a `usize` field).
#[test]
fn the_codec_round_trips_a_message() {
    let message = WireMessage::Snapshot {
        v: codec_protocol_version(),
        id: "pane-1".to_string(),
        lines: vec!["$ cargo test".to_string(), "ok".to_string()],
        cursor: WireCursor { row: 3, col: 17 },
    };
    let body = codec_encode(message.clone()).expect("the message encodes");
    assert_eq!(codec_decode(body.clone()).expect("and decodes"), message);

    // The framed form is the body plus a `u32 LE` length prefix, and the frame
    // decoder reports how many bytes it consumed so a caller advances exactly.
    let frame = codec_encode_frame(message.clone()).expect("the frame encodes");
    assert_eq!(frame.len(), body.len() + 4);
    let decoded =
        arreo_core_ffi::codec::codec_decode_frame(frame.clone()).expect("the frame decodes");
    assert_eq!(decoded.message, message);
    assert_eq!(decoded.consumed as usize, body.len() + 4);
    // `frame_body_len` reads a *framed* buffer: it is the helper a caller uses
    // to decide whether the bytes have arrived yet, before it decodes.
    assert_eq!(
        arreo_core_ffi::codec::codec_frame_body_len(frame.clone()).expect("the length reads"),
        body.len() as u64
    );
    let error = arreo_core_ffi::codec::codec_frame_body_len(body.clone())
        .expect_err("a bare body is not framed");
    assert!(
        format!("{error}").contains("over the 1048576-byte budget"),
        "a bare body read as a length is refused before any allocation: {error}"
    );

    // A state event carries the enum, and a spawn carries the optional budget —
    // the two shapes a sidebar and a launcher actually send.
    let event = WireMessage::StateEvent {
        v: codec_protocol_version(),
        id: "pane-1".to_string(),
        state: WireAgentState::Question,
        confidence: "inferred:silence+prompt-shape".to_string(),
        matched_pattern: Some("[y/n]".to_string()),
    };
    assert_eq!(
        codec_decode(codec_encode(event.clone()).expect("encode")).expect("decode"),
        event
    );

    let spawn = WireMessage::Spawn {
        v: codec_protocol_version(),
        id: "pane-2".to_string(),
        program: "pi".to_string(),
        args: vec!["--print".to_string()],
        cols: 120,
        rows: 40,
        memory_max: Some(512 * 1024 * 1024),
        pids_max: None,
        kill_on_breach: false,
    };
    assert_eq!(
        codec_decode(codec_encode(spawn.clone()).expect("encode")).expect("decode"),
        spawn
    );

    // Decode is total on garbage: an `Err`, never a panic.
    let error = codec_decode(vec![0xff, 0xff, 0xff, 0xff]).expect_err("garbage is refused");
    assert!(
        matches!(error, CodecFfiError::Decode(_)),
        "garbage must be a typed decode error, got {error:?}"
    );

    // A frame that declares more than it carries is truncated, and the sentence
    // names both numbers — the length prefix is little-endian, so `0x00100000`
    // is the 1 MiB budget exactly and the frame wants four bytes more than that.
    let short = vec![0x00, 0x00, 0x10, 0x00, 0x01];
    let error = arreo_core_ffi::codec::codec_decode_frame(short).expect_err("short frames fail");
    match error {
        CodecFfiError::Truncated(sentence) => {
            assert_eq!(
                sentence,
                "codec frame: truncated (want 1048580 bytes, have 5)"
            );
        }
        other => panic!("expected a Truncated, got {other:?}"),
    }

    // A length prefix over the budget is refused before any allocation, which is
    // the bound that stops one sender making the codec hold an unbounded buffer.
    let over = vec![0x01, 0x00, 0x10, 0x00];
    let error = arreo_core_ffi::codec::codec_decode_frame(over).expect_err("over budget");
    assert!(
        format!("{error}").contains("over the 1048576-byte budget"),
        "an over-budget length is refused by name: {error}"
    );
}

/// The theme engine's tokens cross with their quantization intact.
#[test]
fn theme_tokens_cross_the_boundary() {
    let theme = theme_builtin(FfiVariant::Dark, FfiDepth::Truecolor);
    assert_eq!(theme.name(), "arreo");
    assert_eq!(theme.variant(), FfiVariant::Dark);
    assert_eq!(theme.depth(), FfiDepth::Truecolor);

    let tokens = theme.tokens();
    assert!(
        tokens.len() > 20,
        "the built-in theme resolves a real token table, got {}",
        tokens.len()
    );
    assert!(
        tokens.windows(2).all(|w| w[0].name < w[1].name),
        "the token table is sorted by name, so two runs agree"
    );

    // The background is a real color at truecolor, and an ANSI index at 16.
    let background = theme.color("background".to_string());
    assert!(
        matches!(background, FfiColor::Rgb { .. } | FfiColor::Ansi { .. }),
        "the background must resolve to a color, got {background:?}"
    );
    let quantized = theme.with_depth(FfiDepth::Ansi16);
    assert_eq!(quantized.depth(), FfiDepth::Ansi16);
    assert!(
        matches!(
            quantized.color("background".to_string()),
            FfiColor::Ansi { .. } | FfiColor::TerminalDefault
        ),
        "an ANSI-16 surface must not be handed a 24-bit value"
    );

    // An unknown token inherits the terminal rather than inventing a color.
    assert_eq!(
        theme.color("no-such-token".to_string()),
        FfiColor::TerminalDefault
    );

    // The state board's language, and the label rule that keeps a state's own
    // hue from failing WCAG AA as text.
    for state in ["working", "blocked", "done", "idle", "question", "error"] {
        let color = theme.state_color(state.to_string());
        assert!(
            matches!(color, FfiColor::Rgb { .. } | FfiColor::Ansi { .. }),
            "{state} must resolve to a color, got {color:?}"
        );
    }
    assert_eq!(
        theme.state_color("not-a-state".to_string()),
        theme.color("textMuted".to_string()),
        "an unknown state falls back to the muted token, as the TUI's board does"
    );
    let label = theme.state_label_color("idle".to_string());
    let own = theme.state_color("idle".to_string());
    assert!(
        matches!(label, FfiColor::Rgb { .. } | FfiColor::Ansi { .. }),
        "the label rule must resolve, got {label:?}"
    );
    // The rule's whole point: a hue that cannot carry AA text is replaced.
    if label != own {
        assert_eq!(label, theme.color("textMuted".to_string()));
    }

    // A theme the user authored arrives as a token table, with no file read.
    let custom = arreo_core_ffi::theme::theme_from_tokens(
        "custom".to_string(),
        FfiVariant::Light,
        FfiDepth::Ansi256,
        vec![arreo_core_ffi::theme::ThemeToken {
            name: "background".to_string(),
            color: FfiColor::Rgb {
                r: 0x12,
                g: 0x34,
                b: 0x56,
            },
        }],
    );
    assert_eq!(custom.name(), "custom");
    assert_eq!(custom.tokens().len(), 1);
    assert!(
        matches!(
            custom.color("background".to_string()),
            FfiColor::Ansi { .. } | FfiColor::TerminalDefault
        ),
        "a 24-bit color on a 256-color surface is quantized, not passed through: {:?}",
        custom.color("background".to_string())
    );

    // The color primitives, on their own.
    assert_eq!(
        arreo_core_ffi::theme::color_parse("#ff0000".to_string()).expect("a hex color"),
        FfiColor::Rgb { r: 255, g: 0, b: 0 }
    );
    assert_eq!(
        arreo_core_ffi::theme::color_parse("none".to_string()).expect("the inherit keyword"),
        FfiColor::TerminalDefault
    );
    let error = arreo_core_ffi::theme::color_parse("not a color".to_string()).expect_err("bad");
    assert!(
        format!("{error}").starts_with("malformed color"),
        "a bad color carries the core's sentence: {error}"
    );
    assert_eq!(
        arreo_core_ffi::theme::color_quantize(
            FfiColor::Rgb { r: 255, g: 0, b: 0 },
            FfiDepth::NoColor
        ),
        FfiColor::TerminalDefault,
        "no-color means no color, not a black"
    );
    assert!(
        arreo_core_ffi::theme::color_contrast_ratio(
            FfiColor::Rgb { r: 0, g: 0, b: 0 },
            FfiColor::Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        )
        .expect("two real colors have a ratio")
            > 20.0,
        "black on white is the maximum contrast"
    );
    assert!(
        arreo_core_ffi::theme::color_contrast_ratio(
            FfiColor::TerminalDefault,
            FfiColor::Rgb { r: 0, g: 0, b: 0 }
        )
        .is_none(),
        "a terminal's own palette is not knowable, so no ratio is invented"
    );
}

/// The directory cache is a value the UI owns, and its rules survive the
/// boundary: a mirror replaces, and a bad name is refused by name.
#[test]
fn the_directory_cache_mirrors_and_refuses() {
    let cache = directory_cache_new();
    assert!(cache.is_empty());
    assert_eq!(cache.len(), 0);
    assert_eq!(cache.as_of_ms(), 0, "0 means the relay was never contacted");

    let rows = vec![
        MachineRowInfo {
            machine_id: "0123456789abcdef0123456789abcdef".to_string(),
            name: "workbox".to_string(),
            name_conflict: false,
            presence: FfiPresence::Online,
            last_seen_ms: 1_760_000_000_000,
            proto_version: 0,
            tombstone_until_ms: None,
            daemon_key: Some("ab".repeat(32)),
        },
        // A row whose name cannot exist is dropped rather than poisoning the
        // mirror: the rest of what the relay said still arrives.
        MachineRowInfo {
            machine_id: "ffffffffffffffffffffffffffffffff".to_string(),
            name: "Not A Name".to_string(),
            name_conflict: false,
            presence: FfiPresence::Offline,
            last_seen_ms: 0,
            proto_version: 0,
            tombstone_until_ms: None,
            daemon_key: None,
        },
    ];
    cache.mirror(rows.clone(), 1_760_000_000_000);
    assert_eq!(cache.len(), 1, "the unrepresentable row is dropped");
    assert_eq!(cache.as_of_ms(), 1_760_000_000_000);
    let found = cache
        .lookup("workbox".to_string())
        .expect("a valid name")
        .expect("the row is there");
    assert_eq!(found.machine_id, "0123456789abcdef0123456789abcdef");
    assert_eq!(found.presence, FfiPresence::Online);
    // **The cache carries no dial key**, and that is the core's rule rather than
    // an omission: `CachedMachine` has no such field, because a key a client
    // could dial out of a stale row is exactly the "a copy acts like a source of
    // truth" hazard the cache's type exists to prevent. A phone dials from a
    // live `DirectoryReply` (which does carry `daemon_key` — asserted above);
    // the cache is the offline fallback for *names*.
    assert_eq!(found.daemon_key, None);

    // A mirror *replaces*: a name the relay no longer lists is gone.
    cache.mirror(Vec::new(), 1_760_000_001_000);
    assert!(cache.is_empty(), "a mirror never merges");
    assert_eq!(cache.lookup("workbox".to_string()).expect("valid"), None);

    let error = cache.lookup("Bad Name".to_string()).expect_err("refused");
    assert!(
        format!("{error}").starts_with("invalid machine name"),
        "the name rule's own sentence crosses: {error}"
    );
    let error = cache
        .resolve_known("not-a-machine-id".to_string())
        .expect_err("refused");
    assert!(
        format!("{error}").starts_with("not a machine id"),
        "the id rule's own sentence crosses: {error}"
    );
}

/// **The issuing door refuses a key nobody can prove possession of** — the one
/// place this boundary is policy rather than encoding.
///
/// A small-order ed25519 point is not a key: ed25519-dalek's own documentation
/// says a signature can be forged for almost any message under one. The core's
/// admitting door (`DeviceAuthority::issue`) refuses it with
/// `AuthorityError::WeakKey`; this surface is the only issuing door a mobile UI
/// has, and the key it issues for arrives from the peer
/// (`PairingRequest.public_key`), so a malicious phone would otherwise push a
/// weak key through the one path that had no check — a divergence between two
/// doors that do the same job, which is exactly what the typed-error criterion
/// exists to prevent.
///
/// The sentence is the core's, carried rather than restated, so the two doors
/// cannot drift apart: this asserts the *core's* wording reaches the caller.
#[test]
fn a_weak_public_key_cannot_be_pinned_through_the_boundary() {
    // The all-zero compressed point: a valid encoding of a small-order point, and
    // the same fixture the core's own authority test refuses.
    let weak = "00".repeat(32);

    // `DeviceCertHandle` has no `Debug` (a UniFFI object), so the refusal is
    // destructured rather than unwrapped.
    let error = match device_cert_issue(
        root_key_from_seed(SERVER_SEED.to_vec()).expect("a root"),
        weak.clone(),
        "weak-phone".to_string(),
        FfiRole::Owner,
        1_760_000_000_000,
        1,
    ) {
        Ok(_) => panic!("a small-order point must not be pinnable"),
        Err(error) => error,
    };

    // The core's sentence, verbatim — not a second wording of it.
    assert_eq!(
        format!("{error}"),
        arreo_core::identity::AuthorityError::WeakKey.to_string(),
        "the refusal must carry the core's own sentence"
    );
    assert!(
        format!("{error}").contains("small-order"),
        "and it must name what is wrong with the key: {error}"
    );

    // The control: the same call with a real key still issues, so the check
    // refuses the *key* and not the call.
    let good = device_key_from_seed(PHONE_SEED.to_vec()).expect("a key");
    device_cert_issue(
        root_key_from_seed(SERVER_SEED.to_vec()).expect("a root"),
        good.public_hex(),
        "pixel-7".to_string(),
        FfiRole::Owner,
        1_760_000_000_000,
        1,
    )
    .expect("an ordinary key is still issued for");
}
