//! The client that speaks the daemon protocol over either transport (T-0015,
//! T-0032; moved here by T-0045).
//!
//! **One code path, two transports — and now three callers.** ROADMAP §3.7's
//! core scenario is a client on one machine driving the panes of another, and
//! what makes it the *same* client is that everything above this file sees
//! [`Target`] and [`Message`]: framing, handshake, verbs and resume semantics do
//! not know which transport carried them. A remote-only client would fork all
//! four.
//!
//! **Why this lives in `arreo-core`.** It began in `arreo-tui` (T-0015/T-0032),
//! which was the only caller. T-0045 adds two more — the CLI's
//! `arreo attach --machine`, and a daemon attaching to another machine
//! (server-as-client) — and the dependency rule forbids `arreo-cli` depending on
//! `arreo-tui`. Three copies of a handshake, a retry policy and a framing would
//! be three places for the reconnect semantics below to drift; one shared client
//! is what makes "the same client, both roles" a property of the code rather than
//! a claim. The TUI re-exports it, so nothing above it changed.
//!
//! The remote transport is the relay's byte stream (`crate::relay::session`,
//! `crate::transport`'s Noise channel): the relay carries ciphertext envelopes,
//! Noise turns them into a stream, and this file turns the stream into the same
//! `Message` frames the Unix socket carries — connecting to the *daemon*, not to
//! the relay, so the daemon's own per-verb authorization (which device, which
//! role) decides whether a keystroke lands.

//! Socket client (T-0015): framed MessagePack over the daemon socket, or over
//! the relay to a daemon on another machine (T-0032).
//!
//! One connection per call (the CLI pattern): Hello→Welcome handshake, one
//! verb, read the answer(s). The TUI keeps one long-lived attach per focused
//! pane plus per-tick polls for the sidebar (panes + metrics).
//!
//! **One code path, two transports.** §3.7's core scenario is the TUI on one
//! machine driving the panes of another, and the thing that makes it the *same*
//! client is that everything above this file sees [`Target`] and [`Message`]:
//! the framing, the handshake, the verbs and the resume semantics do not know
//! which transport carried them. A remote-only client would fork all four.
//!
//! The remote transport is the relay's byte stream (T-0050's session, T-0023's
//! Noise channel): the relay carries ciphertext envelopes, Noise turns them into
//! a stream, and this file turns the stream into the same `Message` frames the
//! Unix socket carries — connecting to the *daemon*, not to the relay, so the
//! daemon's own per-verb authorization (which device, which role) is the thing
//! that decides whether a keystroke lands.

use crate::identity::{verifying_key_from_hex, DeviceCert, DeviceId, DeviceKey, VerifyingKey};
use crate::proto::codec;
use crate::proto::{Message, VERSION};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("mesh client io: {0}")]
    Io(#[from] std::io::Error),
    #[error("mesh client codec: {0}")]
    Codec(String),
    #[error("mesh client daemon: {0}")]
    Daemon(String),
    #[error("mesh client handshake: {0}")]
    Handshake(String),
    #[error("mesh client remote: {0}")]
    Remote(String),
}

/// Default socket (`$XDG_RUNTIME_DIR/arreo.sock`, else `/tmp/arreo-<uid>.sock`).
#[must_use]
pub fn default_socket() -> PathBuf {
    if let Ok(runtime) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime).join("arreo.sock");
    }
    PathBuf::from(format!("/tmp/arreo-{}.sock", unsafe_uid()))
}

#[cfg(unix)]
fn unsafe_uid() -> u32 {
    unsafe {
        extern "C" {
            fn getuid() -> u32;
        }
        getuid()
    }
}

#[cfg(not(unix))]
fn unsafe_uid() -> u32 {
    0
}

/// Where to connect, and everything needed to get there.
///
/// One enum rather than a trait object: there are exactly two transports, they
/// differ in what they *establish* (a socket, a relay session) and not in what
/// they carry, and a closed set keeps the failure modes enumerable.
#[derive(Debug, Clone)]
pub enum Target {
    /// The daemon on this machine's socket.
    Local(PathBuf),
    /// A daemon on another machine, reached through the relay.
    ///
    /// Boxed because the two variants differ by an order of magnitude in size
    /// (the remote one carries keys and a certificate), and a `Target` is passed
    /// by value in the UI loop.
    Remote(Box<RemoteTarget>),
}

/// A remote daemon: the relay that carries the session, the device it belongs
/// to, and this client's own identity.
///
/// The peer's identity key is **pinned** — the relay routes by device id, which
/// the relay decides, so the device id alone says nothing about who is at the
/// other end. The Noise handshake proves the peer holds the private key for
/// `server_key`, which is what the pairing flow made this client remember
/// (`server.key`); trusting the relay's routing instead is trusting the relay.
#[derive(Debug, Clone)]
pub struct RemoteTarget {
    pub relay: SocketAddr,
    pub account: String,
    pub peer: DeviceId,
    pub server_key: VerifyingKey,
    pub device: Arc<DeviceKey>,
    pub cert: Arc<DeviceCert>,
}

impl Target {
    /// The socket path, for the local case — what the UI shows as "where am I".
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Target::Local(path) => path.display().to_string(),
            Target::Remote(remote) => format!("{} via relay {}", remote.peer, remote.relay),
        }
    }

    /// Load a remote target from this machine's identity directory.
    ///
    /// The pieces are the ones `arreo pair --join` wrote: this device's key and
    /// certificate (`device.key`, `devices/<id>.cert`) and the server key the
    /// invite pinned (`server.key`). Nothing here is a secret beyond
    /// `device.key`, which the identity directory already protects.
    pub fn remote(
        relay: SocketAddr,
        account: &str,
        peer: &str,
        identity_root: &Path,
    ) -> Result<Self, ClientError> {
        let peer = DeviceId::parse(peer)
            .map_err(|e| ClientError::Remote(format!("peer device id {peer:?}: {e}")))?;
        let key = DeviceKey::load(&identity_root.join("device.key"))
            .map_err(|e| ClientError::Remote(format!("cannot load this device's key: {e}")))?;
        let id = DeviceId::from_key(&key.public());
        // `DeviceCert::save` names the file after the bare hex id, not the
        // `dev_`-prefixed display form — one spelling, the one the cert file
        // uses (the same mismatch this project has repaired in the transport
        // resolver and the relay's announced-device check).
        let cert_path = identity_root
            .join("devices")
            .join(format!("{}.cert", id.as_str()));
        let cert = DeviceCert::load(&cert_path).map_err(|e| {
            ClientError::Remote(format!("no certificate at {}: {e}", cert_path.display()))
        })?;
        let server_key_path = identity_root.join("server.key");
        let text = std::fs::read_to_string(&server_key_path).map_err(|e| {
            ClientError::Remote(format!(
                "no pinned server key at {}: {e}",
                server_key_path.display()
            ))
        })?;
        let server_key = verifying_key_from_hex(&text)
            .map_err(|e| ClientError::Remote(format!("{}: {e}", server_key_path.display())))?;
        Ok(Target::Remote(Box::new(RemoteTarget {
            relay,
            account: account.to_string(),
            peer,
            server_key,
            device: Arc::new(key),
            cert: Arc::new(cert),
        })))
    }
}

/// The pipe a [`Client`] speaks over. Both variants are byte streams carrying
/// the same frames; the difference is only what was needed to establish them.
enum Link {
    Local {
        reader: BufReader<tokio::net::unix::OwnedReadHalf>,
        writer: tokio::net::unix::OwnedWriteHalf,
    },
    Remote {
        channel: crate::transport::SecureChannel,
        /// The session the stream was opened on, kept so the connection cannot
        /// be garbage-collected out from under the stream, and so a caller can
        /// ask when it closed.
        session: Box<crate::relay::session::RelaySession>,
    },
}

pub struct Client {
    link: Link,
    buf: Vec<u8>,
}

impl Client {
    /// Connect to the daemon on `socket` (the local case, unchanged from T-0015).
    pub async fn connect(socket: &Path) -> Result<Self, ClientError> {
        Self::connect_to(&Target::Local(socket.to_path_buf())).await
    }

    /// Connect to whatever `target` names, then complete the daemon handshake.
    pub async fn connect_to(target: &Target) -> Result<Self, ClientError> {
        let link = match target {
            Target::Local(socket) => {
                let stream = tokio::net::UnixStream::connect(socket).await.map_err(|_| {
                    ClientError::Handshake(format!("no daemon at {}", socket.display()))
                })?;
                let (reader, writer) = stream.into_split();
                Link::Local {
                    reader: BufReader::new(reader),
                    writer,
                }
            }
            Target::Remote(remote) => Link::remote(remote).await?,
        };
        let mut conn = Self {
            link,
            buf: Vec::new(),
        };
        conn.send(&Message::Hello {
            v: VERSION,
            client: "arreo-tui".to_string(),
            wants: vec![VERSION],
        })
        .await?;
        match conn.recv().await? {
            Message::Welcome { .. } => Ok(conn),
            Message::Error { message, .. } => Err(ClientError::Handshake(message)),
            other => Err(ClientError::Handshake(format!("unexpected {other:?}"))),
        }
    }

    pub async fn send(&mut self, message: &Message) -> Result<(), ClientError> {
        let frame = codec::encode_frame(message).map_err(|e| ClientError::Codec(e.to_string()))?;
        match &mut self.link {
            Link::Local { writer, .. } => {
                writer.write_all(&frame).await?;
                writer.flush().await?;
            }
            Link::Remote { channel, .. } => {
                channel.write_all(&frame).await?;
                channel.flush().await?;
            }
        }
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Message, ClientError> {
        loop {
            if let Ok((message, consumed)) = codec::decode_frame(&self.buf) {
                self.buf.drain(..consumed);
                return Ok(message);
            }
            let mut chunk = [0u8; 8192];
            let n = match &mut self.link {
                Link::Local { reader, .. } => reader.read(&mut chunk).await?,
                Link::Remote { channel, .. } => channel.read(&mut chunk).await?,
            };
            if n == 0 {
                return Err(ClientError::Daemon("server closed connection".to_string()));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// A handle that fires when the underlying session ends, for a remote
    /// client.
    ///
    /// `None` for a local connection, which has no session to outlive: the
    /// socket ends when the daemon does, and the next read says so. The remote
    /// case is different — the relay's connection can die between two verbs, and
    /// a UI that only learned about it at the next poll would render a frozen
    /// transcript as if it were live. This is what lets the status bar say
    /// "reconnecting" when the session drops rather than one tick later.
    #[must_use]
    pub fn closed(&self) -> Option<Arc<crate::relay::session::Closed>> {
        match &self.link {
            Link::Local { .. } => None,
            Link::Remote { session, .. } => Some(session.closed_handle()),
        }
    }

    /// Stop the session tidily: flush what is queued, then close.
    ///
    /// The graceful half of a pair whose other half is [`Client::kill`]. For a
    /// remote client both end with the relay session dropped — which is what
    /// tells the relay this device left, and therefore what tells its peers
    /// (T-0054) — but only this one waits for queued bytes to reach the wire.
    pub async fn close(mut self) {
        match &mut self.link {
            Link::Local { writer, .. } => {
                let _ = writer.flush().await;
            }
            Link::Remote { channel, .. } => {
                let _ = AsyncWriteExt::shutdown(channel).await;
            }
        }
    }

    /// Stop the session the way a crash does: no flush, no goodbye.
    ///
    /// Named rather than left as a bare `drop` at the call site, because the
    /// difference between this and [`Client::close`] is exactly what a
    /// drop/reconnect test is asserting, and a reader should not have to infer it
    /// from the absence of a call.
    pub fn kill(self) {}

    /// Send one verb and read its answer, on a connection already open.
    ///
    /// Distinct from [`Client::request`], which opens a connection per call:
    /// this is for a caller making several verbs in a pass (the TUI's sidebar
    /// poll), where one connection per pass is the right unit — a relay dial per
    /// verb would be one per second per pane. Named `call` rather than `request`
    /// because an inherent method cannot share a name with the associated one.
    pub async fn call(&mut self, message: &Message) -> Result<Message, ClientError> {
        self.send(message).await?;
        self.recv().await
    }

    /// One-shot request/response against a socket (kept for local callers).
    pub async fn request(socket: &Path, message: &Message) -> Result<Message, ClientError> {
        Self::request_to(&Target::Local(socket.to_path_buf()), message).await
    }

    /// One-shot request/response against any target.
    pub async fn request_to(target: &Target, message: &Message) -> Result<Message, ClientError> {
        let mut conn = Self::connect_to(target).await?;
        conn.send(message).await?;
        conn.recv().await
    }
}

impl Link {
    /// Dial the relay, open a stream to the peer device, and run the Noise
    /// handshake over it.
    ///
    /// The order matters and is the security shape of the whole path: register
    /// with the relay (which authenticates *us*, from our certificate), ask for
    /// a stream to the peer's device id, then prove over that stream that the
    /// peer holds the key we pinned. Only the last step says who is at the other
    /// end; the relay is a carrier and is never trusted for identity.
    ///
    /// **A handshake is retried on a fresh stream, because a reconnect races the
    /// far end's old one.** The relay does not tell a device that its peer went
    /// away, so after an abrupt drop the daemon still holds the previous stream
    /// and delivers our new handshake into it — where the Noise layer reads it as
    /// garbage, fails, and finally ends that stream. The *next* stream is the one
    /// that gets a fresh session. Retrying is what turns that into a reconnect
    /// instead of a ten-second stall; reusing the stream would not work at all
    /// (a failed handshake leaves the stream unusable), which is why each attempt
    /// asks the session for a new one.
    ///
    /// The proper fix is for the relay to tell peers when a device disconnects
    /// (T-0054); this is the client-side half that works without it.
    async fn remote(remote: &RemoteTarget) -> Result<Self, ClientError> {
        let session = crate::relay::session::RelaySession::dial(
            remote.relay,
            &remote.account,
            &remote.device,
            &remote.cert,
        )
        .await
        .map_err(|e| ClientError::Remote(format!("relay {}: {e}", remote.relay)))?;
        let local = remote.device.noise_static();
        let ours = DeviceId::from_key(&remote.device.public());
        let mut last = String::new();
        // Bound to a local so the borrow outlives the `timeout` future.
        let ours = ours.display_id();
        for attempt in 1..=REMOTE_HANDSHAKE_ATTEMPTS {
            let stream = session.stream_to(&remote.peer);
            let handshake =
                crate::transport::SecureChannel::connect(stream, &local, &ours, &remote.server_key);
            match tokio::time::timeout(REMOTE_HANDSHAKE_TIMEOUT, handshake).await {
                Ok(Ok(channel)) => {
                    return Ok(Link::Remote {
                        channel,
                        session: Box::new(session),
                    })
                }
                Ok(Err(e)) => last = e.to_string(),
                Err(_) => {
                    last = format!(
                        "no answer within {REMOTE_HANDSHAKE_TIMEOUT:?} (the peer may still be \
                         holding an earlier stream)"
                    )
                }
            }
            if attempt < REMOTE_HANDSHAKE_ATTEMPTS {
                // A brief pause: the far end needs its next read to fail before
                // it will accept a new stream for this peer.
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        }
        Err(ClientError::Remote(format!(
            "{}: {last} (after {REMOTE_HANDSHAKE_ATTEMPTS} attempts)",
            remote.peer
        )))
    }
}

/// How many fresh streams one connect may use (see [`Link::remote`]).
const REMOTE_HANDSHAKE_ATTEMPTS: usize = 3;

/// How long one remote handshake may take before the stream is abandoned.
///
/// Shorter than the transport's own 10 s: the relay path is a byte stream that is
/// already connected, so a peer that has not answered in seconds is not slow, it
/// is absent (or holding a stale stream) — and a UI that waits ten seconds before
/// its first retry is a UI that looks hung.
const REMOTE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(4);
