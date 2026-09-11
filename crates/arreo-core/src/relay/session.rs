//! A relay session (T-0050): one connection to the relay, and a byte stream to
//! each peer that wants to talk to us.
//!
//! Used by both ends of §3.7: a *daemon* keeps a session so its peers can reach
//! its panes, and a *client* (the TUI, T-0032) keeps one so it can reach a peer's
//! panes. The two differ in what they do with the streams, not in how they hold
//! the session, so the code is here once rather than twice — and `arreo-core` is
//! where it must live, because the dependency rule forbids `arreo-tui` from
//! reaching into `arreo-server` (AGENTS.md).
//!
//! One sentence: dial the relay, keep the connection's two directions moving at
//! once, and turn the relay's *message* transport into the *byte stream* that
//! [`crate::transport::noise::SecureChannel`] (T-0023) expects — so the
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

use crate::identity::{DeviceCert, DeviceId, DeviceKey};
use crate::relay::{ClientError, Incoming, Outcome, RelayClient, RelayReader, RelayWriter};
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
    /// Refresh this device's `last_seen_ms` (T-0031). A zero-length frame to
    /// self: it reaches the relay's read loop, which is where the heartbeat is
    /// recorded, and no peer ever sees it.
    Heartbeat,
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
    pub(crate) fn mark(&self) {
        self.flag.store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    /// Wait until the session has closed. Public because the accept loop lives
    /// in the daemon crate and watches this signal in a `select!` beside
    /// `next_peer` — the two borrow different objects, which is the whole reason
    /// this type exists.
    pub async fn wait(&self) {
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
    /// Abort handles for the two pumps.
    ///
    /// Held so that **dropping the session closes the connection**. The pumps
    /// own the QUIC streams, and a detached tokio task is not stopped by dropping
    /// the handle that spawned it — so without this a dropped session left the
    /// device registered with the relay, and every peer kept a stream to a client
    /// that had gone. That is not a tidiness bug: it is what made a reconnect
    /// arrive at a far end that still believed the old session was live
    /// (T-0054/T-0032). The same lesson as `SecureChannel`'s `Drop`.
    pumps: Vec<tokio::task::AbortHandle>,
}

impl Drop for RelaySession {
    fn drop(&mut self) {
        for pump in &self.pumps {
            pump.abort();
        }
    }
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
        let pumps = vec![writer_task.abort_handle(), reader_task.abort_handle()];

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
            pumps,
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

    /// Refresh this device's `last_seen_ms` on the relay (T-0031).
    ///
    /// A zero-length frame to self: the relay records it and consumes it, so no
    /// peer ever sees it, and the sender's own reader never announces itself as
    /// a new peer. The caller decides the cadence — `HEARTBEAT_INTERVAL` is the
    /// stated one — because a session that only ever receives would otherwise
    /// age out while still connected.
    pub async fn heartbeat(&self) -> Result<(), SessionError> {
        self.outbound
            .send(Outbound::Heartbeat)
            .await
            .map_err(|_| SessionError::Closed)
    }

    /// The outbound half, for a task that must keep beating after the session
    /// handle moves on (the daemon's heartbeat task, T-0031).
    #[must_use]
    pub fn outbound_handle(&self) -> OutboundHandle {
        OutboundHandle {
            outbound: self.outbound.clone(),
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

/// The outbound half of a session: enough to send, not enough to receive.
///
/// A heartbeat task holds this rather than the session, so the session can move
/// into the accept loop while the task keeps beating. When the session ends the
/// channel closes, the send fails, and the task exits with it — no leak, no
/// second "is the session alive" flag to keep in sync.
#[derive(Debug, Clone)]
pub struct OutboundHandle {
    outbound: mpsc::Sender<Outbound>,
}

impl OutboundHandle {
    /// Send one heartbeat. Fails when the session is gone, which is the
    /// task's signal to exit.
    pub async fn send_heartbeat(&self) -> Result<(), SessionError> {
        self.outbound
            .send(Outbound::Heartbeat)
            .await
            .map_err(|_| SessionError::Closed)
    }
}

/// How long until the next heartbeat, given a jitter fraction in `[-1, 1]`.
///
/// The stated cadence (30 s) plus up to the jitter (6 s) in either direction,
/// so a fleet that connected together does not write in lockstep. Pure, so the
/// policy is testable without sleeping.
///
/// The numbers are literals, not imports, on purpose: `arreo-core` may not
/// depend on `arreo-relay` (the AGPL boundary, T-0035), and the relay's
/// `presence` module states the same two constants. Two spellings of one fact
/// would be a drift risk, so the relay test asserts the values agree — the
/// direction the dependency rule allows the check to point.
#[must_use]
pub fn heartbeat_delay(jitter_fraction: f64) -> Duration {
    let jitter = jitter_fraction.clamp(-1.0, 1.0);
    let delay_ms = 30_000 + (6_000.0 * jitter) as i64;
    Duration::from_millis(delay_ms.max(1) as u64)
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
            // A heartbeat is addressed to self with an empty payload: it is
            // routed back to this session's own reader, which drops it (a frame
            // from self carries nothing to deliver), but the relay's read loop
            // has already recorded it as `last_seen_ms` on the way through.
            // Sending through the normal path keeps one framing and one
            // attribution — a side channel would be a second answer to "who is
            // alive".
            Outbound::Heartbeat => {
                let this = writer.device_id().clone();
                writer.send(&this, &[]).await.map(|_| None)
            }
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
    mut reader: RelayReader,
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
                match target.inbound.try_send(envelope.payload) {
                    Ok(()) => {}
                    // **Nobody is reading for this peer**: its stream was
                    // dropped, so the receiver is gone. That is not
                    // backpressure, it is a peer whose stream ended — and the
                    // bytes that just arrived are the first flight of whatever
                    // the peer does next (a reconnecting client's handshake, in
                    // T-0032's case). Parking them for a fresh stream and
                    // announcing the peer again is what makes a reconnect work;
                    // dropping them, as this did, silently ate one chunk of every
                    // handshake that followed a disconnect.
                    Err(mpsc::error::TrySendError::Closed(payload)) => {
                        let fresh = {
                            let mut held = match peers.lock() {
                                Ok(guard) => guard,
                                Err(poisoned) => poisoned.into_inner(),
                            };
                            held.live.remove(&key);
                            let (fresh, rx) = Peer::new();
                            held.pending.insert(key.clone(), rx);
                            held.live.insert(key.clone(), fresh.clone());
                            fresh
                        };
                        // The channel is fresh and empty, so this cannot block.
                        let _ = fresh.inbound.try_send(payload);
                        let _ = new_peers.try_send(peer.clone());
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        // A stream that is *reading* but not keeping up has lost
                        // a byte, and a gap in a byte stream is not something the
                        // layer above can detect — it would surface later as a
                        // decryption failure with no explanation. So that stream
                        // ends, with the reason.
                        //
                        // A defensive bound rather than the usual path: the
                        // relay's own per-connection queue is the same size and
                        // QUIC flow control means it cannot deliver faster than
                        // the local consumer drains, so through the relay this
                        // branch is close to unreachable (T-0050's notes). It
                        // stays because the alternative to a bound is unbounded
                        // memory, and the alternative to failing loudly is a
                        // silent gap.
                        eprintln!(
                            "arreo-server: {peer} is not keeping up; ending its stream rather than \
                             leaving a gap"
                        );
                        target.break_stream(format!(
                            "the local session dropped a chunk for {peer}: its stream was not read \
                             fast enough"
                        ));
                        let mut held = match peers.lock() {
                            Ok(guard) => guard,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                        held.live.remove(&key);
                    }
                }
            }
            Incoming::PeerGone(peer) => {
                // End the stream we hold for that peer, if any: the far end is
                // gone, so anything still queued for it can never arrive, and a
                // stream that waits for a peer that has left is a stream the
                // layer above keeps writing into. Ignored when we have no stream
                // for that peer — the notice is account-wide news, and a device
                // that did not care is not an error.
                let key = peer.as_str().to_string();
                let target = {
                    let mut held = match peers.lock() {
                        Ok(guard) => guard,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    held.live.remove(&key)
                };
                if let Some(target) = target {
                    eprintln!(
                        "arreo-server: {peer} went offline; ending its stream rather than \
                         leaving a session that can never deliver"
                    );
                    target.break_stream("the peer went offline".to_string());
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
/// [`crate::transport::noise::SecureChannel`] takes exactly that, so the
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
