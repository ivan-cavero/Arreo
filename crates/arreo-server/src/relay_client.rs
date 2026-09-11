//! The daemon's relay session (T-0050): one connection to the relay, and a byte
//! stream to each peer that wants to talk to us.
//!
//! One sentence: dial the relay, keep the connection's two directions moving at
//! once, and turn the relay's *message* transport into the *byte stream* that
//! [`arreo_core::transport::noise::SecureChannel`] (T-0023) expects — so the
//! daemon-to-daemon path is encrypted by the one crypto implementation the
//! product already has, and the relay carries ciphertext.
//!
//! **Why an adapter at all.** The relay carries discrete envelopes: a header, a
//! destination, and an opaque payload. Noise wants a stream: `AsyncRead` +
//! `AsyncWrite`, with no notion of messages. Those are different shapes, and the
//! ways to bridge them are not equivalent:
//!
//! - a second Noise implementation that speaks messages would be a second crypto
//!   path to audit, and the task that built the first one explicitly rejected
//!   that;
//! - re-framing the daemon protocol over envelopes would change the protocol
//!   above the transport, which is exactly the thing T-0023 bought by making the
//!   channel look like a byte stream.
//!
//! So this module adapts the *transport* instead: writes are chunked into
//! envelopes, incoming envelopes are reassembled into the stream, and everything
//! above stays byte-oriented and unchanged.
//!
//! **What happens when the relay cannot deliver.** A stream that silently loses
//! a chunk is worse than one that fails, because the loss is invisible to the
//! Noise layer — it would show up later as a decryption failure with no
//! explanation. So every chunk's sequence number is tracked, and a report that is
//! not `delivered`/`queued` ends that peer's stream with an error rather than a
//! gap.

use arreo_core::identity::{DeviceCert, DeviceId, DeviceKey};
use arreo_core::relay::{ClientError, Incoming, Outcome, RelayClient, RelayWriter};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, DuplexStream, ReadBuf};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Largest plaintext chunk carried in one envelope.
///
/// Well under the relay's 1 MiB envelope cap, and small enough that a slow peer
/// backs up quickly instead of buffering megabytes — the relay's own 64-item
/// outbound queue is the real bound, and this keeps one stream from filling it
/// with a single write.
pub const MAX_CHUNK: usize = 32 * 1024;

/// How much decrypted data one stream may hold before its writer must wait.
const STREAM_BUFFER: usize = 64 * 1024;

/// How many chunks one peer's stream may have queued before the session stops
/// accepting more from the relay for it.
///
/// A daemon that never accepts a peer must not let that peer's traffic grow
/// without bound in memory; the relay's per-connection queue bounds the rest.
const PEER_QUEUE: usize = 64;

/// The first backoff step.
pub const BACKOFF_BASE: Duration = Duration::from_millis(250);

/// The longest a reconnect may wait.
pub const BACKOFF_CEILING: Duration = Duration::from_secs(30);

/// How long one attempt of the boot-time probe of the configured peer may take.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// How many times the boot-time probe is attempted before it gives up until the
/// next session.
///
/// More than one, because the common case at boot is that the *peer* has not
/// reached the relay yet: two machines starting together race, and a probe that
/// gave up on the first "the relay does not know that device" would report a
/// failure that resolves itself a second later. Bounded, because this is a
/// diagnostic and not a delivery guarantee — a peer that is genuinely absent is
/// worth one line, not an endless retry.
pub const PROBE_ATTEMPTS: u32 = 5;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("relay client: {0}")]
    Client(#[from] ClientError),
    #[error("the relay session is closed")]
    Closed,
    #[error("no stream to {0}")]
    NoStream(String),
}

/// The reconnect delay for one attempt, given a jitter fraction in `[0, 1)`.
///
/// Pure and total, so the policy is testable without sleeping and the same
/// numbers can be asserted from the outside. Exponential with a ceiling — a
/// relay that is down for an hour costs a retry every `BACKOFF_CEILING`, not a
/// spin — plus up to 25% jitter, so a fleet that lost its relay does not
/// reconnect in lockstep.
#[must_use]
pub fn backoff_delay(attempt: u32, jitter: f64) -> Duration {
    let base = BACKOFF_BASE.as_millis() as u64;
    // Saturating shift: at attempt 64 the doubling would overflow, and the
    // ceiling is reached long before that.
    let scaled = base.saturating_mul(1u64.checked_shl(attempt.min(32)).unwrap_or(u64::MAX));
    let ceiling = BACKOFF_CEILING.as_millis() as u64;
    let capped = scaled.min(ceiling);
    let jitter = jitter.clamp(0.0, 0.999);
    Duration::from_millis(capped + (capped as f64 * jitter * 0.25) as u64)
}

/// Something to write to the relay.
enum Outbound {
    /// A chunk for a peer.
    Data(DeviceId, Vec<u8>),
    /// Ask the relay to drain this device's inbox.
    Drain(u64),
    /// Acknowledge everything up to `seq`.
    Ack(u64),
}

/// One peer's stream state.
///
/// The failure slot is deliberately *outside* the data channel: a stream that
/// has fallen behind has a full queue, and a reason delivered through that queue
/// could never arrive. Keeping it separate is what lets "your bytes were not
/// delivered" reach a stream that is already backed up.
#[derive(Clone)]
struct Peer {
    inbound: mpsc::Sender<Vec<u8>>,
    failure: Arc<Mutex<Option<String>>>,
}

impl Peer {
    fn new() -> (Self, mpsc::Receiver<Vec<u8>>) {
        let (inbound, rx) = mpsc::channel::<Vec<u8>>(PEER_QUEUE);
        (
            Self {
                inbound,
                failure: Arc::new(Mutex::new(None)),
            },
            rx,
        )
    }

    /// Mark this stream broken. The reason is recorded where it cannot be
    /// blocked, and the sender is dropped so the stream ends instead of
    /// continuing with a gap in it.
    fn break_stream(&self, reason: String) {
        match self.failure.lock() {
            Ok(mut slot) => *slot = Some(reason),
            Err(poisoned) => *poisoned.into_inner() = Some(reason),
        }
    }
}

/// The peer table.
///
/// Two halves, because a peer can be heard from *before* anyone opens a stream
/// to it: the sender is where the reader task delivers, and the receiver is
/// parked here until [`RelaySession::stream_to`] takes it. Dropping the receiver
/// on first contact would throw away the very first chunks of a handshake — the
/// ones that matter most.
#[derive(Default)]
struct Peers {
    live: HashMap<String, Peer>,
    /// Receivers for peers heard from before anyone opened a stream to them.
    pending: HashMap<String, mpsc::Receiver<Vec<u8>>>,
}

/// A cheap handle that can open peer streams without owning the session.
///
/// The accept loop needs the session mutably (to take arriving peers) while a
/// probe task needs only to open a stream — and a failed handshake leaves a
/// stream unusable, so the probe needs a *fresh* one per attempt. This handle is
/// what makes both possible: it holds the two things `stream_to` actually uses.
#[derive(Clone)]
pub struct StreamFactory {
    outbound: mpsc::Sender<Outbound>,
    peers: Arc<Mutex<Peers>>,
}

impl StreamFactory {
    /// Open (or reuse) the byte stream to `peer`.
    #[must_use]
    pub fn stream_to(&self, peer: &DeviceId) -> RelayStream {
        stream_for(&self.outbound, &self.peers, peer)
    }
}

/// "The session ended" as a thing more than one caller can wait on.
///
/// A channel would do for one waiter, but the accept loop has to watch for the
/// session ending *and* for a peer arriving in the same `select!`, and both would
/// borrow the session mutably. A flag sidesteps that without inventing a second
/// channel to keep in sync.
#[derive(Debug, Default)]
pub struct Closed {
    flag: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl Closed {
    fn mark(&self) {
        self.flag.store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        // Register before checking, so a closure between the two cannot be lost.
        let notified = self.notify.notified();
        if self.flag.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        notified.await;
    }
}

/// A live relay session, multiplexed across peers.
pub struct RelaySession {
    device_id: DeviceId,
    outbound: mpsc::Sender<Outbound>,
    peers: Arc<Mutex<Peers>>,
    new_peers: mpsc::Receiver<DeviceId>,
    inflight: Arc<Mutex<HashMap<u64, String>>>,
    closed: Arc<Closed>,
}

impl std::fmt::Debug for RelaySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelaySession")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

impl RelaySession {
    /// Dial the relay and register this device.
    ///
    /// One attempt, and a refusal comes back with the relay's own reason: the
    /// caller decides whether to retry, which is what keeps a *refused*
    /// registration (a bad certificate, an unregistered account) from being
    /// retried in a tight loop. The reconnect policy for a relay that is simply
    /// absent is [`backoff_delay`], applied by the caller.
    pub async fn dial(
        addr: SocketAddr,
        account_id: &str,
        device: &DeviceKey,
        cert: &DeviceCert,
    ) -> Result<Self, SessionError> {
        let client = RelayClient::connect(addr, account_id, device, cert).await?;
        Ok(Self::from_client(client))
    }

    fn from_client(client: RelayClient) -> Self {
        let device_id = client.device_id().clone();
        let (writer, reader) = client.into_split();
        let (outbound_tx, outbound_rx) = mpsc::channel::<Outbound>(PEER_QUEUE * 4);
        let (new_peers_tx, new_peers) = mpsc::channel::<DeviceId>(PEER_QUEUE);
        let closed = Arc::new(Closed::default());
        let peers: Arc<Mutex<Peers>> = Arc::new(Mutex::new(Peers::default()));
        let inflight: Arc<Mutex<HashMap<u64, String>>> = Arc::new(Mutex::new(HashMap::new()));

        let writer_task = tokio::spawn(write_pump(writer, outbound_rx, Arc::clone(&inflight)));
        let reader_task = tokio::spawn(read_pump(
            reader,
            Arc::clone(&peers),
            new_peers_tx,
            Arc::clone(&inflight),
        ));

        // The session is closed when either direction stops: a half-open session
        // would accept writes that can never be delivered.
        {
            let closed = Arc::clone(&closed);
            tokio::spawn(async move {
                tokio::select! {
                    _ = writer_task => {}
                    _ = reader_task => {}
                }
                closed.mark();
            });
        }

        Self {
            device_id,
            outbound: outbound_tx,
            peers,
            new_peers,
            inflight,
            closed,
        }
    }

    #[must_use]
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    /// Wait until the session ends (the relay went away, or the connection
    /// failed). A caller that wants to stay connected loops on this and
    /// [`RelaySession::dial`] with [`backoff_delay`] between attempts.
    pub async fn closed(&self) {
        self.closed.wait().await;
    }

    /// A handle that reports the session's closure on its own.
    ///
    /// The accept loop has to wait for a peer *and* for the session ending at
    /// once, and both would borrow the session — one of them mutably. Holding
    /// the closure signal separately means the two waits touch different objects.
    #[must_use]
    pub fn closed_handle(&self) -> Arc<Closed> {
        Arc::clone(&self.closed)
    }

    /// The next peer that has sent us something and has no stream yet.
    ///
    /// This is the accept door: the daemon takes a peer id and opens a stream
    /// with [`RelaySession::stream_to`], which is where the Noise responder
    /// starts.
    pub async fn next_peer(&mut self) -> Option<DeviceId> {
        self.new_peers.recv().await
    }

    /// Open (or reuse) the byte stream to `peer`.
    ///
    /// Idempotent by peer: a second call returns a stream on the same channel,
    /// so a caller cannot accidentally create two streams whose chunks would
    /// interleave into one peer's Noise channel.
    pub fn stream_to(&self, peer: &DeviceId) -> RelayStream {
        stream_for(&self.outbound, &self.peers, peer)
    }

    /// A handle that can open peer streams on its own (see [`StreamFactory`]).
    #[must_use]
    pub fn stream_factory(&self) -> StreamFactory {
        StreamFactory {
            outbound: self.outbound.clone(),
            peers: Arc::clone(&self.peers),
        }
    }
}

/// The one implementation of "open a stream to a peer", shared by the session
/// and the handle so the two cannot disagree about the peer table.
fn stream_for(
    outbound: &mpsc::Sender<Outbound>,
    peers: &Arc<Mutex<Peers>>,
    peer: &DeviceId,
) -> RelayStream {
    {
        let key = peer.as_str().to_string();
        let (inbound, failure) = {
            let mut peers = match peers.lock() {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
            match peers.pending.remove(&key) {
                // Chunks already arrived for this peer: hand them to the stream
                // being created for them, with the failure slot the reader task
                // already installed.
                Some(parked) => {
                    let failure = peers
                        .live
                        .get(&key)
                        .map_or_else(Default::default, |peer| Arc::clone(&peer.failure));
                    (parked, failure)
                }
                None => {
                    let (fresh, rx) = Peer::new();
                    // Installing a peer replaces any previous one, so a second
                    // stream to one peer takes over its traffic — which is what a
                    // re-handshake after a reconnect wants.
                    let failure = Arc::clone(&fresh.failure);
                    peers.live.insert(key, fresh);
                    (rx, failure)
                }
            }
        };
        RelayStream::new(outbound.clone(), peer.clone(), inbound, failure)
    }
}

impl RelaySession {
    /// Ask the relay to drain this device's durable inbox from `from_seq`.
    ///
    /// Drained messages arrive as ordinary envelopes and are dispatched to the
    /// peer streams like any other, which is what makes "the machine was off"
    /// and "the machine is on" the same code path.
    pub async fn drain(&self, from_seq: u64) -> Result<(), SessionError> {
        self.outbound
            .send(Outbound::Drain(from_seq))
            .await
            .map_err(|_| SessionError::Closed)
    }

    /// Acknowledge everything up to `seq`.
    pub async fn ack(&self, seq: u64) -> Result<(), SessionError> {
        self.outbound
            .send(Outbound::Ack(seq))
            .await
            .map_err(|_| SessionError::Closed)
    }

    /// The sequence numbers still awaiting a delivery report, for tests and for
    /// the operator's own diagnostics.
    #[must_use]
    pub fn inflight_count(&self) -> usize {
        match self.inflight.lock() {
            Ok(guard) => guard.len(),
            Err(poisoned) => poisoned.into_inner().len(),
        }
    }
}

/// The write direction: chunks out, drain/ack requests out, sequence numbers
/// recorded so a delivery report can be attributed.
async fn write_pump(
    mut writer: RelayWriter,
    mut outbound: mpsc::Receiver<Outbound>,
    inflight: Arc<Mutex<HashMap<u64, String>>>,
) {
    while let Some(item) = outbound.recv().await {
        let result = match item {
            Outbound::Data(peer, payload) => writer
                .send(&peer, &payload)
                .await
                .map(|seq| Some((seq, peer.as_str().to_string()))),
            Outbound::Drain(from_seq) => writer.drain(from_seq).await.map(|()| None),
            Outbound::Ack(seq) => writer.ack(seq).await.map(|()| None),
        };
        match result {
            Ok(Some((seq, key))) => {
                match inflight.lock() {
                    Ok(mut map) => map.insert(seq, key),
                    Err(poisoned) => poisoned.into_inner().insert(seq, key),
                };
            }
            Ok(None) => {}
            Err(e) => {
                // The session is gone; the reader task will see the same and the
                // caller learns through `closed()`.
                eprintln!("arreo-server: relay write failed: {e}");
                return;
            }
        }
    }
}

/// The read direction: dispatch envelopes to peers, attribute delivery reports.
async fn read_pump(
    mut reader: arreo_core::relay::RelayReader,
    peers: Arc<Mutex<Peers>>,
    new_peers: mpsc::Sender<DeviceId>,
    inflight: Arc<Mutex<HashMap<u64, String>>>,
) {
    loop {
        let incoming = match reader.next().await {
            Ok(incoming) => incoming,
            Err(e) => {
                eprintln!("arreo-server: relay session ended: {e}");
                return;
            }
        };
        match incoming {
            Incoming::Envelope(envelope) => {
                let Ok(peer) = DeviceId::parse(&envelope.header.src_device) else {
                    eprintln!(
                        "arreo-server: relay sent an envelope from an unparseable sender {:?}",
                        envelope.header.src_device
                    );
                    continue;
                };
                let key = peer.as_str().to_string();
                let (target, announce) = {
                    let mut held = match peers.lock() {
                        Ok(guard) => guard,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    match held.live.get(&key) {
                        Some(existing) => (existing.clone(), false),
                        None => {
                            let (fresh, rx) = Peer::new();
                            held.pending.insert(key.clone(), rx);
                            held.live.insert(key.clone(), fresh.clone());
                            (fresh, true)
                        }
                    }
                };
                if announce {
                    // A peer that only ever receives is not news, but a peer
                    // that sent us something is: that is the accept door.
                    let _ = new_peers.try_send(peer.clone());
                }
                if target.inbound.try_send(envelope.payload).is_err() {
                    // A peer whose stream is not keeping up has lost bytes, and
                    // a gap in a byte stream is not something the layer above can
                    // detect — it would surface later as a decryption failure
                    // with no explanation. So the stream ends, with the reason.
                    //
                    // This is a *defensive* bound rather than the usual path: the
                    // relay's own per-connection queue is the same size, and QUIC
                    // flow control means it cannot deliver faster than the local
                    // consumer drains, so through the relay this branch is close
                    // to unreachable (recorded in T-0050's notes). It stays
                    // because the alternative to a bound is unbounded memory, and
                    // the alternative to failing loudly is a silent gap.
                    eprintln!(
                        "arreo-server: {peer} is not keeping up; ending its stream rather than \
                         leaving a gap"
                    );
                    target.break_stream(format!(
                        "the local session dropped a chunk for {peer}: its stream was not read fast \
                         enough"
                    ));
                    let mut held = match peers.lock() {
                        Ok(guard) => guard,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    held.live.remove(&key);
                }
            }
            Incoming::Status { seq, outcome } => {
                let key = match inflight.lock() {
                    Ok(mut map) => map.remove(&seq),
                    Err(poisoned) => poisoned.into_inner().remove(&seq),
                };
                let Some(key) = key else { continue };
                if matches!(outcome, Outcome::Delivered | Outcome::Queued { .. }) {
                    continue;
                }
                // The bytes did not arrive. Telling the stream is the whole
                // point: a silent gap would surface later as a decryption
                // failure with no explanation.
                let reason = match outcome {
                    Outcome::Offline => {
                        "the relay could not deliver: the peer is offline".to_string()
                    }
                    Outcome::NoSuchDevice => "the relay does not know that device".to_string(),
                    Outcome::Refused { reason } => {
                        format!("the relay refused the message: {reason}")
                    }
                    Outcome::Delivered | Outcome::Queued { .. } => unreachable!("handled above"),
                };
                let target = match peers.lock() {
                    Ok(guard) => guard.live.get(&key).cloned(),
                    Err(poisoned) => poisoned.into_inner().live.get(&key).cloned(),
                };
                if let Some(target) = target {
                    target.break_stream(reason);
                    // Dropping the peer closes its channel once the buffered
                    // data has been delivered, so the stream ends at the failure
                    // point instead of running on with a hole in it.
                    let mut held = match peers.lock() {
                        Ok(guard) => guard,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    held.live.remove(&key);
                }
            }
            Incoming::Drain(report) => {
                // The daemon's inbox cursor is the daemon's business (T-0051);
                // what this session must never do is let a drop pass in silence.
                if report.dropped > 0 || report.expired > 0 {
                    eprintln!(
                        "arreo-server: relay inbox reported {} dropped and {} expired message(s) \
                         since the last drain",
                        report.dropped, report.expired
                    );
                }
            }
        }
    }
}

/// A byte stream to one peer, carried in relay envelopes.
///
/// Implements `AsyncRead + AsyncWrite`, which is the whole point: T-0023's
/// [`arreo_core::transport::noise::SecureChannel`] takes exactly that, so the
/// Noise handshake and everything above it run over the relay without knowing
/// they are on a message transport.
///
/// **Why the two directions are built differently.** An earlier shape gave each
/// direction a task over one `tokio::io::duplex`, and the read direction could
/// wedge: with the caller not reading, the task blocked inside a `write_all` that
/// no amount of failure reporting could interrupt, so a broken stream never told
/// anyone. The read direction is therefore polled straight off its channel —
/// backpressure is the channel's, a break closes the channel, and the caller
/// learns the reason at the end of the stream it already holds. The write
/// direction keeps a duplex and one forwarding task, because a `poll_write` needs
/// somewhere to put bytes that cannot block the caller and cannot lose a waker;
/// that is exactly what a duplex is for.
pub struct RelayStream {
    sink: DuplexStream,
    inbound: mpsc::Receiver<Vec<u8>>,
    /// The chunk being handed to the caller, and how much of it is left.
    current: Option<Vec<u8>>,
    peer: DeviceId,
    failure: Arc<Mutex<Option<String>>>,
    forward: JoinHandle<()>,
}

impl std::fmt::Debug for RelayStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayStream")
            .field("peer", &self.peer)
            .finish_non_exhaustive()
    }
}

impl RelayStream {
    fn new(
        outbound: mpsc::Sender<Outbound>,
        peer: DeviceId,
        inbound: mpsc::Receiver<Vec<u8>>,
        failure: Arc<Mutex<Option<String>>>,
    ) -> Self {
        let (sink, mut drain) = tokio::io::duplex(STREAM_BUFFER);
        let out_peer = peer.clone();
        let forward = tokio::spawn(async move {
            let mut buf = vec![0u8; MAX_CHUNK];
            loop {
                match drain.read(&mut buf).await {
                    // The caller's end is gone, or the session is: either way
                    // this direction is finished.
                    Ok(0) | Err(_) => return,
                    Ok(read) => {
                        for chunk in buf[..read].chunks(MAX_CHUNK) {
                            if outbound
                                .send(Outbound::Data(out_peer.clone(), chunk.to_vec()))
                                .await
                                .is_err()
                            {
                                return;
                            }
                        }
                    }
                }
            }
        });
        Self {
            sink,
            inbound,
            current: None,
            peer,
            failure,
            forward,
        }
    }

    /// The peer on the other end of this stream.
    #[must_use]
    pub fn peer(&self) -> &DeviceId {
        &self.peer
    }

    /// Take the reason this stream stopped, if it stopped for one. Taking it
    /// makes the report once-only.
    fn take_failure(&self) -> Option<String> {
        match self.failure.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }
}

impl Drop for RelayStream {
    fn drop(&mut self) {
        self.forward.abort();
    }
}

impl AsyncRead for RelayStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.as_mut().get_mut();
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        loop {
            if let Some(chunk) = this.current.as_mut() {
                if !chunk.is_empty() {
                    let take = chunk.len().min(buf.remaining());
                    buf.put_slice(&chunk[..take]);
                    chunk.drain(..take);
                    return Poll::Ready(Ok(()));
                }
                this.current = None;
            }
            match this.inbound.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(bytes)) => this.current = Some(bytes),
                Poll::Ready(None) => {
                    // The channel closed: the session ended, or the session broke
                    // this stream because bytes were lost. A recorded reason *is*
                    // the end of this stream — report it, once.
                    return match this.take_failure() {
                        Some(reason) => Poll::Ready(Err(std::io::Error::new(
                            std::io::ErrorKind::ConnectionAborted,
                            reason,
                        ))),
                        None => Poll::Ready(Ok(())),
                    };
                }
            }
        }
    }
}

impl AsyncWrite for RelayStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.sink).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.sink).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.sink).poll_shutdown(cx)
    }
}

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
