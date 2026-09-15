//! The relay session (T-0104).
//!
//! One sentence: dial the relay as a paired device, then drain the durable
//! inbox, acknowledge what has been read, read the account's machine directory,
//! and carry bytes to and from a peer over the session's per-peer streams.
//!
//! **What a client is, and is not.** `arreo_core::relay::session::RelaySession`
//! has no `send` and no `recv`: sending to a peer goes through `RelayStream`
//! (`AsyncWrite`), and receiving goes through the accept door — `next_peer()`
//! names whoever has written to you and has no stream yet, and `stream_to()`
//! opens the stream their bytes are already queued on. This module mirrors
//! exactly that shape, because a surface offering "receive a message" without a
//! stream would be a different protocol from the one the daemon speaks.
//!
//! **The lock, and its one cost.** `next_peer` takes `&mut RelaySession` and a
//! UniFFI object is shared by `Arc`, so the session lives behind a
//! `tokio::sync::Mutex`. Everything that can avoid the lock does: the closure
//! signal, the identity accessors and a `StreamFactory` are cached at dial time,
//! and `heartbeat` goes through an `OutboundHandle` — so `stream_to`, `closed`,
//! `device_id`, `account`, `nonce` and `heartbeat` never queue behind a parked
//! accept. What *does* queue is a directory read issued while a caller sits in
//! `next_peer`; `docs/mobile.md` records that ordering rule for the UI.
//!
//! **Async, and honestly so.** These calls await a QUIC connection and a relay
//! that may be absent, so the async ones are exported with
//! `async_runtime = "tokio"`: the foreign caller gets a future it can await, and
//! the reactor the session's pumps run on is the one UniFFI's async-compat layer
//! provides. There is no blocking variant, because blocking a phone's UI thread
//! on a socket is the bug this shape exists to prevent.

use std::net::SocketAddr;
use std::sync::Arc;

use arreo_core::identity::DeviceId;
use arreo_core::relay::session::{
    Closed, OutboundHandle, RelaySession, RelayStream, StreamFactory,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

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
/// A phone learns the machine it is talking to from the directory's `daemon_key`
/// / machine id, so it can open a stream *without* waiting to be written to; the
/// accept door ([`RelaySessionHandle::next_peer`]) is for the other direction.
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
#[uniffi::export(async_runtime = "tokio")]
pub async fn relay_session_dial(
    addr: String,
    account: String,
    device: Arc<DeviceKeyHandle>,
    cert: Arc<DeviceCertHandle>,
) -> Result<Arc<RelaySessionHandle>, SessionFfiError> {
    let socket = parse_addr(&addr)?;
    let session = RelaySession::dial(socket, &account, device.key_ref(), cert.cert_ref()).await?;
    let device_id = session.device_id().display_id();
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
        device_id,
        account: confirmed_account,
        nonce,
    }))
}

#[uniffi::export]
impl RelaySessionHandle {
    /// This device's id, as the relay confirmed it: `dev_<hex>`.
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
    /// Idempotent by peer: a second call returns a stream on the same channel, so
    /// a caller cannot accidentally create two streams whose chunks would
    /// interleave into one peer's channel. Lock-free — the factory was taken from
    /// the session at dial time, and it is the same one implementation the
    /// session itself uses.
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
