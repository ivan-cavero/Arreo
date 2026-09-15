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

use arreo_core::identity::{DeviceCert, DeviceId, DeviceKey, VerifyingKey};
use arreo_core::mesh::{MachineId, MachineRow, Name, Presence};
use arreo_core::notify::{NotifyAction, MAX_REPLY_BYTES, PANE_EXITED};
use arreo_core::pairing::{MailboxRequest, MailboxResponse, Slot};
use arreo_core::proto::{codec, AgentState, Message, MetricsPoint, VERSION};
use arreo_core::relay::session::RelaySession;
use arreo_core::relay::{
    decode_message, encode_message, encode_payload, read_envelope, read_frame, verify_auth,
    write_frame, Auth, AuthReply, DirectoryReply, DrainReport, Hello, HelloReply, Outcome,
    RelayEnvelope, RelayHeader, RelayKind, MAX_HANDSHAKE_BYTES, RELAY_SENDER, RELAY_VERSION,
};
use arreo_core::transport::{FlightGuard, SecureChannel};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use arreo_core_ffi::codec::{
    codec_decode, codec_encode, codec_encode_frame, codec_max_frame_bytes, codec_protocol_version,
    WireAgentState, WireCursor, WireMessage, WireMetricsPoint, WireNotifyAction,
};
use arreo_core_ffi::directory::{directory_cache_new, FfiPresence, MachineRowInfo};
use arreo_core_ffi::errors::{CodecFfiError, PairingFfiError, SessionFfiError};
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
        Self::start_declaring(account, root, rows, None).await
    }

    /// The same relay, with one thing changed: the device id it confirms in
    /// `AuthReply::Welcome`, whatever it actually verified.
    ///
    /// A relay is a carrier, and this is the one lie it can tell about a device
    /// to that device's face — the identity it names. Everything else is
    /// unchanged (the proof is verified, routes are keyed by the real id), so a
    /// test using this is testing exactly "whose word is this device's identity".
    async fn start_declaring(
        account: String,
        root: Arc<arreo_core::identity::RootKey>,
        rows: Vec<MachineRow>,
        declares: Option<String>,
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
                    let declares = declares.clone();
                    tokio::spawn(async move {
                        serve_relay_connection(connection, account, root, routes, rows, declares)
                            .await;
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
///
/// `declares` is the id to *confirm* in `Welcome`; `None` is the honest relay,
/// which confirms the id it verified. Routing always uses the verified id.
async fn serve_relay_connection(
    connection: arreo_core::transport::Connection,
    account: String,
    root: Arc<arreo_core::identity::RootKey>,
    routes: Arc<Mutex<HashMap<String, tokio::sync::mpsc::UnboundedSender<Vec<u8>>>>>,
    rows: Arc<Mutex<Vec<MachineRow>>>,
    declares: Option<String>,
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
        device_id: declares
            .clone()
            .unwrap_or_else(|| device.device_id.display_id()),
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
// A machine's daemon, behind the relay
// ---------------------------------------------------------------------------

/// The machine's half of a daemon conversation: the same two steps the daemon's
/// relay peer path runs — accept the Noise channel over the relay stream, answer
/// the daemon handshake, then answer the verbs — with the daemon's own session
/// loop replaced by the answers this test needs. One answer per verb asked, in
/// order, whatever the verb is: a metrics read (T-0114) and a quick action
/// (T-0115) are both daemon verbs and both ride this one conversation.
///
/// **One conversation, N verbs, which is what a real daemon does.** The daemon's
/// relay peer runs `serve_session` behind `serve_peer`, and that loop *stays
/// open after a verb*: it reads the next one on the same channel. The first
/// version of this fixture closed the write half after every single answer — the
/// shape `serve_session_with_handoff` uses on its way *out* — which made the
/// phone's next call reconnect. That is the one path a real daemon does not take,
/// and the reason the two reads below "passed" while a real daemon broke on the
/// second one (T-0114's p1).
///
/// That path lives in `arreo-server` (`relay_client.rs::serve_peer`), which this
/// crate may not link (AGPL), so the fixture is built out of the shipped
/// vocabulary instead: the transport is the product's own [`SecureChannel`], the
/// frames are the product's own [`Message`]s, and the accept-then-open order is
/// the core's. What it is not is a shortcut around the boundary: the phone's half
/// is the exported surface, and nothing here is reachable from it.
///
/// The *ending* is the daemon's own (`serve_session_with_handoff`): write the
/// last frame, close the write half, and give the pump [`FINAL_FRAME_GRACE`] to
/// put it on the wire — so the client reads its answer rather than a bare close.
/// Once a conversation, at the end, instead of once per verb.
async fn serve_daemon(
    mut session: RelaySession,
    key: DeviceKey,
    phone: VerifyingKey,
    answers: Vec<Message>,
) -> Vec<Message> {
    let local = key.noise_static();
    let peer = tokio::time::timeout(Duration::from_secs(10), session.next_peer())
        .await
        .expect("the phone's bytes reach the daemon")
        .expect("the session stays open");
    let stream = session.stream_to(&peer);
    let guard = FlightGuard::default();
    let (channel, device) = SecureChannel::accept(stream, &local, &guard, move |_| Some(phone))
        .await
        .expect("the phone's Noise handshake completes");
    assert_eq!(
        device,
        DeviceId::from_key(&phone),
        "the daemon is told which device dialed"
    );
    let (mut reader, mut writer) = tokio::io::split(channel);

    let mut buf = Vec::new();
    assert!(
        matches!(
            read_daemon_message(&mut reader, &mut buf).await,
            Message::Hello { .. }
        ),
        "the daemon protocol opens with Hello"
    );
    write_daemon_message(
        &mut writer,
        &Message::Welcome {
            v: VERSION,
            server: "arreo-server-test".to_string(),
        },
    )
    .await;

    // The verbs, all on the one channel the handshake opened — no close in
    // between, because that is what the daemon does.
    let mut seen = Vec::new();
    for answer in answers {
        seen.push(read_daemon_message(&mut reader, &mut buf).await);
        write_daemon_message(&mut writer, &answer).await;
    }
    let _ = writer.shutdown().await;
    tokio::time::sleep(FINAL_FRAME_GRACE).await;
    seen
}

/// A machine whose answer is not a frame: the daemon half of the fail-fast case.
///
/// A daemon can produce this — a truncated write, a version skew that mis-frames,
/// a hostile peer — and the client must refuse it *now* rather than treat the
/// undecodable length as a frame that is still arriving. That distinction is the
/// codec's own (`Truncated` means "keep reading"; every other error means the
/// bytes will never be readable), and this fixture is what holds the boundary to
/// it: it answers the handshake and one verb, then writes four bytes that name a
/// body no client can accept.
async fn serve_a_frame_no_client_can_read(
    mut session: RelaySession,
    key: DeviceKey,
    phone: VerifyingKey,
    declared: [u8; 4],
) -> Message {
    let local = key.noise_static();
    let peer = tokio::time::timeout(Duration::from_secs(10), session.next_peer())
        .await
        .expect("the phone's bytes reach the daemon")
        .expect("the session stays open");
    let stream = session.stream_to(&peer);
    let guard = FlightGuard::default();
    let (channel, _) = SecureChannel::accept(stream, &local, &guard, move |_| Some(phone))
        .await
        .expect("the phone's Noise handshake completes");
    let (mut reader, mut writer) = tokio::io::split(channel);
    let mut buf = Vec::new();
    assert!(
        matches!(
            read_daemon_message(&mut reader, &mut buf).await,
            Message::Hello { .. }
        ),
        "the daemon protocol opens with Hello"
    );
    write_daemon_message(
        &mut writer,
        &Message::Welcome {
            v: VERSION,
            server: "arreo-server-test".to_string(),
        },
    )
    .await;
    let asked = read_daemon_message(&mut reader, &mut buf).await;
    writer
        .write_all(&declared)
        .await
        .expect("the bytes leave the daemon");
    writer.flush().await.expect("the bytes flush");
    let _ = writer.shutdown().await;
    tokio::time::sleep(FINAL_FRAME_GRACE).await;
    asked
}

/// One framed [`Message`] off the daemon's channel, reading as needed.
async fn read_daemon_message<R: AsyncRead + Unpin>(reader: &mut R, buf: &mut Vec<u8>) -> Message {
    loop {
        if let Ok((message, consumed)) = codec::decode_frame(buf) {
            buf.drain(..consumed);
            return message;
        }
        let mut chunk = [0u8; 8192];
        let read = reader
            .read(&mut chunk)
            .await
            .expect("the phone's bytes arrive");
        assert!(read > 0, "the phone's stream ended mid-frame");
        buf.extend_from_slice(&chunk[..read]);
    }
}

/// One framed [`Message`] onto the daemon's channel.
async fn write_daemon_message<W: AsyncWrite + Unpin>(writer: &mut W, message: &Message) {
    let frame = codec::encode_frame(message).expect("the frame encodes");
    writer
        .write_all(&frame)
        .await
        .expect("the frame leaves the daemon");
    writer.flush().await.expect("the frame flushes");
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const ACCOUNT: &str = "acct-contract-test";
const SERVER_SEED: [u8; 32] = [0x11; 32];
const PHONE_SEED: [u8; 32] = [0x22; 32];
const OTHER_DEVICE_SEED: [u8; 32] = [0x33; 32];
const DAEMON_SEED: [u8; 32] = [0x44; 32];

/// How long a finished conversation stays alive after its write half closes, so
/// the pump can put the final frame on the wire.
///
/// The daemon's own value and reason (`arreo-server/src/daemon.rs`,
/// `FINAL_FRAME_GRACE`, T-0052): closing the write half is the pump's signal to
/// drain what the session already wrote, and without the bounded pause the bytes
/// are written into the duplex and discarded with the channel — a client that
/// sees a bare close instead of its answer. `arreo-server` is AGPL and this test
/// may not link it, so the number is repeated here rather than imported.
const FINAL_FRAME_GRACE: Duration = Duration::from_millis(300);

/// The operator's reply text in the act test, verbatim across the boundary.
///
/// Not a bare `"y"`: the daemon sends `text + "\n"` to the pane through the
/// audited send path, so the bytes a phone sends *are* what the agent reads, and a
/// boundary that trimmed, re-encoded or newline-fixed the text would change the
/// agent's input. Spaces and punctuation are where such a mangle shows.
const REPLY_TEXT: &str = "yes, go ahead: run the migration first";

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

        // The identity is derived from the key this device dialed with, and the
        // relay's `Welcome` was checked against it at dial time — an honest relay
        // confirms the id the certificate names, which is this one. See
        // `a_relay_that_confirms_someone_elses_identity_is_refused` for the other
        // half of that rule.
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

/// A relay's word is not this device's identity.
///
/// `AuthReply::Welcome` carries the id the relay verified; this session derives
/// its id from the key it dialed with, and checks the echo against it — so a
/// relay that confirms *someone else's* id is refused rather than trusted. That
/// matters because the id is used as an assertion, not as a display string: it
/// is the hint that opens a peer stream ("this is who is dialing") and what
/// [`arreo_core_ffi::relay::RelaySessionHandle::device_id`] reports about the
/// phone itself.
///
/// The relay here does everything else an honest relay does — it verifies the
/// proof, and routes by the id it verified — and lies only in the confirmation.
#[test]
fn a_relay_that_confirms_someone_elses_identity_is_refused() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    runtime.block_on(async {
        let root = Arc::new(arreo_core::identity::RootKey::from_seed(SERVER_SEED));
        let other = device_key_from_seed(OTHER_DEVICE_SEED.to_vec()).expect("a key");
        let relay = TestRelay::start_declaring(
            ACCOUNT.to_string(),
            Arc::clone(&root),
            Vec::new(),
            Some(other.display_id()),
        )
        .await;

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

        let refusal = match relay_session_dial(
            relay.address(),
            ACCOUNT.to_string(),
            Arc::clone(&device),
            cert,
        )
        .await
        {
            Ok(_) => panic!("a relay that confirms another identity must be refused"),
            Err(error) => error,
        };
        assert_eq!(
            refusal,
            SessionFfiError::RelayIdentityMismatch {
                confirmed: other.display_id(),
                derived: device.display_id(),
            },
            "the refusal carries both: the relay's claim and this device's own key"
        );
        assert_eq!(
            format!("{refusal}"),
            format!(
                "the relay confirmed {} for a device whose own key names {}",
                other.display_id(),
                device.display_id()
            ),
            "the sentence a phone shows names what was claimed and what is true"
        );
    });
}

/// The metrics read a RAM meter needs: a pane's durable series crosses the
/// boundary as a typed record, the tier the machine *actually served* travels
/// with it, and an empty window is an empty list rather than a failure.
///
/// The phone is the exported surface; the machine is a daemon on the far side of
/// the test relay, speaking the same Noise + Hello/Welcome + verb path the real
/// daemon's relay peer runs.
#[test]
fn the_session_reads_a_panes_metrics_series() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    runtime.block_on(async {
        let root = Arc::new(arreo_core::identity::RootKey::from_seed(SERVER_SEED));
        let relay = TestRelay::start(ACCOUNT.to_string(), Arc::clone(&root), Vec::new()).await;
        let root_handle = root_key_from_seed(SERVER_SEED.to_vec()).expect("a root");

        // The phone: a viewer, which is what a phone is, dialed through the
        // exported surface.
        let phone = device_key_from_seed(PHONE_SEED.to_vec()).expect("a key");
        let phone_cert = device_cert_issue(
            Arc::clone(&root_handle),
            phone.public_hex(),
            "pixel-7".to_string(),
            FfiRole::Viewer,
            1_760_000_000_000,
            1,
        )
        .expect("a certificate");
        let session = relay_session_dial(
            relay.address(),
            ACCOUNT.to_string(),
            Arc::clone(&phone),
            phone_cert,
        )
        .await
        .expect("the relay accepts the phone");
        relay.await_route(&phone.fingerprint()).await;

        // The machine: a daemon on the far side of the relay. Its half is the
        // core's own session, because the *daemon* half of the relay peer path
        // is a server and `arreo-server` is AGPL.
        let daemon_key = DeviceKey::from_seed(DAEMON_SEED);
        let daemon_id = DeviceId::from_key(&daemon_key.public());
        let daemon_cert = device_cert_issue(
            Arc::clone(&root_handle),
            daemon_key.public_hex(),
            "server-box".to_string(),
            FfiRole::Owner,
            1_760_000_000_000,
            2,
        )
        .expect("a certificate");
        // The certificate the boundary issued, back as the core's own type: the
        // daemon dials with it exactly as the real one dials with the file
        // pairing wrote.
        let daemon_cert =
            DeviceCert::decode(&daemon_cert.encode().expect("the certificate encodes"))
                .expect("the core reads its own certificate");
        let daemon = RelaySession::dial(relay.addr, ACCOUNT, &daemon_key, &daemon_cert)
            .await
            .expect("the relay accepts the daemon");
        relay.await_route(daemon_id.as_str()).await;

        let answers = vec![
            // A window the machine answered with a coarser tier than the ask.
            Message::MetricsSeries {
                v: VERSION,
                id: "pane-1".to_string(),
                step_ms: 60_000,
                downshifted: true,
                rows: vec![
                    MetricsPoint {
                        ts_ms: 1_760_000_000_000,
                        rss_avg: 4 * 1024 * 1024,
                        rss_peak: 9 * 1024 * 1024,
                        cpu_avg: 1.5,
                        cpu_peak: 7.25,
                        pids: 12,
                    },
                    MetricsPoint {
                        ts_ms: 1_760_000_060_000,
                        rss_avg: 5 * 1024 * 1024,
                        rss_peak: 10 * 1024 * 1024,
                        cpu_avg: 2.5,
                        cpu_peak: 8.5,
                        pids: 13,
                    },
                ],
            },
            // A refusal — the machine *answered*, in its own words. The session
            // is still in step with the next verb, so the reads after it must run
            // on this same conversation: a client that dropped it on any failure
            // would hand its next handshake to a daemon that is still reading
            // verbs, which is the failure the whole cache exists to prevent.
            Message::Error {
                v: VERSION,
                message: "no history for that pane in this window".to_string(),
            },
            // A pane that just started: no rows in this window, which is what
            // the daemon answers for a pane it has no history for.
            Message::MetricsSeries {
                v: VERSION,
                id: "pane-2".to_string(),
                step_ms: 10_000,
                downshifted: false,
                rows: Vec::new(),
            },
            // The last answer, and it is here to be *unclaimed*: the read after
            // the wrong-key refusal takes it, which is only possible if that
            // refusal never reached the machine.
            Message::MetricsSeries {
                v: VERSION,
                id: "pane-3".to_string(),
                step_ms: 1_000,
                downshifted: false,
                rows: vec![MetricsPoint {
                    ts_ms: 1_760_000_120_000,
                    rss_avg: 6 * 1024 * 1024,
                    rss_peak: 11 * 1024 * 1024,
                    cpu_avg: 3.5,
                    cpu_peak: 9.0,
                    pids: 14,
                }],
            },
        ];
        // One conversation for both reads: the machine accepts the phone's
        // handshake once and answers every verb on that channel, which is what
        // the daemon's own `serve_session` loop does.
        let daemon_key_hex = daemon_key.public_hex();
        let phone_key =
            arreo_core::identity::verifying_key_from_hex(&phone.public_hex()).expect("hex");
        let machine = tokio::spawn(serve_daemon(daemon, daemon_key, phone_key, answers));

        // The peer, named the way a phone names it: the device id of the key the
        // machine publishes as its dial key (`daemon_key`), which is the id it
        // dialed the relay with. Not the row's `machine_id` — that is the
        // machine's directory identity, and the relay routes by the dialing id.
        let peer = relay_peer_parse(daemon_id.display_id()).expect("a device id");

        let series = session
            .metrics_history(
                Arc::clone(&peer),
                daemon_key_hex.clone(),
                "pane-1".to_string(),
                1_759_999_784_000,
                u64::MAX,
                1_000,
            )
            .await
            .expect("the machine answers");

        assert_eq!(
            series.v,
            codec_protocol_version(),
            "the answer carries the protocol version it speaks"
        );
        assert_eq!(
            series.step_ms, 60_000,
            "the tier the machine served, not the tier that was asked for"
        );
        assert!(
            series.downshifted,
            "a coarser tier than the ask must say so: the meter draws what it is told"
        );
        assert_eq!(series.rows.len(), 2);
        assert_eq!(
            series.rows[0],
            WireMetricsPoint {
                ts_ms: 1_760_000_000_000,
                rss_avg: 4 * 1024 * 1024,
                rss_peak: 9 * 1024 * 1024,
                cpu_avg: 1.5,
                cpu_peak: 7.25,
                pids: 12,
            },
            "the point crosses field for field"
        );
        assert!(
            series.rows[1].ts_ms > series.rows[0].ts_ms,
            "the series arrives oldest first, the order the daemon sends"
        );
        assert_eq!(series.rows[1].rss_peak, 10 * 1024 * 1024);

        // A refusal is an answer, and it does not cost the conversation: the
        // machine read the verb and said no, so the channel is still in step and
        // the reads below stay on it (the fixture accepts exactly one handshake,
        // so a client that reconnected here would stall rather than pass).
        let refusal = session
            .metrics_history(
                Arc::clone(&peer),
                daemon_key_hex.clone(),
                "pane-refused".to_string(),
                1_759_999_784_000,
                u64::MAX,
                1_000,
            )
            .await
            .expect_err("a refusal is not an answer");
        assert!(
            matches!(refusal, SessionFfiError::Daemon(_)),
            "the machine's own sentence, not a session failure: {refusal:?}"
        );
        assert_eq!(
            format!("{refusal}"),
            "no history for that pane in this window"
        );

        // The second read, on the conversation the first one opened: a pane with
        // no history in the window is an empty list, which is a state a meter
        // renders. This is the case a real daemon breaks if the client hands the
        // verb to a *new* conversation — the daemon is still holding the first
        // one, so the new handshake lands in the stale stream and the read fails
        // (T-0114's p1).
        let empty = session
            .metrics_history(
                Arc::clone(&peer),
                daemon_key_hex.clone(),
                "pane-2".to_string(),
                1_759_999_784_000,
                u64::MAX,
                10_000,
            )
            .await
            .expect("an empty window is an answer, not a failure");
        assert!(
            empty.rows.is_empty(),
            "a pane that just started has no history, and that is an empty list"
        );
        assert_eq!(empty.step_ms, 10_000);
        assert!(
            !empty.downshifted,
            "nothing was downshifted, so nothing claims to have been"
        );

        // The question is the CLI's own, field for field: the same verb, the
        // same "to now" sentinel, and the tier *asked for* — which is what lets
        // the reply report the downshift. (The machine is still answering, so
        // `seen` is read once it has finished — below, after the third read.)
        // **The pin, falsified where it can actually fail.** A *valid* public
        // key that is not this machine's — another device's — must be refused.
        // The malformed-key case below cannot prove this: `verifying_key_from_hex`
        // rejects it before any stream exists, so it would pass unchanged if the
        // read ignored `server_key` entirely and trusted whatever the relay's
        // directory said. This one names a key the machine does not hold, and the
        // refusal is a `Peer` failure — the conversation that exists with this
        // peer was proven under the machine's key, and a call naming another
        // key cannot be served on it.
        //
        // *Refused*, not merely failed: the read below with the pinned key still
        // works and takes the machine's third answer, which is only possible if
        // this call never reached the machine. A check that let it through would
        // consume that answer here and leave the next read with nothing.
        let stranger = device_key_from_seed(OTHER_DEVICE_SEED.to_vec()).expect("a key");
        let refusal = session
            .metrics_history(
                Arc::clone(&peer),
                stranger.public_hex(),
                "pane-1".to_string(),
                0,
                u64::MAX,
                0,
            )
            .await
            .expect_err("a key the machine does not hold is refused");
        assert!(
            matches!(refusal, SessionFfiError::Peer(_)),
            "a wrong-but-valid pinned key is a peer failure, not a served read: {refusal:?}"
        );

        let after = session
            .metrics_history(
                Arc::clone(&peer),
                daemon_key_hex.clone(),
                "pane-3".to_string(),
                1_759_999_784_000,
                u64::MAX,
                1_000,
            )
            .await
            .expect("the conversation survives a refused key");
        assert_eq!(
            after.rows.len(),
            1,
            "the machine's third answer is still there to be taken"
        );
        assert_eq!(after.rows[0].pids, 14);

        // What the machine saw, read once it has finished answering: four
        // questions, in order, and never the refused key's. `pane-3` arriving at
        // all is the proof that the wrong-key call was refused on this side — a
        // read that reached the machine would have taken this answer.
        let seen = machine.await.expect("the machine task finishes");
        match &seen[0] {
            Message::MetricsHistory {
                v,
                id,
                since_ms,
                until_ms,
                step_ms,
            } => {
                assert_eq!(*v, codec_protocol_version());
                assert_eq!(id, "pane-1");
                assert_eq!(*since_ms, 1_759_999_784_000);
                assert_eq!(*until_ms, u64::MAX, "the CLI's open-ended \"to now\"");
                assert_eq!(*step_ms, 1_000);
            }
            other => panic!("the boundary must ask MetricsHistory, got {other:?}"),
        }
        assert!(
            matches!(&seen[1], Message::MetricsHistory { id, .. } if id == "pane-refused"),
            "the refused verb went to the machine and no further: {:?}",
            seen[1]
        );
        assert_eq!(
            seen.len(),
            4,
            "one question per read and none from the refused key: {seen:?}"
        );
        assert!(
            matches!(&seen[3], Message::MetricsHistory { id, .. } if id == "pane-3"),
            "the last question is the one the pinned key asked: {:?}",
            seen[3]
        );

        // A pinned key that is not a public key is refused by name, and by the
        // core's own sentence — before any stream exists (the machine above is
        // gone, and this still answers). The words are compared against the
        // core's rendering rather than a copy, so the two cannot drift.
        let refusal = session
            .metrics_history(
                Arc::clone(&peer),
                "not a key".to_string(),
                "pane-1".to_string(),
                0,
                u64::MAX,
                0,
            )
            .await
            .expect_err("a malformed pinned key is refused");
        assert!(
            matches!(refusal, SessionFfiError::BadPeerKey(_)),
            "the boundary's own precondition, not a session failure: {refusal:?}"
        );
        assert_eq!(
            format!("{refusal}"),
            arreo_core::identity::verifying_key_from_hex("not a key")
                .expect_err("the core refuses it")
                .to_string()
        );
    });
}

/// The act door: a phone answers a blocked agent from its notification, and
/// every refusal is the machine's own sentence.
///
/// T-0094 built the whole path — `Message::NotifyAct` with the bounded three
/// actions, the daemon's single act path, the pane's state as the authority on
/// refusals, the audit row — and the CLI and the TUI are its two callers. This is
/// the third, over the same boundary the metrics read crosses and the same
/// one-conversation-per-peer cache: a phone that polls a meter and then answers a
/// question does both on one handshake.
///
/// All three actions cross, because the daemon's gate treats them differently
/// (`serve_notify_act`'s caller: `reply`/`skip` are `Verb::Send`, `kill` is
/// `Verb::Kill`), and so does every refusal — the pane's state, the state gate and
/// the reply bound are the daemon's sentences, carried byte for byte into
/// [`SessionFfiError::Daemon`]. The sentences the fixture returns are built the
/// way the daemon builds them (`notify::state_word`, `notify::MAX_REPLY_BYTES`,
/// the pinned `notify::PANE_EXITED`), so this asserts the crossing rather than a
/// second spelling of it.
///
/// The phone is an **owner** because that is what acting needs:
/// `Capability::Control` (`identity::role::required`), which only the owner role
/// holds. `a_viewer_cannot_answer_a_notification` is the other half of that.
#[test]
fn the_session_acts_on_a_notification_over_one_conversation() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    runtime.block_on(async {
        let root = Arc::new(arreo_core::identity::RootKey::from_seed(SERVER_SEED));
        let relay = TestRelay::start(ACCOUNT.to_string(), Arc::clone(&root), Vec::new()).await;
        let root_handle = root_key_from_seed(SERVER_SEED.to_vec()).expect("a root");

        let phone = device_key_from_seed(PHONE_SEED.to_vec()).expect("a key");
        let phone_cert = device_cert_issue(
            Arc::clone(&root_handle),
            phone.public_hex(),
            "pixel-7".to_string(),
            FfiRole::Owner,
            1_760_000_000_000,
            1,
        )
        .expect("a certificate");
        let session = relay_session_dial(
            relay.address(),
            ACCOUNT.to_string(),
            Arc::clone(&phone),
            phone_cert,
        )
        .await
        .expect("the relay accepts the phone");
        relay.await_route(&phone.fingerprint()).await;

        let daemon_key = DeviceKey::from_seed(DAEMON_SEED);
        let daemon_id = DeviceId::from_key(&daemon_key.public());
        let daemon_cert = device_cert_issue(
            Arc::clone(&root_handle),
            daemon_key.public_hex(),
            "server-box".to_string(),
            FfiRole::Owner,
            1_760_000_000_000,
            2,
        )
        .expect("a certificate");
        let daemon_cert =
            DeviceCert::decode(&daemon_cert.encode().expect("the certificate encodes"))
                .expect("the core reads its own certificate");
        let daemon = RelaySession::dial(relay.addr, ACCOUNT, &daemon_key, &daemon_cert)
            .await
            .expect("the relay accepts the daemon");
        relay.await_route(daemon_id.as_str()).await;

        // The daemon's own sentences, built by the daemon's own rules rather than
        // typed here: the state word is `notify::state_word`'s, the bound is
        // `notify::MAX_REPLY_BYTES`, and the exited sentence is the pinned
        // `notify::PANE_EXITED` the CLI keys its exit code on. A refusal that this
        // boundary invented could not equal these.
        let not_asking = format!(
            "cannot reply: the pane is not asking (state={})",
            arreo_core::notify::state_word(AgentState::Working)
        );
        let too_long_text = "x".repeat(MAX_REPLY_BYTES + 1);
        let too_long = format!(
            "reply text is too long: {} bytes, the bound is {}",
            too_long_text.len(),
            MAX_REPLY_BYTES
        );

        let answers = vec![
            // A read first, so the conversation is shared across *both* kinds of
            // daemon verb: the cache is per peer, not per verb.
            Message::MetricsSeries {
                v: VERSION,
                id: "pane-1".to_string(),
                step_ms: 10_000,
                downshifted: false,
                rows: vec![MetricsPoint {
                    ts_ms: 1_760_000_000_000,
                    rss_avg: 4 * 1024 * 1024,
                    rss_peak: 9 * 1024 * 1024,
                    cpu_avg: 1.5,
                    cpu_peak: 7.25,
                    pids: 12,
                }],
            },
            // The reply the operator sent: taken.
            Message::NotifyActReply {
                v: VERSION,
                ok: true,
                detail: String::new(),
            },
            // A dismissal: taken, and it writes no pane bytes.
            Message::NotifyActReply {
                v: VERSION,
                ok: true,
                detail: String::new(),
            },
            // The pane has since exited — the pane's own word, pinned.
            Message::NotifyActReply {
                v: VERSION,
                ok: false,
                detail: PANE_EXITED.to_string(),
            },
            // The state gate: a reply needs a pane the engine sees asking.
            Message::NotifyActReply {
                v: VERSION,
                ok: false,
                detail: not_asking.clone(),
            },
            // The bound: a refusal, never a truncation.
            Message::NotifyActReply {
                v: VERSION,
                ok: false,
                detail: too_long.clone(),
            },
            // Text on a non-reply: the daemon refuses it rather than guessing.
            Message::NotifyActReply {
                v: VERSION,
                ok: false,
                detail: "--text applies only to reply".to_string(),
            },
            // The kill, taken — after five verbs and three refusals, on the
            // conversation the read opened: a refusal is an *answer*, so it costs
            // the conversation nothing.
            Message::NotifyActReply {
                v: VERSION,
                ok: true,
                detail: String::new(),
            },
        ];

        let daemon_key_hex = daemon_key.public_hex();
        let phone_key =
            arreo_core::identity::verifying_key_from_hex(&phone.public_hex()).expect("hex");
        let machine = tokio::spawn(serve_daemon(daemon, daemon_key, phone_key, answers));
        let peer = relay_peer_parse(daemon_id.display_id()).expect("a device id");

        let series = session
            .metrics_history(
                Arc::clone(&peer),
                daemon_key_hex.clone(),
                "pane-1".to_string(),
                1_759_999_784_000,
                u64::MAX,
                10_000,
            )
            .await
            .expect("the machine answers the read");
        assert_eq!(series.rows.len(), 1);

        // `reply`, with the operator's text. The boundary does not append the
        // newline and does not redact: the daemon sends `text + "\n"` through the
        // audited send path (T-0094), and a client that mangled the bytes would
        // change what the agent reads.
        session
            .notify_act(
                Arc::clone(&peer),
                daemon_key_hex.clone(),
                "pane-1".to_string(),
                WireNotifyAction::Reply,
                Some(REPLY_TEXT.to_string()),
            )
            .await
            .expect("the machine takes the reply");

        // `skip`: a dismissal, no pane bytes.
        session
            .notify_act(
                Arc::clone(&peer),
                daemon_key_hex.clone(),
                "pane-1".to_string(),
                WireNotifyAction::Skip,
                None,
            )
            .await
            .expect("the machine takes the skip");

        // `kill` on a pane that has exited: the pinned sentence, as the typed
        // refusal. The CLI keys its exit code on exactly these bytes, so a phone
        // and a terminal cannot disagree about why the kill did not happen.
        let refusal = session
            .notify_act(
                Arc::clone(&peer),
                daemon_key_hex.clone(),
                "pane-1".to_string(),
                WireNotifyAction::Kill,
                None,
            )
            .await
            .expect_err("a pane that has exited cannot be killed");
        assert_eq!(
            refusal,
            SessionFfiError::Daemon(PANE_EXITED.to_string()),
            "the pane's own word, byte for byte, typed as the machine's refusal"
        );

        // `reply` on a pane that is not asking: the state gate's sentence, which
        // names the state the engine saw.
        let refusal = session
            .notify_act(
                Arc::clone(&peer),
                daemon_key_hex.clone(),
                "pane-1".to_string(),
                WireNotifyAction::Reply,
                Some("y".to_string()),
            )
            .await
            .expect_err("a pane that is not asking cannot be answered");
        assert_eq!(
            refusal,
            SessionFfiError::Daemon(not_asking.clone()),
            "the gate's sentence, naming the state it saw"
        );

        // `reply` past the bound: refused, and the sentence names the core's
        // constant — the one definition the CLI checks and the daemon enforces.
        let refusal = session
            .notify_act(
                Arc::clone(&peer),
                daemon_key_hex.clone(),
                "pane-1".to_string(),
                WireNotifyAction::Reply,
                Some(too_long_text.clone()),
            )
            .await
            .expect_err("text past the bound is refused, never truncated");
        assert_eq!(
            refusal,
            SessionFfiError::Daemon(too_long.clone()),
            "the bound's refusal is the daemon's, and it names {MAX_REPLY_BYTES} bytes"
        );

        // Text on a `skip`: the boundary passes the caller's arguments through and
        // lets the daemon refuse — it does not silently drop the text, which would
        // hide a caller's bug behind a successful dismissal.
        let refusal = session
            .notify_act(
                Arc::clone(&peer),
                daemon_key_hex.clone(),
                "pane-1".to_string(),
                WireNotifyAction::Skip,
                Some("y".to_string()),
            )
            .await
            .expect_err("text on a skip is refused");
        assert_eq!(
            refusal,
            SessionFfiError::Daemon("--text applies only to reply".to_string())
        );

        session
            .notify_act(
                Arc::clone(&peer),
                daemon_key_hex.clone(),
                "pane-1".to_string(),
                WireNotifyAction::Kill,
                None,
            )
            .await
            .expect("the machine takes the kill");

        // What the machine saw, read once it has finished: eight verbs on the one
        // conversation the read opened — a second handshake would have reached no
        // accept door (the fixture accepts exactly one), which is the property
        // T-0114's p1 turned on.
        let seen = machine.await.expect("the machine task finishes");
        assert_eq!(
            seen.len(),
            8,
            "one verb per call, all on one conversation: {seen:?}"
        );
        assert_eq!(
            seen[0],
            Message::MetricsHistory {
                v: VERSION,
                id: "pane-1".to_string(),
                since_ms: 1_759_999_784_000,
                until_ms: u64::MAX,
                step_ms: 10_000,
            }
        );
        assert_eq!(
            seen[1],
            Message::NotifyAct {
                v: VERSION,
                pane: "pane-1".to_string(),
                action: NotifyAction::Reply,
                text: Some(REPLY_TEXT.to_string()),
            },
            "the action and the operator's text cross field for field"
        );
        assert_eq!(
            seen[2],
            Message::NotifyAct {
                v: VERSION,
                pane: "pane-1".to_string(),
                action: NotifyAction::Skip,
                text: None,
            },
            "a skip must not arrive as a reply: the gate treats them differently"
        );
        assert_eq!(
            seen[3],
            Message::NotifyAct {
                v: VERSION,
                pane: "pane-1".to_string(),
                action: NotifyAction::Kill,
                text: None,
            },
            "kill is its own action, and it is the one that ends the pane"
        );
        assert_eq!(
            seen[4],
            Message::NotifyAct {
                v: VERSION,
                pane: "pane-1".to_string(),
                action: NotifyAction::Reply,
                text: Some("y".to_string()),
            }
        );
        assert_eq!(
            seen[5],
            Message::NotifyAct {
                v: VERSION,
                pane: "pane-1".to_string(),
                action: NotifyAction::Reply,
                text: Some(too_long_text.clone()),
            },
            "the over-bound text went to the machine, which refused it — the \
             boundary does not pre-check the bound and invent its own sentence"
        );
        assert_eq!(
            seen[6],
            Message::NotifyAct {
                v: VERSION,
                pane: "pane-1".to_string(),
                action: NotifyAction::Skip,
                text: Some("y".to_string()),
            },
            "the text is passed through, not dropped, so the daemon can refuse it"
        );
        assert_eq!(
            seen[7],
            Message::NotifyAct {
                v: VERSION,
                pane: "pane-1".to_string(),
                action: NotifyAction::Kill,
                text: None,
            }
        );
    });
}

/// A phone admitted as a **viewer** cannot answer, and the refusal is the gate's
/// sentence rather than this boundary's.
///
/// `reply` and `skip` are `Verb::Send` and `kill` is `Verb::Kill` (the daemon's
/// `verb_of`), and both verbs need `Capability::Control`, which only the owner
/// role holds (`identity::role::required`). So the act a viewer attempts is the
/// one refusal a phone actually meets on this path, and it arrives as
/// `Message::Error` carrying `RoleError::Denied`'s sentence — the same bytes the
/// CLI prints for `arreo notify act` against a machine that admitted it as a
/// viewer.
///
/// The assertion that carries the weight is the last one: **the verb reached the
/// machine.** A boundary that refused the act itself (a client-side role check)
/// would never send it, and the phone would be showing a sentence the daemon never
/// said.
#[test]
fn a_viewer_cannot_answer_a_notification() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    runtime.block_on(async {
        let root = Arc::new(arreo_core::identity::RootKey::from_seed(SERVER_SEED));
        let relay = TestRelay::start(ACCOUNT.to_string(), Arc::clone(&root), Vec::new()).await;
        let root_handle = root_key_from_seed(SERVER_SEED.to_vec()).expect("a root");

        let phone = device_key_from_seed(PHONE_SEED.to_vec()).expect("a key");
        let phone_cert = device_cert_issue(
            Arc::clone(&root_handle),
            phone.public_hex(),
            "pixel-7".to_string(),
            FfiRole::Viewer,
            1_760_000_000_000,
            1,
        )
        .expect("a certificate");
        let session = relay_session_dial(
            relay.address(),
            ACCOUNT.to_string(),
            Arc::clone(&phone),
            phone_cert,
        )
        .await
        .expect("the relay accepts the phone");
        relay.await_route(&phone.fingerprint()).await;

        let daemon_key = DeviceKey::from_seed(DAEMON_SEED);
        let daemon_id = DeviceId::from_key(&daemon_key.public());
        let daemon_cert = device_cert_issue(
            Arc::clone(&root_handle),
            daemon_key.public_hex(),
            "server-box".to_string(),
            FfiRole::Owner,
            1_760_000_000_000,
            2,
        )
        .expect("a certificate");
        let daemon_cert =
            DeviceCert::decode(&daemon_cert.encode().expect("the certificate encodes"))
                .expect("the core reads its own certificate");
        let daemon = RelaySession::dial(relay.addr, ACCOUNT, &daemon_key, &daemon_cert)
            .await
            .expect("the relay accepts the daemon");
        relay.await_route(daemon_id.as_str()).await;

        // The gate's sentence, asked of the core's own policy rather than spelled
        // here: the daemon answers a refusal with `denial.to_string()`
        // (`SessionAuth::authorize`), so this is the byte-for-byte answer a real
        // machine sends a viewer.
        let denial = arreo_core::identity::role::check(
            arreo_core::identity::Role::Viewer,
            arreo_core::identity::role::Verb::Send,
        )
        .expect_err("a viewer may not send")
        .to_string();
        assert!(
            denial.contains("viewer"),
            "the refusal names the role: {denial}"
        );

        let answers = vec![Message::Error {
            v: VERSION,
            message: denial.clone(),
        }];
        let daemon_key_hex = daemon_key.public_hex();
        let phone_key =
            arreo_core::identity::verifying_key_from_hex(&phone.public_hex()).expect("hex");
        let machine = tokio::spawn(serve_daemon(daemon, daemon_key, phone_key, answers));
        let peer = relay_peer_parse(daemon_id.display_id()).expect("a device id");

        let refusal = session
            .notify_act(
                Arc::clone(&peer),
                daemon_key_hex,
                "pane-1".to_string(),
                WireNotifyAction::Reply,
                Some("y".to_string()),
            )
            .await
            .expect_err("a viewer's act is refused by the machine");
        assert_eq!(
            refusal,
            SessionFfiError::Daemon(denial.clone()),
            "the gate's own sentence, carried rather than restated"
        );

        let seen = machine.await.expect("the machine task finishes");
        assert_eq!(
            seen.len(),
            1,
            "the refused act is a verb the machine saw: {seen:?}"
        );
        assert!(
            matches!(&seen[0], Message::NotifyAct { .. }),
            "the boundary sent the act and let the machine refuse it, rather than \
             refusing it itself: {:?}",
            seen[0]
        );
    });
}

/// One side of an accepted conversation: the read half, the write half, and the
/// bytes already read off the wire.
type Conversation = (
    tokio::io::ReadHalf<SecureChannel>,
    tokio::io::WriteHalf<SecureChannel>,
    Vec<u8>,
);

/// One accepted conversation: the handshake, and the channel it opened.
///
/// The daemon half of every fixture here, factored out because the recovery case
/// needs it twice on one session.
async fn accept_conversation(
    session: &mut RelaySession,
    key: &DeviceKey,
    phone: VerifyingKey,
) -> Conversation {
    let peer = tokio::time::timeout(Duration::from_secs(10), session.next_peer())
        .await
        .expect("the phone's bytes reach the daemon")
        .expect("the session stays open");
    let stream = session.stream_to(&peer);
    let guard = FlightGuard::default();
    let local = key.noise_static();
    let (channel, device) = SecureChannel::accept(stream, &local, &guard, move |_| Some(phone))
        .await
        .expect("the phone's Noise handshake completes");
    assert_eq!(
        device,
        DeviceId::from_key(&phone),
        "the daemon is told which device dialed"
    );
    let (mut reader, mut writer) = tokio::io::split(channel);
    let mut buf = Vec::new();
    assert!(
        matches!(
            read_daemon_message(&mut reader, &mut buf).await,
            Message::Hello { .. }
        ),
        "the daemon protocol opens with Hello"
    );
    write_daemon_message(
        &mut writer,
        &Message::Welcome {
            v: VERSION,
            server: "arreo-server-test".to_string(),
        },
    )
    .await;
    (reader, writer, buf)
}

/// The machine's half of a *recovered* conversation: one session that ends
/// badly, then the one the phone opens next.
///
/// This is what the daemon's own accept loop does once a session ends: its stream
/// is dropped, the relay session's read pump parks whatever arrives next and
/// announces the peer again, and `serve_peer` runs a handshake over a fresh
/// stream. The client's half is what the test asserts — the call *after* a failed
/// one must arrive as a new conversation, not as another verb on a channel that is
/// already dead.
///
/// Phase one answers the handshake and one verb, then writes a frame the codec
/// refuses (which is what fails the client's read), then lets the stream go.
/// Phase two is an ordinary conversation, and the verb it is asked is returned.
async fn serve_recovered_daemon(
    mut session: RelaySession,
    key: DeviceKey,
    phone: VerifyingKey,
) -> Message {
    let (mut reader, mut writer, mut buf) = accept_conversation(&mut session, &key, phone).await;
    let _ = read_daemon_message(&mut reader, &mut buf).await;
    let declared = ((codec::MAX_FRAME_BYTES + 1) as u32).to_le_bytes();
    writer
        .write_all(&declared)
        .await
        .expect("the bytes leave the daemon");
    writer.flush().await.expect("the bytes flush");
    let _ = writer.shutdown().await;
    tokio::time::sleep(FINAL_FRAME_GRACE).await;
    drop((reader, writer, buf));

    // The next conversation, which only exists if the client came back with a
    // handshake rather than another verb on the dead channel.
    let (mut reader, mut writer, mut buf) = accept_conversation(&mut session, &key, phone).await;
    let asked = read_daemon_message(&mut reader, &mut buf).await;
    write_daemon_message(
        &mut writer,
        &Message::MetricsSeries {
            v: VERSION,
            id: "pane-1".to_string(),
            step_ms: 10_000,
            downshifted: false,
            rows: vec![MetricsPoint {
                ts_ms: 1_760_000_180_000,
                rss_avg: 7 * 1024 * 1024,
                rss_peak: 12 * 1024 * 1024,
                cpu_avg: 4.0,
                cpu_peak: 9.5,
                pids: 15,
            }],
        },
    )
    .await;
    let _ = writer.shutdown().await;
    tokio::time::sleep(FINAL_FRAME_GRACE).await;
    asked
}

/// A failed read does not leave the conversation dead: the next one reconnects.
///
/// The rule the cache carries (T-0114's decided fix, point 3): a call that fails
/// drops the conversation, so the *next* call opens a fresh one instead of writing
/// another verb into a channel the machine has already let go of. Without it a
/// phone's meter would never recover — every later poll would fail the same way,
/// on the same dead conversation — which is why the recovery is "drop and
/// reconnect", not "retry inside the call".
///
/// The machine here ends its first session the way a daemon does when its loop
/// returns, and accepts a second one. The client is given the pause a poll's
/// cadence gives for free: a daemon needs its old stream to be gone before the
/// next handshake reaches its accept door (the stale-stream race T-0054 records).
#[test]
fn a_failed_read_reconnects_rather_than_reusing_the_dead_conversation() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    runtime.block_on(async {
        let root = Arc::new(arreo_core::identity::RootKey::from_seed(SERVER_SEED));
        let relay = TestRelay::start(ACCOUNT.to_string(), Arc::clone(&root), Vec::new()).await;
        let root_handle = root_key_from_seed(SERVER_SEED.to_vec()).expect("a root");

        let phone = device_key_from_seed(PHONE_SEED.to_vec()).expect("a key");
        let phone_cert = device_cert_issue(
            Arc::clone(&root_handle),
            phone.public_hex(),
            "pixel-7".to_string(),
            FfiRole::Viewer,
            1_760_000_000_000,
            1,
        )
        .expect("a certificate");
        let session = relay_session_dial(
            relay.address(),
            ACCOUNT.to_string(),
            Arc::clone(&phone),
            phone_cert,
        )
        .await
        .expect("the relay accepts the phone");
        relay.await_route(&phone.fingerprint()).await;

        let daemon_key = DeviceKey::from_seed(DAEMON_SEED);
        let daemon_id = DeviceId::from_key(&daemon_key.public());
        let daemon_cert = device_cert_issue(
            Arc::clone(&root_handle),
            daemon_key.public_hex(),
            "server-box".to_string(),
            FfiRole::Owner,
            1_760_000_000_000,
            2,
        )
        .expect("a certificate");
        let daemon_cert =
            DeviceCert::decode(&daemon_cert.encode().expect("the certificate encodes"))
                .expect("the core reads its own certificate");
        let daemon = RelaySession::dial(relay.addr, ACCOUNT, &daemon_key, &daemon_cert)
            .await
            .expect("the relay accepts the daemon");
        relay.await_route(daemon_id.as_str()).await;

        let daemon_key_hex = daemon_key.public_hex();
        let phone_key =
            arreo_core::identity::verifying_key_from_hex(&phone.public_hex()).expect("hex");
        let machine = tokio::spawn(serve_recovered_daemon(daemon, daemon_key, phone_key));
        let peer = relay_peer_parse(daemon_id.display_id()).expect("a device id");

        // The first read fails on the machine's own answer: a frame the codec
        // refuses. The machine's session ends with it.
        let first = session
            .metrics_history(
                Arc::clone(&peer),
                daemon_key_hex.clone(),
                "pane-1".to_string(),
                0,
                u64::MAX,
                0,
            )
            .await
            .expect_err("a frame the codec refuses is not an answer");
        assert!(
            matches!(first, SessionFfiError::Peer(_)),
            "the channel's failure, not the machine's: {first:?}"
        );

        // The client's pause, and a daemon's own timing: its stream has to be
        // gone before the next handshake reaches its accept door.
        tokio::time::sleep(Duration::from_secs(2)).await;

        let second = session
            .metrics_history(
                Arc::clone(&peer),
                daemon_key_hex.clone(),
                "pane-1".to_string(),
                0,
                u64::MAX,
                0,
            )
            .await
            .expect("the read after a failure opens a fresh conversation");
        assert_eq!(second.rows.len(), 1);
        assert_eq!(second.rows[0].pids, 15);

        assert!(
            matches!(
                machine.await.expect("the machine task finishes"),
                Message::MetricsHistory { .. }
            ),
            "the second read was a verb on a conversation the machine accepted"
        );
    });
}

/// A frame that cannot be read is refused, not waited out.
///
/// The machine's length prefix names a body over the codec's budget, so the codec
/// says exactly that (`frame declares N bytes, over the M-byte budget`) instead of
/// "truncated" — and the read must answer with that sentence, the one the CLI's own
/// client renders for the same bytes, rather than appending whatever arrives next
/// until the reply bound expires. A client that waited would spend five seconds of
/// a phone's spinner on an answer that can never become readable; the corruption
/// and the slow frame are different facts and the codec already draws the line
/// between them.
///
/// The bound in the assertion is the reply bound itself, not a stopwatch reading:
/// the point is that the refusal comes from the codec rather than from a timeout.
#[test]
fn an_over_budget_frame_is_refused_rather_than_waited_out() {
    let runtime = tokio::runtime::Runtime::new().expect("a runtime");
    runtime.block_on(async {
        let root = Arc::new(arreo_core::identity::RootKey::from_seed(SERVER_SEED));
        let relay = TestRelay::start(ACCOUNT.to_string(), Arc::clone(&root), Vec::new()).await;
        let root_handle = root_key_from_seed(SERVER_SEED.to_vec()).expect("a root");

        let phone = device_key_from_seed(PHONE_SEED.to_vec()).expect("a key");
        let phone_cert = device_cert_issue(
            Arc::clone(&root_handle),
            phone.public_hex(),
            "pixel-7".to_string(),
            FfiRole::Viewer,
            1_760_000_000_000,
            1,
        )
        .expect("a certificate");
        let session = relay_session_dial(
            relay.address(),
            ACCOUNT.to_string(),
            Arc::clone(&phone),
            phone_cert,
        )
        .await
        .expect("the relay accepts the phone");
        relay.await_route(&phone.fingerprint()).await;

        let daemon_key = DeviceKey::from_seed(DAEMON_SEED);
        let daemon_id = DeviceId::from_key(&daemon_key.public());
        let daemon_cert = device_cert_issue(
            Arc::clone(&root_handle),
            daemon_key.public_hex(),
            "server-box".to_string(),
            FfiRole::Owner,
            1_760_000_000_000,
            2,
        )
        .expect("a certificate");
        let daemon_cert =
            DeviceCert::decode(&daemon_cert.encode().expect("the certificate encodes"))
                .expect("the core reads its own certificate");
        let daemon = RelaySession::dial(relay.addr, ACCOUNT, &daemon_key, &daemon_cert)
            .await
            .expect("the relay accepts the daemon");
        relay.await_route(daemon_id.as_str()).await;

        // One byte more than the codec will ever read.
        let declared = ((codec_max_frame_bytes() + 1) as u32).to_le_bytes();
        let daemon_key_hex = daemon_key.public_hex();
        let machine = tokio::spawn(serve_a_frame_no_client_can_read(
            daemon,
            daemon_key,
            arreo_core::identity::verifying_key_from_hex(&phone.public_hex()).expect("hex"),
            declared,
        ));

        let peer = relay_peer_parse(daemon_id.display_id()).expect("a device id");
        let started = std::time::Instant::now();
        let refusal = session
            .metrics_history(
                Arc::clone(&peer),
                daemon_key_hex,
                "pane-1".to_string(),
                0,
                u64::MAX,
                0,
            )
            .await
            .expect_err("an unreadable frame is not an answer");
        assert!(
            matches!(refusal, SessionFfiError::Peer(_)),
            "a frame the codec refuses is a peer failure: {refusal:?}"
        );
        assert_eq!(
            format!("{refusal}"),
            arreo_core::mesh::MeshClientError::Codec(
                codec::frame_body_len(&declared)
                    .expect_err("the codec refuses the declared length")
                    .to_string()
            )
            .to_string(),
            "the codec's own sentence, not the reply bound's"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the refusal is the codec's, so it does not wait out the reply bound (took {:?})",
            started.elapsed()
        );
        assert!(
            matches!(
                machine.await.expect("the machine task finishes"),
                Message::MetricsHistory { .. }
            ),
            "the verb was asked once, before the answer was refused"
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
