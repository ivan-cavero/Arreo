//! Remote transport wiring for the daemon (T-0023).
//!
//! One sentence: this is where a Noise-QUIC session from a pinned device
//! becomes an ordinary daemon session — the same `serve_session` loop the Unix
//! socket uses, plus a per-verb authorization gate, because the peer is not on
//! this machine.
//!
//! **Zero inbound ports is the shipped posture** (ROADMAP §4). This module opens
//! a listener only when `ARREO_TRANSPORT_TEST_LISTEN` names an address, and that
//! is a *test seam*: the production remote path is the daemon dialling out to a
//! relay (T-0029), so a deployment that sets this variable has changed the
//! product's network posture and should know it.
//!
//! What the daemon contributes to the handshake:
//! - its **static identity** is the server root key, so the key a paired device
//!   pinned during `arreo pair` is the key it authenticates here (one identity,
//!   not a second transport key to distribute — ADR 0011);
//! - the **resolver**: the announced device id must name a pinned, non-revoked,
//!   non-retired device, otherwise the connection is refused before any
//!   cryptography runs;
//! - the **flight guard** and **rate limiter**, shared across connections, so a
//!   replay cannot be re-established and one peer cannot make the daemon do
//!   unbounded handshake work.

use crate::daemon::{serve_session, Registry, SessionAuth};
use crate::devices::DeviceAuthority;
use arreo_core::identity::keys::{NoiseStatic, RootKey};
use arreo_core::identity::{DeviceId, VerifyingKey};
/// The environment variable that opens the loopback listener. Re-exported so a
/// deployment reads it from the same place the transport does.
pub use arreo_core::transport::TEST_LISTEN_ENV;
use arreo_core::transport::{
    accept_connection, accept_session, server_endpoint, Endpoint, FlightGuard, HandshakeLimiter,
    QuicError,
};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The loopback listener address, if the test seam is set.
///
/// A malformed value is reported and treated as unset: silently serving on a
/// guessed address would be worse than not serving.
#[must_use]
pub fn test_listen_addr() -> Option<SocketAddr> {
    let raw = std::env::var(TEST_LISTEN_ENV).ok()?;
    match raw.trim().parse::<SocketAddr>() {
        Ok(addr) => Some(addr),
        Err(e) => {
            eprintln!(
                "arreo-server: {TEST_LISTEN_ENV}={raw:?} is not an IP:PORT address ({e}); \
                 remote transport disabled"
            );
            None
        }
    }
}

/// Where the server root key lives. The same path the authority and the CLI use.
#[must_use]
pub fn root_key_path() -> PathBuf {
    arreo_core::identity::identity_root().join("root.key")
}

/// The daemon's static transport identity: the root key's X25519 view.
///
/// One identity, not two: the key a device pinned during `arreo pair` is the key
/// it authenticates here (ADR 0011). Loaded by the composition root so the
/// transport itself never reaches for the filesystem, and so a failure to load
/// is reported where the operator can see it.
pub fn server_identity() -> Result<NoiseStatic, QuicError> {
    RootKey::load_or_generate(&root_key_path())
        .map(|root| root.noise_static())
        .map_err(|e| QuicError::Config(format!("server root key unavailable: {e}")))
}

/// Serve remote sessions until the endpoint closes.
pub async fn serve(
    endpoint: Endpoint,
    local: NoiseStatic,
    authority: Arc<Mutex<DeviceAuthority>>,
    registry: Registry,
    db: PathBuf,
) -> Result<(), QuicError> {
    // Shared across connections: the limiter is the per-peer handshake budget,
    // the guard is the replay memory, and both only mean anything if they span
    // connections.
    let limiter = Arc::new(HandshakeLimiter::default());
    let guard = Arc::new(FlightGuard::default());
    let local = Arc::new(local);

    loop {
        // Only the accept happens here; everything peer-paced moves into the
        // per-connection task below, so one quiet peer cannot stop the listener
        // from accepting the next device.
        let Some(connection) = accept_connection(&endpoint, &limiter).await? else {
            continue; // refused by the rate limiter; keep serving
        };
        let local = Arc::clone(&local);
        let limiter = Arc::clone(&limiter);
        let guard = Arc::clone(&guard);
        let authority = Arc::clone(&authority);
        let registry = Arc::clone(&registry);
        let db = db.clone();
        tokio::spawn(async move {
            let resolution = Arc::clone(&authority);
            let session = match accept_session(
                &connection,
                &local,
                &limiter,
                &guard,
                move |device: &DeviceId| pinned_key(&resolution, device),
            )
            .await
            {
                Ok(session) => session,
                Err(e) => {
                    eprintln!("arreo-server: remote handshake refused: {e}");
                    return;
                }
            };
            // The key the resolver named is the key the handshake
            // authenticated, so id and key agree; the authority re-checks both
            // per verb.
            let Some(peer) = pinned_key(&authority, &session.device) else {
                eprintln!(
                    "arreo-server: refusing a session for {} — no longer pinned",
                    session.device
                );
                return;
            };
            let auth = SessionAuth::new(Arc::clone(&authority), peer, session.device.clone());
            auth.touch();
            eprintln!("arreo-server: remote session from {}", session.device);

            let (reader, writer) = tokio::io::split(session.channel);
            if let Err(e) = serve_session(reader, writer, registry, db, Some(auth)).await {
                eprintln!("daemon: remote connection error: {e}");
            }
        });
    }
}

/// Say *why* a device was refused, using the store's record of it.
///
/// Best-effort and log-only: the refusal itself has already happened, and the
/// authority's own `authorize` writes the audit row for the paths that go
/// through it. This exists so the one path that cannot return a typed error (the
/// resolver's `Option`) still tells the operator the truth.
fn report_refusal(authority: &DeviceAuthority, device: &DeviceId) {
    let record = authority.record(device).unwrap_or_default();
    if let Some(record) = record {
        if let Err(denied) = arreo_core::identity::revocation::may_connect(&record) {
            eprintln!("arreo-server: refusing {}: {denied}", device.display_id());
        }
    }
}

/// The key pinned for `device`, or `None` if it is unknown, revoked or retired.
///
/// Used both as the handshake resolver and as the post-handshake re-check — and
/// by the relay session (T-0051), which authenticates peers the same way. One
/// implementation, because a second one is a second answer to "is this device
/// pinned".
pub(crate) fn pinned_key(
    authority: &Arc<Mutex<DeviceAuthority>>,
    device: &DeviceId,
) -> Option<VerifyingKey> {
    let authority = match authority.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    // The index, not the store listing: `check_verb` authorizes through the
    // index, and `reload` deliberately accepts a certificate file with no store
    // row. Asking the store here would refuse a device the gate would allow.
    let Some(record) = authority.device(device) else {
        // The index holds only authorized devices, so a miss here is "unknown,
        // revoked, or rotated away". The store can still tell which, and the
        // difference is the whole point of a log line: an operator who revoked a
        // phone must see *revoked*, not the misleading "not pinned" that a bare
        // `None` produces (T-0026).
        report_refusal(&authority, device);
        return None;
    };
    let key = VerifyingKey::from_bytes(&record.public_key).ok()?;
    // The record's id must match the key it carries: the store is only trusted
    // once the two agree, the same rule the authority itself applies.
    (DeviceId::from_key(&key) == *device).then_some(key)
}

/// Open the test listener on `addr` and serve remote sessions on it.
///
/// The loopback seam of [`test_listen_addr`]; returns the bound address so a
/// caller can report what it actually got (`:0` picks a free port).
pub async fn listen_on(
    addr: SocketAddr,
    local: NoiseStatic,
    authority: Arc<Mutex<DeviceAuthority>>,
    registry: Registry,
    db: PathBuf,
) -> Result<SocketAddr, QuicError> {
    let endpoint = server_endpoint(addr)?;
    let bound = endpoint.local_addr().map_err(QuicError::Io)?;
    tokio::spawn(async move {
        if let Err(e) = serve(endpoint, local, authority, registry, db).await {
            eprintln!("arreo-server: remote transport stopped: {e}");
        }
    });
    Ok(bound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::PaneEntry;
    use arreo_core::identity::authority::sidecar_db;
    use arreo_core::identity::role::Role;
    use arreo_core::identity::DeviceKey;
    use arreo_core::proto::codec;
    use arreo_core::proto::{Message, VERSION};
    use arreo_core::transport::{client_endpoint, open_session, SecureChannel};
    use std::collections::HashMap;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::RwLock;

    /// A scratch identity root + store + socket, mirroring the layout the
    /// authority uses in production (`<socket>.db`).
    struct Scratch {
        dir: PathBuf,
        layout: crate::devices::Layout,
    }

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "arreo-transport-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("scratch dir");
            let socket = dir.join("arreo.sock");
            let layout = crate::devices::Layout {
                root_key: dir.join("identity").join("root.key"),
                cert_dir: dir.join("identity").join("devices"),
                store: sidecar_db(&socket),
            };
            Self { dir, layout }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn send(channel: &mut SecureChannel, message: &Message) {
        let frame = codec::encode_frame(message).expect("encode");
        channel.write_all(&frame).await.expect("write");
        channel.flush().await.expect("flush");
    }

    async fn recv(channel: &mut SecureChannel, buf: &mut Vec<u8>) -> Message {
        loop {
            if let Ok((message, consumed)) = codec::decode_frame(buf) {
                buf.drain(..consumed);
                return message;
            }
            let mut chunk = [0u8; 8192];
            let n =
                tokio::time::timeout(std::time::Duration::from_secs(5), channel.read(&mut chunk))
                    .await
                    .expect("the server answers within the timeout")
                    .expect("read");
            assert!(n > 0, "the server closed before answering");
            buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Pin a device and bring up a loopback listener, returning everything a
    /// client needs.
    async fn serving(
        tag: &str,
        role: Role,
    ) -> (
        Scratch,
        Arc<Mutex<DeviceAuthority>>,
        DeviceKey,
        RootKey,
        SocketAddr,
    ) {
        let scratch = Scratch::new(tag);
        let mut authority = DeviceAuthority::load(scratch.layout.clone()).expect("authority");
        let device = DeviceKey::generate().expect("entropy");
        authority
            .issue("phone", role, &device.public())
            .expect("pin the device");
        let authority = Arc::new(Mutex::new(authority));

        // Same root key the authority bootstrapped, so the pins verify.
        let root = RootKey::load_or_generate(&scratch.layout.root_key).expect("root key");
        let registry: Registry = Arc::new(RwLock::new(HashMap::<String, Arc<PaneEntry>>::new()));
        let addr = listen_on(
            "127.0.0.1:0".parse().expect("loopback"),
            root.noise_static(),
            Arc::clone(&authority),
            registry,
            scratch.layout.store.clone(),
        )
        .await
        .expect("the listener binds");
        (scratch, authority, device, root, addr)
    }

    async fn connect(
        device: &DeviceKey,
        root: &RootKey,
        addr: SocketAddr,
    ) -> Result<SecureChannel, QuicError> {
        let endpoint = client_endpoint().expect("client endpoint");
        open_session(
            &endpoint,
            addr,
            &device.noise_static(),
            &DeviceId::from_key(&device.public()).display_id(),
            &root.public(),
        )
        .await
    }

    /// The acceptance criterion "wire into the daemon behind `check_verb`",
    /// exercised end to end over real sockets: a pinned viewer observes, is
    /// refused when it tries to drive, and its session is recorded.
    #[tokio::test]
    async fn a_pinned_viewer_observes_is_refused_writes_and_is_recorded() {
        let (scratch, authority, device, root, addr) = serving("viewer", Role::Viewer).await;
        let mut channel = connect(&device, &root, addr)
            .await
            .expect("the viewer connects");
        let mut buf = Vec::new();

        send(
            &mut channel,
            &Message::Hello {
                v: VERSION,
                client: "phone".to_string(),
                wants: vec![VERSION],
            },
        )
        .await;
        assert!(
            matches!(recv(&mut channel, &mut buf).await, Message::Welcome { .. }),
            "the remote path must speak the same Hello/Welcome handshake"
        );

        // Observing: allowed for a viewer.
        send(
            &mut channel,
            &Message::Panes {
                v: VERSION,
                panes: vec![],
            },
        )
        .await;
        assert!(
            matches!(recv(&mut channel, &mut buf).await, Message::Panes { .. }),
            "a viewer may list panes"
        );

        // Driving: refused by the policy, with the reason on the wire.
        send(
            &mut channel,
            &Message::Send {
                v: VERSION,
                id: "nope".to_string(),
                data: "rm -rf /".to_string(),
            },
        )
        .await;
        match recv(&mut channel, &mut buf).await {
            Message::Error { message, .. } => assert!(
                message.contains("viewer"),
                "the refusal must name the role: {message}"
            ),
            other => panic!("a viewer's send must be refused, got {other:?}"),
        }

        // The refusal did not end the session: observing still works.
        send(
            &mut channel,
            &Message::Panes {
                v: VERSION,
                panes: vec![],
            },
        )
        .await;
        assert!(matches!(
            recv(&mut channel, &mut buf).await,
            Message::Panes { .. }
        ));

        // The session was recorded against the device (`last_seen`).
        let id = DeviceId::from_key(&device.public());
        let record = authority
            .lock()
            .expect("authority")
            .devices()
            .into_iter()
            .find(|record| record.id == id)
            .expect("the device record");
        assert!(
            record.last_seen_ms.is_some(),
            "a remote session must be recorded against the device"
        );
        drop(scratch);
    }

    /// An owner may drive: the same path, the other half of the policy.
    #[tokio::test]
    async fn a_pinned_owner_may_drive() {
        let (_scratch, _authority, device, root, addr) = serving("owner", Role::Owner).await;
        let mut channel = connect(&device, &root, addr)
            .await
            .expect("the owner connects");
        let mut buf = Vec::new();

        send(
            &mut channel,
            &Message::Hello {
                v: VERSION,
                client: "phone".to_string(),
                wants: vec![VERSION],
            },
        )
        .await;
        assert!(matches!(
            recv(&mut channel, &mut buf).await,
            Message::Welcome { .. }
        ));

        // `send` to a pane that does not exist proves the gate let it through:
        // the answer is "no such pane", not "not allowed".
        send(
            &mut channel,
            &Message::Send {
                v: VERSION,
                id: "missing".to_string(),
                data: "x".to_string(),
            },
        )
        .await;
        match recv(&mut channel, &mut buf).await {
            Message::Error { message, .. } => assert!(
                message.contains("not found"),
                "an owner's send must reach dispatch: {message}"
            ),
            other => panic!("expected the dispatch answer, got {other:?}"),
        }
    }

    /// Open a session, complete the handshake, send one streaming verb, and
    /// return the first `Delta` it produces.
    ///
    /// A separate session per stream is the protocol's contract, not a test
    /// convenience: `Attach`/`Resume` own their connection until the child exits
    /// (`serve_session`), so a second verb queued behind one is never read.
    async fn stream_first_delta(
        device: &DeviceKey,
        root: &RootKey,
        addr: SocketAddr,
        verb: Message,
    ) -> Vec<String> {
        let mut channel = connect(device, root, addr)
            .await
            .expect("a streaming session");
        let mut buf = Vec::new();
        send(
            &mut channel,
            &Message::Hello {
                v: VERSION,
                client: "phone".to_string(),
                wants: vec![VERSION],
            },
        )
        .await;
        assert!(matches!(
            recv(&mut channel, &mut buf).await,
            Message::Welcome { .. }
        ));
        send(&mut channel, &verb).await;
        match recv(&mut channel, &mut buf).await {
            Message::Delta { lines, .. } => lines,
            other => panic!("a streaming verb must open with a delta, got {other:?}"),
        }
    }

    /// The acceptance criterion "snapshot → delta → resume with semantics
    /// identical to the local path": drive a *real pane* over the remote
    /// transport and read its output. This runs the same `serve_session` loop
    /// the Unix socket runs, so what it proves is that the remote path carries
    /// the whole protocol, not just the handshake.
    #[tokio::test]
    async fn a_remote_owner_drives_a_real_pane_and_resumes_the_stream() {
        let (_scratch, _authority, device, root, addr) = serving("attach", Role::Owner).await;
        let mut control = connect(&device, &root, addr)
            .await
            .expect("the owner connects");
        let mut buf = Vec::new();

        send(
            &mut control,
            &Message::Hello {
                v: VERSION,
                client: "phone".to_string(),
                wants: vec![VERSION],
            },
        )
        .await;
        assert!(matches!(
            recv(&mut control, &mut buf).await,
            Message::Welcome { .. }
        ));

        // Spawn a real process over the remote path.
        send(
            &mut control,
            &Message::Spawn {
                v: VERSION,
                id: "remote".to_string(),
                program: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "printf 'remote-line-1\nremote-line-2\n'; sleep 30".to_string(),
                ],
                cols: 80,
                rows: 24,
                memory_max: None,
                pids_max: None,
                kill_on_breach: false,
            },
        )
        .await;
        match recv(&mut control, &mut buf).await {
            Message::Ok { .. } => {}
            other => panic!("spawn over the remote path failed: {other:?}"),
        }

        // Attach, then resume at the cursor: the §3.2 primitive, over the
        // remote path.
        let attached = stream_first_delta(
            &device,
            &root,
            addr,
            Message::Attach {
                v: VERSION,
                id: "remote".to_string(),
                from_line: 0,
            },
        )
        .await;
        assert!(
            attached.iter().any(|line| line.contains("remote-line-1")),
            "the pane's output must arrive over the transport: {attached:?}"
        );

        let resumed = stream_first_delta(
            &device,
            &root,
            addr,
            Message::Resume {
                v: VERSION,
                id: "remote".to_string(),
                from_line: 0,
            },
        )
        .await;
        assert!(
            resumed.iter().any(|line| line.contains("remote-line-2")),
            "resume must replay the pane's history from the cursor: {resumed:?}"
        );

        // Concurrent sessions: a third one observes while the streams run.
        send(
            &mut control,
            &Message::Panes {
                v: VERSION,
                panes: vec![],
            },
        )
        .await;
        match recv(&mut control, &mut buf).await {
            Message::Panes { panes, .. } => assert_eq!(
                panes.iter().filter(|pane| pane.id == "remote").count(),
                1,
                "the pane is visible to another remote session"
            ),
            other => panic!("expected the pane list, got {other:?}"),
        }

        send(
            &mut control,
            &Message::Kill {
                v: VERSION,
                id: "remote".to_string(),
            },
        )
        .await;
        assert!(matches!(
            recv(&mut control, &mut buf).await,
            Message::Ok { .. }
        ));
    }

    /// A device the server never pinned cannot even finish the handshake.
    #[tokio::test]
    async fn an_unpinned_device_gets_no_session() {
        let (_scratch, _authority, _pinned, root, addr) = serving("stranger", Role::Viewer).await;
        let stranger = DeviceKey::generate().expect("entropy");
        let refused = connect(&stranger, &root, addr).await;
        assert!(
            refused.is_err(),
            "an unpinned device must not get a session"
        );
    }

    /// A session that stops mid-handshake must not wedge the listener: the next
    /// device is still served.
    #[tokio::test]
    async fn a_stalled_peer_does_not_block_the_listener() {
        let (_scratch, _authority, device, root, addr) = serving("stall", Role::Viewer).await;

        // Open a QUIC connection and send nothing at all, then walk away.
        let stalled = client_endpoint().expect("client endpoint");
        let abandoned = stalled.connect(addr, arreo_core::transport::SERVER_NAME);
        if let Ok(connecting) = abandoned {
            let _ = tokio::time::timeout(std::time::Duration::from_millis(200), connecting).await;
        }

        let mut channel = connect(&device, &root, addr)
            .await
            .expect("a live device still connects while one peer stalls");
        let mut buf = Vec::new();
        send(
            &mut channel,
            &Message::Hello {
                v: VERSION,
                client: "phone".to_string(),
                wants: vec![VERSION],
            },
        )
        .await;
        assert!(matches!(
            recv(&mut channel, &mut buf).await,
            Message::Welcome { .. }
        ));
    }
}
