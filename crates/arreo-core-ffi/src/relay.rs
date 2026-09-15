//! The relay session (T-0104).
//!
//! One sentence: dial the relay as a paired device, then drain the durable
//! inbox, acknowledge what has been read, read the account's machine directory,
//! read a pane's metrics series from a machine's daemon over a peer stream, and
//! carry bytes to and from a peer over the session's per-peer streams.
//!
//! **What a client is, and is not.** `arreo_core::relay::session::RelaySession`
//! has no `send` and no `recv`: sending to a peer goes through `RelayStream`
//! (`AsyncWrite`), and receiving goes through the accept door — `next_peer()`
//! names whoever has written to you and has no stream yet, and `stream_to()`
//! opens the stream their bytes are already queued on. This module mirrors
//! exactly that shape, because a surface offering "receive a message" without a
//! stream would be a different protocol from the one the daemon speaks.
//!
//! **The daemon read, and why it does not dial a second session.** Metrics are a
//! *daemon* verb, not a relay one: the relay routes bytes and knows nothing about
//! panes. So [`RelaySessionHandle::metrics_history`] opens a stream to the
//! machine, runs the transport's Noise handshake over it with the key the caller
//! pinned, and speaks the daemon protocol's own Hello/Welcome before asking —
//! which is the path `arreo attach --machine` takes, because a machine's daemon
//! refuses a stream it has not authenticated. What it deliberately does **not**
//! do is dial a second relay session the way `mesh::session`'s remote client
//! does: the relay allows one session per device, so a second one displaces the
//! first (T-0060) and the phone would lose the session it was already using. One
//! session, one stream, **one conversation per peer, reused by every verb** — the
//! daemon keeps its own session open after a verb, so a client that handshakes
//! per call hands its next handshake to the conversation the daemon is still
//! holding, and the read never arrives (T-0114's p1). The conversation lives in
//! [`RelaySessionHandle`] and is dropped when a verb fails.
//!
//! **Two locks, and what each one costs.** `next_peer` takes `&mut RelaySession`
//! and a UniFFI object is shared by `Arc`, so the session lives behind a
//! `tokio::sync::Mutex`. Everything that can avoid that lock does: the closure
//! signal, the identity accessors and a `StreamFactory` are cached at dial time,
//! `heartbeat` goes through an `OutboundHandle`, and the metrics read opens its
//! stream through that same cached factory — so `stream_to`, `closed`,
//! `device_id`, `account`, `nonce`, `heartbeat` and `metrics_history` never queue
//! behind a parked accept. What *does* queue is a directory read issued while a
//! caller sits in `next_peer`; `docs/mobile.md` records that ordering rule for
//! the UI. The daemon conversations are a **second** lock, for a different
//! reason: one conversation per peer is the protocol, and the mutex is what stops
//! two concurrent reads from opening two of them on one peer's channel — so two
//! metrics polls are serialized against each other, and against nothing else.
//!
//! **Identity comes from this device's key, not from the relay.** The Noise hint
//! that opens a peer stream is an assertion of who is dialing, and
//! [`RelaySessionHandle::device_id`] is what a phone reports about itself; both
//! are derived from the key this session dialed with, and the id the relay echoes
//! in `AuthReply::Welcome` is checked against that derivation at dial time rather
//! than carried ([`SessionFfiError::RelayIdentityMismatch`]).
//!
//! **Async, and honestly so.** These calls await a QUIC connection and a relay
//! that may be absent, so the async ones are exported with
//! `async_runtime = "tokio"`: the foreign caller gets a future it can await, and
//! the reactor the session's pumps run on is the one UniFFI's async-compat layer
//! provides. There is no blocking variant, because blocking a phone's UI thread
//! on a socket is the bug this shape exists to prevent.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use arreo_core::identity::{verifying_key_from_hex, DeviceId, VerifyingKey};
use arreo_core::mesh::MeshClientError;
use arreo_core::proto::{client_versions, codec, Message, VERSION};
use arreo_core::relay::session::{
    Closed, OutboundHandle, RelaySession, RelayStream, StreamFactory,
};
use arreo_core::transport::SecureChannel;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

use crate::codec::WireMetricsPoint;
use crate::directory::MachineRowInfo;
use crate::errors::{CertFfiError, SessionFfiError};
use crate::identity::{DeviceCertHandle, DeviceKeyHandle};

/// The relay's answer to a `machines` request.
///
/// A refusal is an *answer*, not an error: "you may not" and "there are none"
/// are different facts, and a client that cannot tell them apart shows the wrong
/// thing. So `refused` travels on the record and only a broken session is an
/// `Err`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DirectoryReplyInfo {
    pub v: u32,
    /// The `seq` of the request being answered.
    pub seq: u64,
    /// The row a join produced, when it produced one.
    pub granted: Option<MachineRowInfo>,
    /// The rows answering a directory read.
    pub machines: Vec<MachineRowInfo>,
    /// Why the relay would not answer. Absent means it did.
    pub refused: Option<String>,
}

impl DirectoryReplyInfo {
    fn from_core(reply: &arreo_core::relay::DirectoryReply) -> Self {
        Self {
            v: reply.v,
            seq: reply.seq,
            granted: reply.granted.as_ref().map(MachineRowInfo::from_row),
            machines: reply
                .machines
                .iter()
                .map(MachineRowInfo::from_row)
                .collect(),
            refused: reply.refused.clone(),
        }
    }
}

/// A pane's durable metrics series, one window of it (T-0040), as a meter draws
/// it.
///
/// **`step_ms` is the tier the machine *served*, not the tier that was asked
/// for**, and `downshifted` says whether the two differ. A meter that drew the
/// ask would draw a graph of the wrong shape whenever the machine had no rows
/// that fine, which is why the pair travels with the rows rather than the rows
/// alone: the daemon downshifts to the nearest real tier and says so (T-0040's
/// rule), and the CLI's `--step 1s` over six hours is the same case.
///
/// **An empty `rows` is a state, not a failure.** A pane that just started has no
/// history, and a meter renders that as an empty graph; only a session that could
/// not be reached is an `Err`.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct MetricsSeriesInfo {
    pub v: u32,
    /// The tier actually read, in milliseconds.
    pub step_ms: u64,
    /// Whether the machine answered with a coarser tier than the ask.
    pub downshifted: bool,
    /// The points, oldest first.
    pub rows: Vec<WireMetricsPoint>,
}

/// A peer, named by its device id — the token `stream_to` takes.
///
/// An object rather than a bare string so a peer id is parsed exactly once, at
/// the door that names it, and a stream is never opened to a string that only
/// looks like an id.
#[derive(uniffi::Object)]
pub struct RelayPeerHandle {
    device: DeviceId,
}

/// Name a peer by its device id: `dev_<hex>`, or the bare 32 hex characters.
///
/// A phone learns the machine it is talking to from the key it pinned (its
/// fingerprint is this id), so it can open a stream *without* waiting to be
/// written to; the accept door ([`RelaySessionHandle::next_peer`]) is for the
/// other direction.
#[uniffi::export]
pub fn relay_peer_parse(device_id: String) -> Result<Arc<RelayPeerHandle>, CertFfiError> {
    Ok(Arc::new(RelayPeerHandle {
        device: DeviceId::parse(&device_id)?,
    }))
}

#[uniffi::export]
impl RelayPeerHandle {
    /// The peer in the display spelling: `dev_<hex>`.
    #[must_use]
    pub fn device_id(&self) -> String {
        self.device.display_id()
    }

    /// The peer's fingerprint: the bare 32-hex identity.
    #[must_use]
    pub fn fingerprint(&self) -> String {
        self.device.as_str().to_string()
    }
}

/// A live byte stream to one peer.
///
/// The core's `RelayStream` is `AsyncRead + AsyncWrite` with no inherent
/// methods; UniFFI cannot export a trait impl, so `read`/`write`/`close` here
/// are the same three operations named.
#[derive(uniffi::Object)]
pub struct RelayStreamHandle {
    stream: Mutex<RelayStream>,
    peer: String,
}

#[uniffi::export]
impl RelayStreamHandle {
    /// The peer on the other end, in the display spelling.
    #[must_use]
    pub fn peer(&self) -> String {
        self.peer.clone()
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl RelayStreamHandle {
    /// Read up to `max_bytes` of the peer's bytes.
    ///
    /// An empty result means the peer closed the stream. A stream the session
    /// ended out from under the reader fails with the reason instead — never a
    /// short read that looks like a clean end, which is the distinction the core
    /// draws and this keeps.
    pub async fn read(&self, max_bytes: u32) -> Result<Vec<u8>, SessionFfiError> {
        let mut buf = vec![0u8; max_bytes as usize];
        let mut stream = self.stream.lock().await;
        let read = stream.read(&mut buf).await.map_err(|e| io_error(&e))?;
        buf.truncate(read);
        Ok(buf)
    }

    /// Write the peer's bytes.
    pub async fn write(&self, bytes: Vec<u8>) -> Result<(), SessionFfiError> {
        let mut stream = self.stream.lock().await;
        stream.write_all(&bytes).await.map_err(|e| io_error(&e))?;
        Ok(())
    }

    /// Close the write half, which is what tells the peer the stream is done.
    ///
    /// **Not `abort`**: the core's own `Drop` says so, and a shutdown that
    /// flushed is the difference between a peer that read the last message and
    /// one that did not.
    pub async fn close(&self) -> Result<(), SessionFfiError> {
        let mut stream = self.stream.lock().await;
        stream.shutdown().await.map_err(|e| io_error(&e))?;
        Ok(())
    }
}

/// A live relay session, multiplexed across peers.
#[derive(uniffi::Object)]
pub struct RelaySessionHandle {
    /// Only `next_peer` truly needs this; see the module docs for what that
    /// costs and what avoids it.
    session: Mutex<RelaySession>,
    /// Cached at dial time so opening a stream never waits on the accept door.
    streams: StreamFactory,
    /// Cached at dial time so a heartbeat never waits on the accept door.
    outbound: OutboundHandle,
    /// Cached at dial time so a UI can always notice the session ending, even
    /// while another caller is parked in `next_peer`.
    closed: Arc<Closed>,
    /// The key this session registered with. Held because a daemon read has to
    /// prove possession of it over the peer stream (the Noise handshake) — the
    /// secret stays in the caller's own handle, which this only borrows.
    device: Arc<DeviceKeyHandle>,
    /// The daemon conversation with each peer, one per peer, reused by every
    /// verb — see [`RelaySessionHandle::daemon_call`] for why it is one and why
    /// the mutex is the serialization.
    daemons: Mutex<HashMap<DeviceId, PeerSession>>,
    /// This device's own id, derived from [`Self::device`]'s key at dial time —
    /// never the id the relay echoed. See [`RelaySessionHandle::device_id`].
    device_id: String,
    account: String,
    nonce: Vec<u8>,
}

/// Dial the relay and register this device.
///
/// `addr` is `IP:PORT`. One attempt, and a refusal comes back with the relay's
/// own reason — the caller decides whether to retry, which is what keeps a
/// *refused* registration (a bad certificate, an unregistered account) from
/// being retried in a tight loop.
///
/// **This device's id comes from its own key, and the relay's echo is checked
/// against it.** `AuthReply::Welcome` names the device the relay verified from
/// the certificate; that is an assertion, not a value to carry, because the id
/// this session reports and asserts over a peer stream is a claim about *this*
/// device. A relay that names a different id is broken or lying, and the session
/// is refused rather than continued under an identity that is not ours.
#[uniffi::export(async_runtime = "tokio")]
pub async fn relay_session_dial(
    addr: String,
    account: String,
    device: Arc<DeviceKeyHandle>,
    cert: Arc<DeviceCertHandle>,
) -> Result<Arc<RelaySessionHandle>, SessionFfiError> {
    let socket = parse_addr(&addr)?;
    let session = RelaySession::dial(socket, &account, device.key_ref(), cert.cert_ref()).await?;
    let device_id = DeviceId::from_key(&device.key_ref().public());
    if session.device_id() != &device_id {
        return Err(SessionFfiError::RelayIdentityMismatch {
            confirmed: session.device_id().display_id(),
            derived: device_id.display_id(),
        });
    }
    let confirmed_account = session.account();
    let nonce = session.nonce().to_vec();
    let streams = session.stream_factory();
    let outbound = session.outbound_handle();
    let closed = session.closed_handle();
    Ok(Arc::new(RelaySessionHandle {
        session: Mutex::new(session),
        streams,
        outbound,
        closed,
        device,
        daemons: Mutex::new(HashMap::new()),
        device_id: device_id.display_id(),
        account: confirmed_account,
        nonce,
    }))
}

#[uniffi::export]
impl RelaySessionHandle {
    /// This device's id, derived from the key this session dialed with:
    /// `dev_<hex>`.
    ///
    /// **Not the relay's word.** `AuthReply::Welcome` carries an id, and dial
    /// time asserts that it equals this one ([`SessionFfiError::RelayIdentityMismatch`]);
    /// what a phone reports about *itself* is its own key's fingerprint, so a
    /// relay that echoes someone else's id cannot make this device announce it —
    /// the same rule the Noise hint follows, and the reason both are derived
    /// here rather than carried from the relay.
    #[must_use]
    pub fn device_id(&self) -> String {
        self.device_id.clone()
    }

    /// The account this session registered under, as the relay confirmed it.
    #[must_use]
    pub fn account(&self) -> String {
        self.account.clone()
    }

    /// This session's handshake challenge.
    ///
    /// It is what a machine join proof is signed over, so a recorded join cannot
    /// be replayed onto another session.
    #[must_use]
    pub fn nonce(&self) -> Vec<u8> {
        self.nonce.clone()
    }

    /// Open (or reuse) the byte stream to a peer.
    ///
    /// **A second call for one peer takes the channel over.** `stream_for`
    /// *replaces* the live peer's entry, so the older stream stops receiving —
    /// the older one never sees its reply, while both write the same per-device
    /// wire channel. That is the right shape for a re-handshake after a
    /// reconnect, and it is why a caller must not hold two conversations with one
    /// peer at once: the daemon conversation is serialized by the lock in
    /// [`RelaySessionHandle::metrics_history`] for exactly this reason.
    ///
    /// Lock-free — the factory was taken from the session at dial time, and it is
    /// the same one implementation the session itself uses.
    #[must_use]
    pub fn stream_to(&self, peer: Arc<RelayPeerHandle>) -> Arc<RelayStreamHandle> {
        let display = peer.device.display_id();
        Arc::new(RelayStreamHandle {
            stream: Mutex::new(self.streams.stream_to(&peer.device)),
            peer: display,
        })
    }

    /// Wait until the session ends (the relay went away, or the connection
    /// failed).
    ///
    /// A caller that wants to stay connected loops on this and a fresh
    /// [`relay_session_dial`], with a backoff between attempts. Lock-free, so it
    /// works even while another caller is parked in `next_peer`.
    pub async fn closed(&self) {
        self.closed.wait().await;
    }
}

#[uniffi::export(async_runtime = "tokio")]
impl RelaySessionHandle {
    /// Refresh this device's `last_seen_ms` on the relay.
    ///
    /// The caller decides the cadence: a session that only ever receives would
    /// otherwise age out while still connected. Lock-free (it goes through the
    /// outbound handle the core provides for exactly this).
    pub async fn heartbeat(&self) -> Result<(), SessionFfiError> {
        Ok(self.outbound.send_heartbeat().await?)
    }

    /// Ask the relay to drain this device's durable inbox from `from_seq`.
    ///
    /// Drained messages arrive as ordinary envelopes on the peer streams, which
    /// is what makes "the machine was off" and "the machine is on" one code path.
    pub async fn drain(&self, from_seq: u64) -> Result<(), SessionFfiError> {
        let session = self.session.lock().await;
        Ok(session.drain(from_seq).await?)
    }

    /// Acknowledge everything up to and including `seq`, removing those rows.
    pub async fn ack(&self, seq: u64) -> Result<(), SessionFfiError> {
        let session = self.session.lock().await;
        Ok(session.ack(seq).await?)
    }

    /// Read the account's machine directory.
    ///
    /// `all` includes tombstoned (removed) names rather than only live machines.
    pub async fn machines(&self, all: bool) -> Result<DirectoryReplyInfo, SessionFfiError> {
        let session = self.session.lock().await;
        Ok(DirectoryReplyInfo::from_core(&session.machines(all).await?))
    }

    /// The next peer that has sent this device something and has no stream yet.
    ///
    /// This is the accept door, and it is the only way to learn that a peer wrote
    /// first: the relay routes bytes to per-peer streams, so a client waits here,
    /// then opens the stream their bytes are queued on.
    pub async fn next_peer(&self) -> Result<Option<Arc<RelayPeerHandle>>, SessionFfiError> {
        let mut session = self.session.lock().await;
        Ok(session
            .next_peer()
            .await
            .map(|device| Arc::new(RelayPeerHandle { device })))
    }

    /// One pane's durable metrics series — the read a RAM meter draws.
    ///
    /// It is the CLI's own question, field for field: `Message::MetricsHistory`
    /// with the pane, the window (`since_ms` … `until_ms`, where `u64::MAX` is the
    /// CLI's "to now") and the tier asked for in `step_ms` (0 = the finest the
    /// machine has), answered by `Message::MetricsSeries`. No new protocol is
    /// involved, so a phone and `arreo metrics history` cannot disagree about what
    /// was asked.
    ///
    /// `server_key` is the machine's **pinned** public key, 64 hex characters —
    /// the key the pairing flow made this device remember, never the one the
    /// relay's directory reports. The relay routes by device id and is not
    /// trusted for identity, so the handshake has to prove the machine holds the
    /// key *this* device pinned; `mesh::session`'s remote target says the same
    /// thing for the CLI and the TUI.
    ///
    /// **The peer is that key's device id** — `relay_peer_parse(fingerprint_of_public_key(server_key))`,
    /// or the bare fingerprint a directory row publishes as its `daemon_key`.
    /// *Not* the row's `machine_id`: the relay routes by the id a device dialed
    /// with, and `machine_id` is the machine's directory identity — its root key
    /// (T-0043), the one that outlives re-pairing. The core's own resolver draws
    /// the same line (`DeviceId::from_key(&server_key)`); a phone that opened a
    /// stream to `machine_id` reaches no device at all (the relay answers "the
    /// relay does not know that device"). Deriving it from the pinned key also
    /// keeps the two facts from disagreeing: the stream goes to whoever holds the
    /// key the handshake is about to prove.
    ///
    /// **One conversation per peer, reused by every read** — the daemon keeps its
    /// session open after a verb, so a client that handshakes per call writes its
    /// next handshake into the session the daemon is still holding and the read
    /// never arrives (see [`RelaySessionHandle::daemon_call`]). The key above is
    /// therefore checked once, when the conversation is opened; a later call
    /// naming a *different* pinned key for the same peer is refused rather than
    /// served on it.
    ///
    /// **It queues behind another read of the same session, deliberately.** The
    /// conversation cache is one mutex: two meters polling at once are serialized
    /// rather than interleaved, because `stream_to` replaces the live peer's
    /// routing and two conversations on one peer would corrupt each other's
    /// framing. It does *not* queue behind `next_peer`, which holds the session's
    /// own lock — a poll and an accept loop are still independent.
    pub async fn metrics_history(
        &self,
        peer: Arc<RelayPeerHandle>,
        server_key: String,
        pane: String,
        since_ms: u64,
        until_ms: u64,
        step_ms: u64,
    ) -> Result<MetricsSeriesInfo, SessionFfiError> {
        let server = verifying_key_from_hex(&server_key)
            .map_err(|e| SessionFfiError::BadPeerKey(e.to_string()))?;
        let answer = self
            .daemon_call(
                &peer,
                &server,
                &Message::MetricsHistory {
                    v: VERSION,
                    id: pane,
                    since_ms,
                    until_ms,
                    step_ms,
                },
            )
            .await?;
        match answer {
            Message::MetricsSeries {
                v,
                // The pane comes back echoed; the caller named it, so the record
                // carries the answer rather than the question.
                id: _,
                step_ms,
                downshifted,
                rows,
            } => Ok(MetricsSeriesInfo {
                v,
                step_ms,
                downshifted,
                rows: rows.iter().map(WireMetricsPoint::from_point).collect(),
            }),
            Message::Error { message, .. } => Err(SessionFfiError::Daemon(message)),
            other => Err(SessionFfiError::Daemon(format!("unexpected {other:?}"))),
        }
    }
}

/// The relay address, parsed with the core's own rule: `IP:PORT`.
fn parse_addr(text: &str) -> Result<SocketAddr, SessionFfiError> {
    text.trim()
        .parse::<SocketAddr>()
        .map_err(|e| SessionFfiError::BadAddress {
            text: text.to_string(),
            detail: e.to_string(),
        })
}

impl RelaySessionHandle {
    /// One verb on this peer's daemon conversation, opening it if there is none.
    ///
    /// **One conversation, many verbs.** The daemon's `serve_session` keeps its
    /// session open after a verb, so a client that opens a fresh one per call
    /// writes its next handshake into the session the daemon is still holding:
    /// the 32-byte cleartext hint is read as a u16-BE frame length, the daemon
    /// waits for bytes that never come, and the client's one unretried attempt
    /// fails — and keeps failing (T-0114's p1, against a real daemon). The core's
    /// own client has the same shape and answers it by retrying on *fresh
    /// streams* (`mesh::session`'s `REMOTE_HANDSHAKE_ATTEMPTS`, "the peer may
    /// still be holding an earlier stream"), which works there because it dials a
    /// whole new relay session per connect; a long-lived phone session has no
    /// such reset, so it keeps the one conversation instead.
    ///
    /// **The lock is the serialization.** `stream_to` *replaces* the live peer's
    /// routing, so two conversations with one peer would write into the same wire
    /// channel while only the newer one could ever read a reply. Holding this
    /// mutex across the verb makes concurrent reads (two meters, a refresh racing
    /// a poll) queue instead of interleave. It is a second lock, not the
    /// session's: a poll still does not queue behind a caller parked in
    /// `next_peer`.
    ///
    /// **A failure drops the conversation, so the next call opens a clean one.**
    /// That is recovery without a retry loop inside a call: the daemon may have
    /// restarted, the relay may have dropped the route, the reply may have been
    /// late (a late answer would be read as the *next* verb's), or the frame may
    /// be undecodable — every one of those leaves the conversation dead or out of
    /// step, and reusing it would keep failing forever rather than reconnecting.
    ///
    /// **Every `Err` here is that kind of failure, by construction.** The
    /// machine's own refusal is an *answer* (`Message::Error`), not an error, so
    /// it keeps the conversation without a special case; and a handshake the
    /// machine refused never reaches the cache at all (`daemon_to` returns before
    /// anything is stored). So there is nothing to distinguish: an `Err` from the
    /// conversation is the channel, and the channel goes.
    ///
    /// **The pin is checked here, and only once.** A conversation *is* the proof
    /// that the peer holds the key it was opened with, and there is no handshake
    /// left to check a second key against — so a call naming a different pinned
    /// key for the same peer is refused rather than served under a proof of
    /// something else. That refusal leaves the conversation in place: it was not
    /// the conversation's fault.
    async fn daemon_call(
        &self,
        peer: &RelayPeerHandle,
        pinned: &VerifyingKey,
        message: &Message,
    ) -> Result<Message, SessionFfiError> {
        let mut daemons = self.daemons.lock().await;
        // One lookup, and no unreachable branch: a vacant slot is filled by the
        // handshake (the lock is held across it, which is what makes two
        // concurrent reads of one peer impossible rather than merely unlikely).
        let open = match daemons.entry(peer.device.clone()) {
            std::collections::hash_map::Entry::Occupied(slot) => slot.into_mut(),
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(self.daemon_to(peer, pinned).await?)
            }
        };
        if open.pinned != *pinned {
            return Err(SessionFfiError::Peer(format!(
                "the conversation with {} was opened under the pinned key for {}, not {}: one \
                 conversation per peer, and it carries the key it was proven with — dial a new \
                 session to read that machine under another key",
                peer.device.display_id(),
                DeviceId::from_key(&open.pinned).display_id(),
                DeviceId::from_key(pinned).display_id(),
            )));
        }
        match open.call(message).await {
            Ok(answer) => Ok(answer),
            Err(error) => {
                daemons.remove(&peer.device);
                Err(error)
            }
        }
    }

    /// Open the daemon conversation: the transport's secure channel over this
    /// session's stream to `peer`, then the protocol's own Hello/Welcome.
    ///
    /// The key used is the one the caller pinned, never the one the relay's
    /// directory reports — see [`RelaySessionHandle::metrics_history`]. The Noise
    /// static is derived from the key this session dialed with, which is the
    /// derivation `DeviceKey::noise_static` exists for; the session holds the
    /// caller's handle, not a copy of the secret.
    async fn daemon_to(
        &self,
        peer: &RelayPeerHandle,
        server: &VerifyingKey,
    ) -> Result<PeerSession, SessionFfiError> {
        let stream = self.streams.stream_to(&peer.device);
        let channel = SecureChannel::connect(
            stream,
            &self.device.key_ref().noise_static(),
            &self.device_id,
            server,
        )
        .await
        .map_err(|e| SessionFfiError::Peer(e.to_string()))?;
        let mut daemon = PeerSession {
            pinned: *server,
            channel,
            buf: Vec::new(),
        };
        daemon
            .send(&Message::Hello {
                v: VERSION,
                client: DAEMON_CLIENT.to_string(),
                wants: client_versions(),
            })
            .await?;
        match daemon.recv(DAEMON_REPLY_TIMEOUT).await? {
            Message::Welcome { .. } => Ok(daemon),
            Message::Error { message, .. } => Err(SessionFfiError::Daemon(message)),
            other => Err(SessionFfiError::Daemon(format!("unexpected {other:?}"))),
        }
    }
}

/// One daemon conversation with one peer.
struct PeerSession {
    /// The machine key this conversation was opened with, as the caller pinned
    /// it. A conversation is a proof of this key and of nothing else.
    pinned: VerifyingKey,
    channel: SecureChannel,
    /// The bytes of a frame that has not arrived in full yet.
    buf: Vec<u8>,
}

impl PeerSession {
    /// Send one message, framed the way the daemon reads frames.
    ///
    /// **Bounded like the read** (T-0114 finding C). The write half was
    /// unbounded, so a machine that stops reading fills the 64 KB duplex and
    /// `write_all` never returns — a verb that cannot be delivered would hang a
    /// phone's poll forever, where a verb that is not *answered* gives up in five
    /// seconds. A write cut off partway leaves the channel in no defined state,
    /// which is why a send that fails this way drops the conversation rather than
    /// sending the next verb into it ([`RelaySessionHandle::daemon_call`]).
    async fn send(&mut self, message: &Message) -> Result<(), SessionFfiError> {
        let frame = codec::encode_frame(message).map_err(|e| {
            SessionFfiError::Peer(MeshClientError::Codec(e.to_string()).to_string())
        })?;
        let write = async {
            self.channel.write_all(&frame).await?;
            self.channel.flush().await
        };
        match tokio::time::timeout(DAEMON_REPLY_TIMEOUT, write).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(peer_io(e)),
            Err(_) => Err(peer_io(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "the machine took no more than {} bytes within {DAEMON_REPLY_TIMEOUT:?} \
                     (it stopped reading)",
                    frame.len()
                ),
            ))),
        }
    }

    /// Read the next message, waiting no longer than `bound` for it.
    ///
    /// A peer that closes the channel is a failure rather than an empty answer:
    /// "the machine hung up" and "the machine has no history for this pane" are
    /// different facts, and only the second one is an empty list.
    ///
    /// **Only a truncated frame means "read more"** (T-0114 finding D). The codec
    /// says `Truncated` for the length prefix and for the body — the two ways a
    /// frame can still be arriving — and anything else (an over-budget declared
    /// length, a body that does not decode) is a frame that will never be
    /// readable. Treating every error as "incomplete" appended bytes until the
    /// timeout, so the same over-budget length that says "this is corruption" was
    /// indistinguishable from a slow frame.
    async fn recv(&mut self, bound: Duration) -> Result<Message, SessionFfiError> {
        let read = async {
            loop {
                match codec::decode_frame(&self.buf) {
                    Ok((message, consumed)) => {
                        self.buf.drain(..consumed);
                        return Ok(message);
                    }
                    Err(codec::CodecError::Truncated { .. }) => {}
                    Err(e) => {
                        return Err(SessionFfiError::Peer(
                            MeshClientError::Codec(e.to_string()).to_string(),
                        ))
                    }
                }
                let mut chunk = [0u8; 8192];
                let n = self.channel.read(&mut chunk).await.map_err(peer_io)?;
                if n == 0 {
                    return Err(peer_io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the machine closed the channel",
                    )));
                }
                self.buf.extend_from_slice(&chunk[..n]);
            }
        };
        match tokio::time::timeout(bound, read).await {
            Ok(Ok(message)) => Ok(message),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(SessionFfiError::Peer(
                MeshClientError::Handshake(format!(
                    "no answer within {bound:?} (the machine accepted the stream and then said \
                     nothing)"
                ))
                .to_string(),
            )),
        }
    }

    /// One verb and its answer — the shape every client call has.
    async fn call(&mut self, message: &Message) -> Result<Message, SessionFfiError> {
        self.send(message).await?;
        self.recv(DAEMON_REPLY_TIMEOUT).await
    }
}

/// How long the machine has to take one verb, or to answer the daemon
/// handshake.
///
/// Five seconds, which is the bound the core's own client puts on the answer to
/// Hello (`mesh::session`'s `HANDSHAKE_REPLY_TIMEOUT`): it runs after the
/// transport has already bounded the Noise handshake, and a peer that has not
/// answered in seconds is absent rather than slow. A phone showing a spinner is
/// worse than one showing "the machine did not answer".
///
/// It bounds **both halves** of a verb (T-0114 finding C): a machine that stops
/// reading fills the duplex and would otherwise stall `write_all` forever, which
/// is a hang no caller can tell from a machine that is thinking.
const DAEMON_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// What this surface calls itself in the protocol's Hello.
const DAEMON_CLIENT: &str = "arreo-mobile";

/// An io failure on a daemon channel, in the client's own sentence.
///
/// `mesh::ClientError::Io` is "mesh client io: …" — the sentence the core's own
/// client renders when a channel breaks under it, so a phone and the CLI
/// describe the same state with the same words.
fn peer_io(error: std::io::Error) -> SessionFfiError {
    SessionFfiError::Peer(MeshClientError::Io(error).to_string())
}

/// An io failure on a peer stream, in the session's own transport sentence.
///
/// The core's `ClientError::Transport` is "relay transport: …" and a broken
/// stream *is* a transport failure, so this reuses that sentence rather than
/// inventing a second spelling for the same state.
fn io_error(error: &std::io::Error) -> SessionFfiError {
    SessionFfiError::Client(
        arreo_core::relay::ClientError::Transport(error.to_string()).to_string(),
    )
}
