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
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The session vocabulary, re-exported so this crate's callers keep one import
/// path for "the relay session" while the implementation lives in core.
pub use arreo_core::relay::session::{
    backoff_delay, Closed, RelaySession, RelayStream, SessionError, StreamFactory, BACKOFF_BASE,
    BACKOFF_CEILING, MAX_CHUNK, PROBE_ATTEMPTS, PROBE_TIMEOUT,
};

// ---- the daemon's use of a session (T-0051) ----------------------------------

/// The `[relay]` section of the daemon's configuration file.
#[derive(Debug, Clone, serde::Deserialize)]
struct RelaySection {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    addr: Option<String>,
    #[serde(default)]
    account: Option<String>,
    /// The device to open a session to at boot. Optional: a machine that only
    /// ever serves its peers needs none.
    #[serde(default)]
    peer: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ConfigFile {
    #[serde(default)]
    relay: Option<RelaySection>,
}

/// A validated relay configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaySettings {
    pub addr: SocketAddr,
    pub account: String,
    pub peer: Option<DeviceId>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {detail}")]
    Io { path: String, detail: String },
    #[error("cannot parse {path}: {detail}")]
    Parse { path: String, detail: String },
    #[error("the [relay] section of {path} is incomplete: {detail}")]
    Incomplete { path: String, detail: String },
}

/// Load the relay configuration, if the file enables it.
///
/// Returns `Ok(None)` for a missing file, a file with no `[relay]` section, or
/// `enabled = false` — all three mean "no relay", which is the default posture
/// and must cost nothing. A file that *does* enable the relay but is incomplete
/// is an error rather than a silent no-op: an operator who asked for the remote
/// path and quietly did not get it has a bug they cannot see.
pub fn load_config(path: &std::path::Path) -> Result<Option<RelaySettings>, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(ConfigError::Io {
                path: path.display().to_string(),
                detail: e.to_string(),
            })
        }
    };
    let parsed: ConfigFile = toml::from_str(&text).map_err(|e| ConfigError::Parse {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    let Some(section) = parsed.relay else {
        return Ok(None);
    };
    if !section.enabled {
        return Ok(None);
    }
    let incomplete = |detail: &str| ConfigError::Incomplete {
        path: path.display().to_string(),
        detail: detail.to_string(),
    };
    let addr = section
        .addr
        .as_deref()
        .ok_or_else(|| incomplete("[relay] enabled without `addr`"))?
        .parse::<SocketAddr>()
        .map_err(|e| incomplete(&format!("`addr` is not an IP:PORT address: {e}")))?;
    let account = section
        .account
        .filter(|account| !account.trim().is_empty())
        .ok_or_else(|| incomplete("[relay] enabled without `account`"))?;
    let peer = match section.peer.as_deref() {
        None => None,
        Some(raw) => Some(
            DeviceId::parse(raw)
                .map_err(|e| incomplete(&format!("`peer` is not a device id: {e}")))?,
        ),
    };
    Ok(Some(RelaySettings {
        addr,
        account,
        peer,
    }))
}

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
    pub db: std::path::PathBuf,
    /// Behind an `Arc` because the context is cloned per peer task, and a
    /// secret-bearing key type is deliberately not `Clone`: sharing one copy is
    /// cheaper and makes it obvious there is still exactly one.
    pub device: Arc<DeviceKey>,
    pub cert: Arc<DeviceCert>,
}

impl Clone for RelayContext {
    fn clone(&self) -> Self {
        Self {
            authority: Arc::clone(&self.authority),
            registry: Arc::clone(&self.registry),
            db: self.db.clone(),
            device: Arc::clone(&self.device),
            cert: Arc::clone(&self.cert),
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

/// Serve one live session: drain what was queued, accept peers, and if a peer is
/// configured, open a session to it.
async fn serve(mut session: RelaySession, context: &RelayContext, peer: Option<&DeviceId>) {
    // Anything the relay queued while this machine was away arrives as ordinary
    // envelopes; draining from the start is what makes "the machine was off" and
    // "the machine is on" the same path (T-0030).
    if let Err(e) = session.drain(1).await {
        eprintln!("arreo-server: cannot drain the relay inbox: {e}");
    }
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
    let auth = crate::daemon::SessionAuth::new(Arc::clone(&authority), key, device.clone());
    auth.touch();
    eprintln!("arreo-server: relay peer {device} authenticated");
    let (reader, writer) = tokio::io::split(channel);
    if let Err(e) = crate::daemon::serve_session(
        reader,
        writer,
        Arc::clone(&context.registry),
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
