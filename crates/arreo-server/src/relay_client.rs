//! The daemon's use of a relay session (T-0051).
//!
//! The session itself — dial, peer streams, delivery attribution, reconnect
//! schedule — lives in `arreo_core::relay::session` (T-0050), because a client
//! needs the same thing and the dependency rule forbids `arreo-tui` from
//! reaching into this crate (AGENTS.md).
//!
//! What stays here is what belongs to *this* daemon: the `[relay]` configuration
//! file, this machine's relay identity, the accept loop that turns an arriving
//! peer into a `serve_session` (the same loop the Unix socket runs), and the
//! boot-time probe of the configured peer.

use arreo_core::identity::{DeviceCert, DeviceId, DeviceKey};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The `[relay]` configuration and its loader, re-exported so this crate's
/// callers keep one import path while the one implementation lives in core
/// (T-0044: the CLI reads the same file, and the dependency rule forbids it
/// depending on this crate).
pub use arreo_core::relay::config::{load_config, ConfigError, RelaySettings};
/// The session vocabulary, re-exported so this crate's callers keep one import
/// path for "the relay session" while the implementation lives in core.
pub use arreo_core::relay::session::{
    backoff_delay, Closed, RelaySession, RelayStream, SessionError, StreamFactory, BACKOFF_BASE,
    BACKOFF_CEILING, MAX_CHUNK, PROBE_ATTEMPTS, PROBE_TIMEOUT,
};

// ---- the daemon's use of a session (T-0051) ----------------------------------

/// This machine's own device identity: the key it holds and the certificate it
/// was paired with.
///
/// The certificate is the one `arreo pair` saved on the *client* side
/// (`identity/devices/<id>.cert`), so the relay authenticates the same identity
/// the machine already presents — no second key, no second pin.
pub fn own_identity() -> Result<(DeviceKey, DeviceCert), ConfigError> {
    let root = arreo_core::identity::identity_root();
    let key = crate::devices::client_key().map_err(|e| ConfigError::Io {
        // Name the file, not the directory: an operator reading "cannot read
        // <dir>" has to guess which file is missing.
        path: arreo_core::identity::keys::identity_root()
            .join("device.key")
            .display()
            .to_string(),
        detail: e.to_string(),
    })?;
    let id = DeviceId::from_key(&key.public());
    // `DeviceCert::save` names the file after the *bare* hex id, not the
    // `dev_`-prefixed display form. Looking for the display form here is the
    // same spelling mismatch that has bitten this project before (T-0023's
    // resolver, T-0029's announced-device check): compare and name by one form.
    let path = root.join("devices").join(format!("{}.cert", id.as_str()));
    let cert = DeviceCert::load(&path).map_err(|e| ConfigError::Io {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    Ok((key, cert))
}

/// Everything the relay task needs from the daemon.
pub struct RelayContext {
    pub authority: Arc<Mutex<crate::devices::DeviceAuthority>>,
    pub registry: crate::daemon::Registry,
    pub sessions: crate::daemon::Sessions,
    pub db: std::path::PathBuf,
    /// Behind an `Arc` because the context is cloned per peer task, and a
    /// secret-bearing key type is deliberately not `Clone`: sharing one copy is
    /// cheaper and makes it obvious there is still exactly one.
    pub device: Arc<DeviceKey>,
    pub cert: Arc<DeviceCert>,
    /// The name this machine asserts in the account's directory (T-0056).
    pub machine_name: String,
    /// This machine's trust ledger (T-0046). A relay peer runs the same
    /// `serve_session` behind the same gate, so this machine's decision about who
    /// may use it applies over the relay exactly as it does on a direct
    /// connection — the relay carries bytes and decides nothing.
    pub ledger: arreo_core::mesh::SharedLedger,
}

impl Clone for RelayContext {
    fn clone(&self) -> Self {
        Self {
            authority: Arc::clone(&self.authority),
            registry: Arc::clone(&self.registry),
            sessions: Arc::clone(&self.sessions),
            db: self.db.clone(),
            device: Arc::clone(&self.device),
            cert: Arc::clone(&self.cert),
            machine_name: self.machine_name.clone(),
            ledger: self.ledger.clone(),
        }
    }
}

/// Keep a relay session up for as long as the daemon runs.
///
/// The loop is the reconnect policy: dial, serve, and on any ending wait a
/// backed-off interval before trying again. A relay that is simply absent costs
/// one attempt per ceiling rather than a spin, and a *refused* registration
/// carries the relay's own reason to the log — it is retried on the same
/// schedule rather than in a tight loop, because a bad certificate will not fix
/// itself by being presented again sooner.
pub async fn run(settings: RelaySettings, context: RelayContext) {
    let mut attempt: u32 = 0;
    loop {
        match RelaySession::dial(
            settings.addr,
            &settings.account,
            &context.device,
            &context.cert,
        )
        .await
        {
            Ok(session) => {
                attempt = 0;
                eprintln!(
                    "arreo-server: relay session up as {} (account {}, relay {})",
                    session.device_id(),
                    settings.account,
                    settings.addr
                );
                serve(session, &context, settings.peer.as_ref()).await;
                eprintln!("arreo-server: relay session ended; reconnecting");
            }
            Err(e) => {
                eprintln!(
                    "arreo-server: relay registration failed ({}): {e}",
                    settings.addr
                );
            }
        }
        let jitter = jitter_fraction();
        let delay = backoff_delay(attempt, jitter);
        attempt = attempt.saturating_add(1);
        eprintln!("arreo-server: retrying the relay in {delay:?}");
        tokio::time::sleep(delay).await;
    }
}

/// A jitter fraction in `[0, 1)`, taken from the clock.
///
/// Jitter exists to stop a fleet that lost its relay from reconnecting in
/// lockstep, which is a spread problem and not a secrecy one — so the nanosecond
/// field is the right source and needs no dependency. Distinct processes start
/// at different nanoseconds, which is all the spread that is being asked for.
fn jitter_fraction() -> f64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos) / 1_000_000_000.0
}

/// Load this machine's root key, or `None` with a reason on the log.
///
/// The root key *is* the machine's directory identity (`MachineId::from_key`,
/// T-0043): it is the one key that outlives device re-pairing, which is exactly
/// what a directory entry must key on.
fn machine_root_key() -> Option<arreo_core::identity::RootKey> {
    match arreo_core::identity::RootKey::load_or_generate(&crate::transport::root_key_path()) {
        Ok(key) => Some(key),
        Err(e) => {
            eprintln!("arreo-server: cannot read the machine root key for the directory: {e}");
            None
        }
    }
}

/// The join request for this machine, signed over `nonce`.
///
/// The signature binds the machine key to *this* session's challenge, so a
/// recorded join cannot be replayed onto another session (T-0056).
fn join_request(
    root: &arreo_core::identity::RootKey,
    nonce: &[u8],
    account: &str,
    name: &str,
) -> arreo_core::relay::JoinRequest {
    let key_hex = root.public_hex();
    let payload = arreo_core::relay::join_proof_payload(nonce, account, &key_hex, name);
    arreo_core::relay::JoinRequest {
        v: arreo_core::relay::RELAY_VERSION,
        name: name.to_string(),
        proto_version: arreo_core::proto::VERSION,
        machine_key: key_hex,
        signature: root.sign(&payload).to_bytes().to_vec(),
    }
}

/// Everything a presence beat needs to re-assert the directory row.
///
/// `Clone` because the beat task owns one copy and the daemon's `machine_name`
/// feeds the initial assertion separately.
#[derive(Clone)]
struct DirectoryRefresh {
    root: std::sync::Arc<arreo_core::identity::RootKey>,
    nonce: Vec<u8>,
    account: String,
    name: String,
}

/// Assert this machine's directory row, logging what the relay granted.
async fn assert_directory_row(
    root: &arreo_core::identity::RootKey,
    session: &RelaySession,
    name: &str,
) {
    let request = join_request(root, session.nonce(), &session.account(), name);
    match session.join_machine(request).await {
        Ok(reply) => match (reply.refused, reply.granted) {
            (Some(reason), _) => {
                eprintln!(
                    "arreo-server: the relay would not register this machine in the \
                     directory ({reason}); remote peers can still connect, but `arreo \
                     machines` will not list this machine"
                );
            }
            (None, Some(row)) => {
                let granted = row.name.as_str();
                if granted == name {
                    eprintln!("arreo-server: directory: this machine is {granted}");
                } else {
                    // Not a failure: T-0043's rule gave a name that was already
                    // live to the other machine and suffixed ours. Saying so is
                    // what keeps "why is my machine called workbox-2" from being
                    // a mystery.
                    eprintln!(
                        "arreo-server: directory: {name:?} was taken; this machine is \
                         {granted} (the deterministic suffix)"
                    );
                }
            }
            (None, None) => {
                eprintln!("arreo-server: the relay answered a join without a row");
            }
        },
        Err(e) => {
            eprintln!("arreo-server: could not register this machine in the directory: {e}");
        }
    }
}

/// Serve one live session: drain what was queued, accept peers, and if a peer is
/// configured, open a session to it.
async fn serve(mut session: RelaySession, context: &RelayContext, peer: Option<&DeviceId>) {
    // Anything the relay queued while this machine was away arrives as ordinary
    // envelopes; draining from the start is what makes "the machine was off" and
    // "the machine is on" the same path (T-0030).
    if let Err(e) = session.drain(1).await {
        eprintln!("arreo-server: cannot drain the relay inbox: {e}");
    }
    // Register this machine in the account's directory (T-0056). Failure is not
    // fatal: the machine keeps serving locally and through the relay, and says
    // so — a directory the relay would not write is a problem an operator needs
    // to see, not one that should stop the daemon.
    //
    // The root key is loaded once here and shared with the presence beat below:
    // it is the machine's directory identity, so both paths must use the same
    // key, and loading it twice would be two chances to disagree.
    let root = machine_root_key().map(std::sync::Arc::new);
    if let Some(root) = &root {
        assert_directory_row(root, &session, &context.machine_name).await;
    }
    let refresh = root.map(|root| DirectoryRefresh {
        root,
        nonce: session.nonce().to_vec(),
        account: session.account(),
        name: context.machine_name.clone(),
    });
    // The heartbeat (T-0031): a quiet machine must stay `online` too. A session
    // that only ever receives would otherwise age out while still connected, so
    // a task refreshes `last_seen_ms` on the stated cadence until the session
    // ends. Jittered, so a fleet that connected together does not write in
    // lockstep; the first beat is immediate, so a fresh session is never stale
    // on arrival. The task holds only the outbound half: when the session ends
    // the send fails and the task exits with it.
    {
        let outbound = session.outbound_handle();
        let closed = session.closed_handle();
        let refresh = refresh.clone();
        tokio::spawn(async move {
            use std::time::{SystemTime, UNIX_EPOCH};
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.subsec_nanos() as f64 / 1e9)
                .unwrap_or(0.0);
            // A spread in [-1, 1): distinct processes start at distinct
            // nanoseconds, which is all the de-synchronization this needs.
            let jitter = (nanos * 2.0 - 1.0).clamp(-1.0, 1.0);
            let mut wait = arreo_core::relay::session::heartbeat_delay(jitter);
            loop {
                tokio::select! {
                    () = closed.wait() => return,
                    () = tokio::time::sleep(wait) => {}
                }
                if outbound.send_heartbeat().await.is_err() {
                    return;
                }
                // The machine's own row, on the same cadence (T-0056): the
                // device's `last_seen_ms` and the machine's are different rows
                // (T-0031 vs T-0043), and a directory that says a connected
                // machine is offline is worse than no directory at all.
                if let Some(refresh) = &refresh {
                    let request = join_request(
                        &refresh.root,
                        &refresh.nonce,
                        &refresh.account,
                        &refresh.name,
                    );
                    if outbound.assert_machine(request).await.is_err() {
                        return;
                    }
                }
                wait = arreo_core::relay::session::heartbeat_delay(0.0);
            }
        });
    }
    if let Some(peer) = peer {
        // The probe takes a fresh stream per attempt: a failed handshake leaves a
        // stream unusable, so retrying on it would retry on a dead object.
        let factory = session.stream_factory();
        let owned = context.clone();
        let peer = peer.clone();
        tokio::spawn(async move {
            for attempt in 1..=PROBE_ATTEMPTS {
                let stream = factory.stream_to(&peer);
                if probe_peer(stream, &owned, &peer).await {
                    return;
                }
                if attempt < PROBE_ATTEMPTS {
                    tokio::time::sleep(Duration::from_secs(1 << (attempt - 1))).await;
                }
            }
            eprintln!(
                "arreo-server: relay peer {peer} did not answer after {PROBE_ATTEMPTS} attempts; \
                 giving up until the session is re-established"
            );
        });
    }
    serve_loop(&mut session, context).await;
}

/// The accept loop: peers in, session closure out.
async fn serve_loop(session: &mut RelaySession, context: &RelayContext) {
    let closed = session.closed_handle();

    loop {
        tokio::select! {
            () = closed.wait() => return,
            arrived = session.next_peer() => {
                let Some(arrived) = arrived else { return };
                let stream = session.stream_to(&arrived);
                let owned = context.clone();
                tokio::spawn(async move { serve_peer(stream, &owned, arrived).await });
            }
        }
    }
}

/// Accept one relay peer as a daemon session.
///
/// The peer's stream runs the *same* `serve_session` loop the local socket and
/// the direct transport run, behind the same per-verb gate — so a device that
/// reaches this daemon through the relay has exactly the permissions it has
/// locally, checked by the same code.
async fn serve_peer(stream: RelayStream, context: &RelayContext, announced: DeviceId) {
    let local = context.device.noise_static();
    let guard = arreo_core::transport::FlightGuard::default();
    let authority = Arc::clone(&context.authority);
    let resolved = Arc::clone(&context.authority);
    let channel =
        arreo_core::transport::SecureChannel::accept(stream, &local, &guard, move |device| {
            crate::transport::pinned_key(&resolved, device)
        })
        .await;
    let (channel, device) = match channel {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("arreo-server: relay peer {announced} refused: {e}");
            return;
        }
    };
    let Some(key) = crate::transport::pinned_key(&authority, &device) else {
        eprintln!("arreo-server: relay peer {device} is not pinned; refusing");
        return;
    };
    // A relay peer has no direct address of its own — what the daemon sees is
    // the relay, which is the honest thing to record.
    let auth = crate::daemon::SessionAuth::new(
        Arc::clone(&authority),
        context.ledger.clone(),
        key,
        device.clone(),
    );
    auth.touch();
    eprintln!("arreo-server: relay peer {device} authenticated");
    let (reader, writer) = tokio::io::split(channel);
    if let Err(e) = crate::daemon::serve_session(
        reader,
        writer,
        Arc::clone(&context.registry),
        Arc::clone(&context.sessions),
        context.db.clone(),
        Some(auth),
    )
    .await
    {
        eprintln!("arreo-server: relay peer {device} session error: {e}");
    }
}

/// Open a session to the configured peer and ask it one question.
///
/// This is the daemon's probe of its own remote path, and the seed of remote
/// attach (T-0032): the answer is logged so an operator can see that the relay
/// leg works end to end. It deliberately asks for the pane *list* rather than
/// anything mutable — a boot-time probe must not change the peer's machine.
async fn probe_peer(stream: RelayStream, context: &RelayContext, peer: &DeviceId) -> bool {
    let Some(key) = crate::transport::pinned_key(&context.authority, peer) else {
        eprintln!("arreo-server: cannot probe {peer}: it is not pinned on this machine");
        return false;
    };
    let local = context.device.noise_static();
    let ours = DeviceId::from_key(&context.device.public());
    let channel = match arreo_core::transport::SecureChannel::connect(
        stream,
        &local,
        &ours.display_id(),
        &key,
    )
    .await
    {
        Ok(channel) => channel,
        Err(e) => {
            eprintln!("arreo-server: cannot reach {peer} through the relay: {e}");
            return false;
        }
    };
    // Bounded: a peer that accepts the session and then goes quiet must not
    // leave a probe task waiting for the daemon's whole lifetime.
    match tokio::time::timeout(PROBE_TIMEOUT, ask_pane_count(channel)).await {
        Ok(Ok(count)) => {
            eprintln!("arreo-server: relay peer {peer} reports {count} pane(s)");
            true
        }
        Ok(Err(e)) => {
            eprintln!("arreo-server: relay peer {peer} did not answer: {e}");
            false
        }
        Err(_) => {
            eprintln!("arreo-server: relay peer {peer} did not answer within {PROBE_TIMEOUT:?}");
            false
        }
    }
}

/// Speak the daemon protocol far enough to ask a peer how many panes it has.
async fn ask_pane_count<S>(mut io: S) -> Result<usize, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use arreo_core::proto::{codec, Message, VERSION};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = Vec::new();
    let send = async |io: &mut S, message: &Message| -> Result<(), String> {
        let frame = codec::encode_frame(message).map_err(|e| e.to_string())?;
        io.write_all(&frame).await.map_err(|e| e.to_string())?;
        io.flush().await.map_err(|e| e.to_string())
    };
    let recv = async |io: &mut S, buf: &mut Vec<u8>| -> Result<Message, String> {
        loop {
            if let Ok((message, consumed)) = codec::decode_frame(buf) {
                buf.drain(..consumed);
                return Ok(message);
            }
            let mut chunk = [0u8; 8192];
            let read = io.read(&mut chunk).await.map_err(|e| e.to_string())?;
            if read == 0 {
                return Err("the peer closed the session".to_string());
            }
            buf.extend_from_slice(&chunk[..read]);
        }
    };

    send(
        &mut io,
        &Message::Hello {
            v: VERSION,
            client: "arreo-server".to_string(),
            wants: vec![VERSION],
        },
    )
    .await?;
    match recv(&mut io, &mut buf).await? {
        Message::Welcome { .. } => {}
        other => return Err(format!("expected Welcome, got {other:?}")),
    }
    send(
        &mut io,
        &Message::Panes {
            v: VERSION,
            panes: vec![],
        },
    )
    .await?;
    match recv(&mut io, &mut buf).await? {
        Message::Panes { panes, .. } => Ok(panes.len()),
        other => Err(format!("expected Panes, got {other:?}")),
    }
}
