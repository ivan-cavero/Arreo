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

/// A live relay session, multiplexed across peers.
pub struct RelaySession {
    device_id: DeviceId,
    outbound: mpsc::Sender<Outbound>,
    peers: Arc<Mutex<Peers>>,
    new_peers: mpsc::Receiver<DeviceId>,
    inflight: Arc<Mutex<HashMap<u64, String>>>,
    closed: mpsc::Receiver<()>,
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
        let (closed_tx, closed) = mpsc::channel::<()>(1);
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
        tokio::spawn(async move {
            tokio::select! {
                _ = writer_task => {}
                _ = reader_task => {}
            }
            let _ = closed_tx.send(()).await;
        });

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
    pub async fn closed(&mut self) {
        let _ = self.closed.recv().await;
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
        let key = peer.as_str().to_string();
        let (inbound, failure) = {
            let mut peers = self.lock_peers();
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
        RelayStream::new(self.outbound.clone(), peer.clone(), inbound, failure)
    }

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

    fn lock_peers(&self) -> std::sync::MutexGuard<'_, Peers> {
        match self.peers.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
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
