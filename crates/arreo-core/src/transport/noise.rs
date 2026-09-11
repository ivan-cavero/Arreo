//! Noise-KK secure channel (T-0023): an encrypted, authenticated byte stream.
//!
//! One sentence: wrap any async byte stream, prove both ends hold the pinned
//! ed25519 identities, and hand back something that reads and writes like the
//! original — so the protocol above it is the *same code* as the local socket.
//!
//! Handshake (`Noise_KK_25519_ChaChaPoly_BLAKE2s`, one extra round trip):
//!
//! ```text
//!   initiator (client)                     responder (server)
//!   -----------------------------------    ----------------------------------
//!   hint: 32-byte device id  ------------>  resolve hint -> pinned identity
//!                                           (unknown/revoked: refuse, no crypto)
//!   -> e, es, ss                           <- e, ee, se
//!   both derive transport keys; the handshake completes only if each side holds
//!   the secret for the identity the other side pinned.
//! ```
//!
//! Why the cleartext hint. Noise-KK needs the responder to know the initiator's
//! static key *before* the first flight, but a server has many pinned devices
//! and cannot know which one is calling. So the caller announces its device id
//! first. That is safe: a device id is a public key fingerprint, and the
//! handshake still fails unless the announcer holds the matching secret. A
//! forged hint costs a refused connection (and an `auth_reject` audit row),
//! never an impersonation.
//!
//! Why Noise-KK *inside* QUIC rather than trusting QUIC's TLS. Trust here is
//! **devices** (ROADMAP §3.3, §4): the pinned static key is the anchor, and QUIC
//! provides the transport (congestion control, streams, migration). QUIC still
//! runs TLS 1.3 — it has to — with an ephemeral certificate that is deliberately
//! *not* the trust anchor (see [`super::quic`]). Noise sits inside, so a hostile
//! middlebox can drop or delay but cannot forge or read.
//!
//! Replay. The responder's fresh ephemeral makes a recorded *second* flight
//! useless, but the **first** flight depends only on the responder's static key,
//! so a recorded first flight still authenticates: a fresh responder decrypts
//! it, answers it, and believes it has a session with a pinned device that is
//! not on the other end. Nothing is readable or forgeable — deriving the
//! transport keys needs the initiator's ephemeral *secret* — but a session the
//! server believes in is worth refusing, so the responder checks a
//! [`FlightGuard`] before it commits. Attempts are additionally rate-limited per
//! peer ([`super::quic::HandshakeLimiter`]).
//!
//! Framing honesty. Each transport message is a `u16` length prefix followed by
//! one Noise ciphertext, and the prefix is necessarily *outside* the seal — it
//! is what tells the receiver how much to authenticate. So a peer (or anyone
//! who can rewrite bytes in transit) can lie about it. A length no seal can
//! produce ([`MAX_FRAME_BYTES`]) is treated as corruption and ends the channel
//! immediately; a length that is merely *plausible but longer than what has
//! arrived* is indistinguishable from a frame still in flight, so the pump waits
//! — bounded by the transport's own idle timeout, which closes the connection.
//! Nothing is decrypted or delivered in either case.

use crate::identity::keys::{noise_public_key, NoiseStatic};
use crate::identity::{DeviceId, VerifyingKey};
use snow::{params::NoiseParams, Builder, HandshakeState, TransportState};
use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream, ReadBuf};
use tokio::task::JoinHandle;

/// The Noise protocol: KK (both statics known), X25519, ChaCha20-Poly1305,
/// BLAKE2s. Spelled in full because the choice is a compatibility surface.
pub const NOISE_PARAMS: &str = "Noise_KK_25519_ChaChaPoly_BLAKE2s";

/// Largest plaintext sealed into one Noise message. The ciphertext adds a
/// 16-byte tag and the wire prefix is a `u16`, so this stays well inside what
/// the framing can express.
pub const MAX_PLAINTEXT_CHUNK: usize = 60_000;

/// Largest wire frame we will accept: one full sealed message plus its `u16`
/// length prefix. Anything larger cannot come from a peer running this code, so
/// it is treated as corruption rather than waited on (see the pump).
pub const MAX_FRAME_BYTES: usize = MAX_PLAINTEXT_CHUNK + 16 + 2;

/// Largest amount of un-decrypted or un-sealed data held per direction. A peer
/// that stops reading (or a hostile stream) must not make us allocate without
/// bound.
pub const MAX_BUFFERED_BYTES: usize = 8 << 20;

/// How long a handshake may take before the connection is abandoned. A half-open
/// handshake is free work for an attacker, so it must not linger.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The cleartext identity announcement: exactly the 32 hex characters of a
/// device id (the `dev_` prefix is optional on the wire).
pub const HINT_LEN: usize = 32;

/// Everything that can go wrong establishing or running a secure channel.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport io: {0}")]
    Io(#[from] io::Error),
    #[error("noise handshake failed: {0}")]
    Handshake(String),
    /// Tampered, truncated or reordered ciphertext — the variant the acceptance
    /// criteria name: flipping one byte of any frame lands here.
    #[error("frame failed to decrypt or authenticate")]
    Decrypt,
    #[error("the peer's identity hint is malformed")]
    BadHint,
    #[error("the peer announced an identity that is not pinned")]
    UnknownPeer,
    /// A first flight that has already been answered — a replay, refused before
    /// the responder commits to the handshake (see [`FlightGuard`]).
    #[error("handshake flight replayed")]
    Replay,
    #[error("the peer's static key is not the one its identity implies")]
    IdentityMismatch,
    #[error("handshake took longer than {0:?}")]
    HandshakeTimeout(Duration),
    #[error("peer exceeded the {0}-byte buffering cap")]
    TooMuchBuffered(usize),
}

impl From<snow::Error> for TransportError {
    fn from(e: snow::Error) -> Self {
        match e {
            snow::Error::Decrypt => Self::Decrypt,
            other => Self::Handshake(other.to_string()),
        }
    }
}

/// Which side of the handshake this channel is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The dialer (client): knows the server's identity up front.
    Initiator,
    /// The listener (server): resolves the peer's announced device id.
    Responder,
}

/// An authenticated, encrypted byte stream.
///
/// Implements [`AsyncRead`] and [`AsyncWrite`], so it can be handed to any code
/// that already speaks the framed protocol — which is how the remote path
/// reuses the local one instead of reimplementing it.
pub struct SecureChannel {
    io: DuplexStream,
    remote: [u8; 32],
    role: Role,
    /// `Option` so `shutdown` can take the handle and await it, while `Drop`
    /// aborts whatever is left: a `JoinHandle` cannot be moved out of a type
    /// that implements `Drop`, and both behaviours are needed.
    pump: Option<JoinHandle<()>>,
    /// Why the pump stopped, when it stopped for a reason. The consumer reads
    /// this exactly once, at the EOF that follows, so "the peer's bytes were
    /// tampered with" arrives as a typed `Decrypt` instead of a bare
    /// end-of-stream that a caller cannot distinguish from a clean close.
    failure: Arc<Mutex<Option<TransportError>>>,
}

impl std::fmt::Debug for SecureChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecureChannel")
            .field("remote", &crate::identity::keys::hex(&self.remote))
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

impl SecureChannel {
    /// Dial: prove we hold `local`, and that the peer holds `server`.
    pub async fn connect<S>(
        io: S,
        local: &NoiseStatic,
        device_id: &str,
        server: &VerifyingKey,
    ) -> Result<Self, TransportError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let expected_remote = noise_public_key(server);
        let hint = hint_bytes(device_id)?;
        let handshake = async move {
            let mut io = io;
            io.write_all(&hint).await?;
            io.flush().await?;
            let mut noise = handshake_state(local, &expected_remote, true)?;
            exchange(&mut io, &mut noise, true, None).await?;
            finish(io, noise, Role::Initiator, expected_remote).await
        };
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await {
            Ok(result) => result,
            Err(_) => Err(TransportError::HandshakeTimeout(HANDSHAKE_TIMEOUT)),
        }
    }

    /// Accept: read the peer's announced device id, let `resolve` map it to a
    /// pinned identity, and prove the peer holds that identity's secret.
    ///
    /// `resolve` returning `None` refuses the connection *before* any
    /// cryptography runs — an unknown device costs the server a lookup, not a
    /// handshake.
    ///
    /// `resolve` is handed a [`DeviceId`], not the raw announcement: the wire
    /// carries the bare 32 hex characters, while the authority, the audit log
    /// and the CLI all name devices `dev_<hex>`. Parsing here keeps that one
    /// spelling — a caller that compares the raw hint against a `display_id()`
    /// would refuse every legitimate device.
    ///
    /// `guard` remembers the flights this process has answered, so a recorded
    /// one cannot be replayed into a fresh session (see [`FlightGuard`]); it is
    /// caller-owned because it must outlive a single connection.
    ///
    /// Returns the channel *and* the device id the peer announced. The responder
    /// must know who it is talking to — for `last_seen`, for audit rows, for the
    /// per-verb policy — and the announcement is the only place that id exists:
    /// the authenticated key is an X25519 point, which does not yield the
    /// ed25519 fingerprint the id is derived from.
    pub async fn accept<S, F>(
        io: S,
        local: &NoiseStatic,
        guard: &FlightGuard,
        resolve: F,
    ) -> Result<(Self, DeviceId), TransportError>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        F: FnOnce(&DeviceId) -> Option<VerifyingKey> + Send + 'static,
    {
        let handshake = async move {
            let mut io = io;
            let mut hint = [0u8; HINT_LEN];
            io.read_exact(&mut hint).await?;
            let announced = std::str::from_utf8(&hint).map_err(|_| TransportError::BadHint)?;
            let device_id = DeviceId::parse(announced).map_err(|_| TransportError::BadHint)?;
            let peer = resolve(&device_id).ok_or(TransportError::UnknownPeer)?;
            let expected_remote = noise_public_key(&peer);
            let mut noise = handshake_state(local, &expected_remote, false)?;
            exchange(&mut io, &mut noise, false, Some(guard)).await?;
            let channel = finish(io, noise, Role::Responder, expected_remote).await?;
            Ok((channel, device_id))
        };
        match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await {
            Ok(result) => result,
            Err(_) => Err(TransportError::HandshakeTimeout(HANDSHAKE_TIMEOUT)),
        }
    }

    /// The peer's X25519 static key, as established by the handshake.
    #[must_use]
    pub fn remote_static(&self) -> [u8; 32] {
        self.remote
    }

    #[must_use]
    pub fn role(&self) -> Role {
        self.role
    }

    /// Take the reason the pump stopped, if it stopped for one. Taking it makes
    /// the report once-only, so a caller that loops on reads does not see the
    /// same failure forever.
    fn take_failure(&self) -> Option<TransportError> {
        match self.failure.lock() {
            Ok(mut slot) => slot.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        }
    }

    /// Close the channel and stop the pump.
    /// Hand over everything written so far, then close.
    ///
    /// The graceful counterpart to [`Drop`]: it closes the caller's end of the
    /// duplex, which is the pump's signal to seal and write whatever is still
    /// buffered, and then waits for the pump to finish. A caller that simply
    /// drops a channel gives up that guarantee — the close is immediate and
    /// anything the pump had not yet put on the wire goes with it.
    ///
    /// This cannot deadlock on a live peer: the pump stops as soon as the
    /// consumer's end is gone and its outbound buffers are drained, because
    /// there is nobody left to deliver incoming bytes to.
    pub async fn shutdown(mut self) {
        // Closing the write half is the signal; the whole channel is finished
        // with, so the read half is not held open for a reply. (Moving `io` out
        // would be the obvious spelling, but a type with `Drop` cannot have its
        // fields moved — hence `Drop` taking the pump, and this taking the
        // handle.)
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut self.io).await;
        if let Some(pump) = self.pump.take() {
            let _ = pump.await;
        }
    }
}

/// Dropping a channel closes it.
///
/// **Why this is not just tidiness.** The pump owns the raw stream, and a
/// `JoinHandle` does not abort its task when the handle is dropped — so without
/// this, dropping a `SecureChannel` left the pump running and the connection
/// open. The peer then noticed only at the transport's idle timeout (15 s), which
/// meant a daemon kept a vanished client's session alive for that long and wrote
/// its "session ended" row a quarter-minute after it ended.
///
/// **The contract, stated because the two behaviours differ:** a *dropped*
/// channel is a *closed* channel — buffered ciphertext that has not reached the
/// wire is discarded, which is what makes the close prompt. A caller whose last
/// write must land calls [`SecureChannel::shutdown`], which closes the caller's
/// end and waits for the pump to finish delivering. This is the same shape as
/// half-closing a socket: closing is immediate, flushing is explicit.
impl Drop for SecureChannel {
    fn drop(&mut self) {
        if let Some(pump) = self.pump.take() {
            pump.abort();
        }
    }
}

impl AsyncRead for SecureChannel {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        match Pin::new(&mut self.io).poll_read(cx, buf) {
            Poll::Ready(Ok(())) if buf.filled().len() == before => {
                // End of stream. If the pump stopped for a reason, that reason
                // *is* the end of this stream — report it, once.
                match self.take_failure() {
                    Some(failure) => {
                        Poll::Ready(Err(io::Error::new(io::ErrorKind::InvalidData, failure)))
                    }
                    None => Poll::Ready(Ok(())),
                }
            }
            other => other,
        }
    }
}

impl AsyncWrite for SecureChannel {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

/// Refuses a first flight that has already been answered.
///
/// **Why this is needed at all.** KK's *first* message depends only on the
/// initiator's ephemeral and the responder's **static** key — not on the
/// responder's ephemeral, and not on anything session-specific. A recorded
/// flight therefore stays valid forever: replayed at a fresh connection, the
/// responder decrypts it, writes its half of the handshake, and reports a
/// session with a pinned device that is not on the other end. The attacker
/// cannot derive the transport keys (that needs the initiator's ephemeral
/// *secret*), so no data flows either way — but the server has been made to
/// believe in a session, and a session it believes in is worth refusing.
///
/// **Why it cannot be abused.** Only flights that `read_message` authenticated
/// are recorded, so an unpinned peer — which cannot produce one — can never
/// grow the map. Entries are pruned by `window`, and honest clients never repeat
/// a flight (snow draws a fresh ephemeral per handshake), so the only thing
/// this refuses is a replay.
#[derive(Debug)]
pub struct FlightGuard {
    seen: Mutex<HashMap<Vec<u8>, Instant>>,
    window: Duration,
}

impl Default for FlightGuard {
    fn default() -> Self {
        Self::new(HANDSHAKE_TIMEOUT)
    }
}

impl FlightGuard {
    #[must_use]
    pub fn new(window: Duration) -> Self {
        Self {
            seen: Mutex::new(HashMap::new()),
            window,
        }
    }

    /// Record an authenticated flight; `false` means it is a replay.
    fn record(&self, flight: &[u8]) -> bool {
        let now = Instant::now();
        let mut seen = match self.seen.lock() {
            Ok(seen) => seen,
            Err(poisoned) => poisoned.into_inner(),
        };
        seen.retain(|_, at| now.duration_since(*at) < self.window);
        match seen.entry(flight.to_vec()) {
            Entry::Occupied(mut entry) => {
                entry.insert(now);
                false
            }
            Entry::Vacant(entry) => {
                entry.insert(now);
                true
            }
        }
    }
}

/// The 32-byte announcement for a device id.
fn hint_bytes(device_id: &str) -> Result<[u8; HINT_LEN], TransportError> {
    let bare = device_id.trim().trim_start_matches("dev_");
    if bare.len() != HINT_LEN || !bare.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(TransportError::BadHint);
    }
    let mut out = [0u8; HINT_LEN];
    out.copy_from_slice(bare.to_ascii_lowercase().as_bytes());
    Ok(out)
}

/// Build the handshake state. The secret is copied into the state here, so the
/// borrow of the builder never escapes this function.
fn handshake_state(
    local: &NoiseStatic,
    remote: &[u8; 32],
    initiator: bool,
) -> Result<HandshakeState, TransportError> {
    let params: NoiseParams = NOISE_PARAMS
        .parse()
        .map_err(|e: snow::Error| TransportError::Handshake(e.to_string()))?;
    let secret = local.secret();
    let builder = Builder::new(params)
        .local_private_key(&secret)
        .remote_public_key(remote);
    let state = if initiator {
        builder.build_initiator()
    } else {
        builder.build_responder()
    };
    state.map_err(TransportError::from)
}

/// One flight out, one flight in. The initiator speaks first (KK).
///
/// The responder also checks `guard` here — after the flight authenticates, so
/// only a genuine pinned peer can ever occupy it, and before the responder's own
/// flight is written, so a replay costs it nothing (see [`FlightGuard`]).
async fn exchange<S>(
    io: &mut S,
    noise: &mut HandshakeState,
    initiator: bool,
    guard: Option<&FlightGuard>,
) -> Result<(), TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; MAX_PLAINTEXT_CHUNK + 128];
    if initiator {
        let len = noise.write_message(&[], &mut buf)?;
        write_framed(io, &buf[..len]).await?;
        let message = read_framed(io).await?;
        noise
            .read_message(&message, &mut buf)
            .map_err(|_| TransportError::Decrypt)?;
    } else {
        let message = read_framed(io).await?;
        noise
            .read_message(&message, &mut buf)
            .map_err(|_| TransportError::Decrypt)?;
        if let Some(guard) = guard {
            if !guard.record(&message) {
                return Err(TransportError::Replay);
            }
        }
        let len = noise.write_message(&[], &mut buf)?;
        write_framed(io, &buf[..len]).await?;
    }
    Ok(())
}

/// Hand the completed handshake to the pump and return the channel.
async fn finish<S>(
    io: S,
    noise: HandshakeState,
    role: Role,
    expected_remote: [u8; 32],
) -> Result<SecureChannel, TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let handshake_remote = noise
        .get_remote_static()
        .map(<[u8]>::to_vec)
        .ok_or(TransportError::IdentityMismatch)?;
    if handshake_remote != expected_remote {
        // We pinned one key and the handshake authenticated another. Refuse
        // rather than continue with a channel we cannot reason about.
        return Err(TransportError::IdentityMismatch);
    }
    let transport = noise.into_transport_mode()?;
    let (ours, theirs) = tokio::io::duplex(64 * 1024);
    let failure: Arc<Mutex<Option<TransportError>>> = Arc::new(Mutex::new(None));
    let pump = tokio::spawn(pump(io, transport, theirs, Arc::clone(&failure)));
    Ok(SecureChannel {
        io: ours,
        remote: expected_remote,
        role,
        pump: Some(pump),
        failure,
    })
}

/// The frame length implied by a `u16` big-endian prefix, or `None` while the
/// header is incomplete.
fn frame_len(input: &[u8]) -> Option<usize> {
    if input.len() < 2 {
        return None;
    }
    Some(2 + u16::from_be_bytes([input[0], input[1]]) as usize)
}

async fn write_framed<W: AsyncWrite + Unpin>(io: &mut W, message: &[u8]) -> io::Result<()> {
    let len = u16::try_from(message.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "noise message exceeds the u16 framing limit",
        )
    })?;
    io.write_all(&len.to_be_bytes()).await?;
    io.write_all(message).await?;
    io.flush().await
}

async fn read_framed<R: AsyncRead + Unpin>(io: &mut R) -> Result<Vec<u8>, TransportError> {
    let mut header = [0u8; 2];
    io.read_exact(&mut header).await?;
    let len = u16::from_be_bytes(header) as usize;
    // Handshake messages are small; refuse an absurd length rather than
    // allocating whatever a hostile peer asks for.
    if len > MAX_PLAINTEXT_CHUNK + 128 {
        return Err(TransportError::BadHint);
    }
    let mut message = vec![0u8; len];
    io.read_exact(&mut message).await?;
    Ok(message)
}

/// The pump: the only owner of the Noise transport state.
///
/// Reads from the raw stream happen inside `select!` (and `AsyncReadExt::read`
/// is cancel-safe); the resulting writes happen *outside* it, because
/// cancelling a half-finished `write_all` would silently drop ciphertext.
///
/// Known limitation (v1): a write that blocks because the peer stopped reading
/// also stalls the other direction. The verb protocol is request/response (one
/// side reads while the other writes), so this cannot deadlock in practice;
/// fully independent per-direction buffers are not needed yet.
async fn pump<S>(
    io: S,
    mut noise: TransportState,
    mut duplex: DuplexStream,
    failure: Arc<Mutex<Option<TransportError>>>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    // Every abnormal exit records why, so the consumer learns the reason at EOF
    // instead of seeing a bare close.
    let record = |reason: TransportError| {
        if let Ok(mut slot) = failure.lock() {
            if slot.is_none() {
                *slot = Some(reason);
            }
        }
    };
    let (mut raw_r, mut raw_w) = tokio::io::split(io);
    let (mut plain_r, mut plain_w) = tokio::io::split(&mut duplex);

    let mut raw_in: Vec<u8> = Vec::new(); // ciphertext awaiting a full frame
    let mut to_consumer: Vec<u8> = Vec::new(); // plaintext awaiting delivery
    let mut to_peer: Vec<u8> = Vec::new(); // plaintext awaiting sealing
    let mut sealed: Vec<u8> = Vec::new(); // ciphertext awaiting the wire

    let mut raw_eof = false;
    let mut plain_eof = false;
    let mut sealed_buf = vec![0u8; MAX_PLAINTEXT_CHUNK + 128];
    // Two buffers: each `select!` branch borrows its own, so neither borrows
    // the other's.
    let mut raw_read_buf = vec![0u8; 32 * 1024];
    let mut plain_read_buf = vec![0u8; 32 * 1024];

    loop {
        // 1. Deliver anything already prepared.
        if !sealed.is_empty() {
            if raw_w.write_all(&sealed).await.is_err() || raw_w.flush().await.is_err() {
                return;
            }
            sealed.clear();
        }
        if !to_consumer.is_empty() {
            if plain_w.write_all(&to_consumer).await.is_err() {
                return;
            }
            to_consumer.clear();
        }
        // 2. Decrypt whole frames.
        while let Some(total) = frame_len(&raw_in) {
            // The length prefix is *not* authenticated (it cannot be: it is what
            // tells us how much to authenticate), so a peer — or anyone who can
            // flip a byte in transit — can make it name a frame far larger than
            // the seal ever produces. Waiting for that frame would stall the
            // session until the connection died, so an impossible length is
            // treated as what it is: a corrupted stream.
            if total > MAX_FRAME_BYTES {
                record(TransportError::Decrypt);
                return;
            }
            if raw_in.len() < total {
                break;
            }
            let frame: Vec<u8> = raw_in.drain(..total).collect();
            let message = &frame[2..];
            let mut out = vec![0u8; message.len()];
            match noise.read_message(message, &mut out) {
                Ok(len) => {
                    out.truncate(len);
                    to_consumer.extend_from_slice(&out);
                    if to_consumer.len() > MAX_BUFFERED_BYTES {
                        record(TransportError::TooMuchBuffered(to_consumer.len()));
                        return;
                    }
                }
                // A frame that fails authentication ends the channel: the
                // stream is no longer trustworthy (bytes are missing, changed or
                // reordered), so there is nothing to resynchronize to.
                Err(_) => {
                    record(TransportError::Decrypt);
                    return;
                }
            }
        }
        // 3. Seal what the protocol handed us.
        while !to_peer.is_empty() {
            let take = to_peer.len().min(MAX_PLAINTEXT_CHUNK);
            let chunk: Vec<u8> = to_peer.drain(..take).collect();
            match noise.write_message(&chunk, &mut sealed_buf) {
                Ok(len) => {
                    sealed.extend_from_slice(&(len as u16).to_be_bytes());
                    sealed.extend_from_slice(&sealed_buf[..len]);
                }
                Err(_) => return,
            }
        }
        // Deliver before waiting for more input: decrypted plaintext is only
        // flushed at the top of the loop, so without this the pump would block
        // in `select!` holding a reply the consumer is waiting for — a
        // request/response exchange that never completes.
        if !sealed.is_empty() || !to_consumer.is_empty() {
            continue;
        }
        if raw_eof && plain_eof {
            return;
        }
        // The **consumer is gone**: its end of the duplex is closed and there is
        // nothing left to hand it (`to_peer` was drained above, and a
        // non-empty `to_consumer` would have `continue`d). Waiting for the peer
        // to close too would keep the connection — and the session on the other
        // side — alive until the transport's idle timeout, which is exactly the
        // quarter-minute delay this branch removed (a client that exits is a
        // session that ended).
        if plain_eof {
            return;
        }

        tokio::select! {
            result = raw_r.read(&mut raw_read_buf), if !raw_eof => {
                match result {
                    Ok(0) => raw_eof = true,
                    Ok(n) => {
                        raw_in.extend_from_slice(&raw_read_buf[..n]);
                        if raw_in.len() > MAX_BUFFERED_BYTES {
                            record(TransportError::TooMuchBuffered(raw_in.len()));
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
            result = plain_r.read(&mut plain_read_buf), if !plain_eof => {
                match result {
                    Ok(0) => plain_eof = true,
                    Ok(n) => {
                        to_peer.extend_from_slice(&plain_read_buf[..n]);
                        if to_peer.len() > MAX_BUFFERED_BYTES {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::keys::DeviceKey;
    use crate::identity::DeviceId;
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    const CLIENT_SEED: [u8; 32] = [11u8; 32];
    const SERVER_SEED: [u8; 32] = [22u8; 32];

    /// No test in this module may hang: a pump regression used to deadlock the
    /// suite silently, so every exchange is bounded and fails loudly instead.
    const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);

    fn client_key() -> DeviceKey {
        DeviceKey::from_seed(CLIENT_SEED)
    }

    fn server_key() -> DeviceKey {
        DeviceKey::from_seed(SERVER_SEED)
    }

    fn client_id() -> String {
        DeviceId::from_key(&client_key().public()).display_id()
    }

    /// Accept side of a pair, resolving only the expected device. The guard is
    /// shared, because it must span connections to catch a replay.
    fn accept_task<S>(
        io: S,
        expected: &DeviceKey,
        guard: Arc<FlightGuard>,
    ) -> JoinHandle<Result<SecureChannel, TransportError>>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let local = server_key().noise_static();
        let expected_id = DeviceId::from_key(&expected.public());
        let expected_key = expected.public();
        tokio::spawn(async move {
            SecureChannel::accept(io, &local, &guard, move |hint| {
                (*hint == expected_id).then_some(expected_key)
            })
            .await
            // The device id is asserted by its own test; every other test only
            // needs the channel.
            .map(|(channel, _device)| channel)
        })
    }

    fn connect_future<S>(io: S) -> impl Future<Output = Result<SecureChannel, TransportError>>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        let local = client_key().noise_static();
        let server_identity = server_key().public();
        async move { SecureChannel::connect(io, &local, &client_id(), &server_identity).await }
    }

    /// A connected pair over in-memory streams (QUIC is exercised in `quic.rs`).
    async fn channel() -> (SecureChannel, SecureChannel) {
        let expected = client_key();
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server = accept_task(server_io, &expected, Arc::new(FlightGuard::default()));
        let client = connect_future(client_io).await.expect("client handshake");
        let server = server.await.expect("join").expect("server handshake");
        (client, server)
    }

    #[tokio::test]
    async fn a_secure_channel_carries_bytes_both_ways() {
        let (mut client, mut server) = channel().await;
        assert_eq!(client.role(), Role::Initiator);
        assert_eq!(server.role(), Role::Responder);

        client.write_all(b"verb").await.expect("write");
        client.flush().await.expect("flush");
        let mut got = [0u8; 4];
        tokio::time::timeout(EXCHANGE_TIMEOUT, server.read_exact(&mut got))
            .await
            .expect("server reads within the timeout")
            .expect("server reads");
        assert_eq!(&got, b"verb");

        server.write_all(b"reply").await.expect("write");
        server.flush().await.expect("flush");
        let mut back = [0u8; 5];
        tokio::time::timeout(EXCHANGE_TIMEOUT, client.read_exact(&mut back))
            .await
            .expect("client reads within the timeout")
            .expect("client reads");
        assert_eq!(&back, b"reply");

        // Both ends agree on the peer's static key (and it is the peer's, not
        // their own).
        let client_static = client_key().noise_static().public();
        let server_static = server_key().noise_static().public();
        assert_eq!(client.remote_static(), server_static);
        assert_eq!(server.remote_static(), client_static);
    }

    #[tokio::test]
    async fn a_large_payload_survives_segmentation_and_reassembly() {
        // Well past one Noise message, so this exercises chunking rather than a
        // single happy frame.
        let (mut client, mut server) = channel().await;
        let payload: Vec<u8> = (0..1_000_000u32).map(|i| (i % 251) as u8).collect();
        let expected = payload.clone();
        let writer = tokio::spawn(async move {
            let _ = client.write_all(&payload).await;
            let _ = client.flush().await;
            // `shutdown`, not a bare drop: a dropped channel is closed
            // immediately and discards what the pump had not yet put on the
            // wire, which for a megabyte is most of it. Flushing is explicit.
            client.shutdown().await;
        });
        let mut got = vec![0u8; expected.len()];
        tokio::time::timeout(EXCHANGE_TIMEOUT, server.read_exact(&mut got))
            .await
            .expect("the whole payload arrives within the timeout")
            .expect("the whole payload arrives");
        assert_eq!(got, expected);
        let _ = writer.await;
    }

    #[tokio::test]
    async fn the_responder_learns_the_device_id_it_authenticated() {
        // The daemon needs this: `last_seen`, the audit row and the per-verb
        // policy are all keyed by the device id, and the announcement is the
        // only place it exists (the authenticated key is an X25519 point, which
        // does not yield the ed25519 fingerprint).
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let local = server_key().noise_static();
        let announced = DeviceId::from_key(&client_key().public());
        let pinned = client_key().public();
        let resolved = announced.clone();
        let server = tokio::spawn(async move {
            SecureChannel::accept(server_io, &local, &FlightGuard::default(), move |hint| {
                (*hint == resolved).then_some(pinned)
            })
            .await
        });
        let client = connect_future(client_io).await.expect("client handshake");
        let (_channel, device) = server.await.expect("join").expect("server handshake");
        assert_eq!(
            device, announced,
            "the responder must report the device it authenticated"
        );
        drop(client);
    }

    #[tokio::test]
    async fn an_unknown_device_is_refused_without_running_cryptography() {
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let local = server_key().noise_static();
        let server = tokio::spawn(async move {
            SecureChannel::accept(server_io, &local, &FlightGuard::default(), |_hint| None).await
        });
        let client = connect_future(client_io).await;
        let server = server.await.expect("join");
        assert!(
            matches!(server, Err(TransportError::UnknownPeer)),
            "got {server:?}"
        );
        assert!(client.is_err(), "a refused handshake cannot complete");
    }

    #[tokio::test]
    async fn claiming_a_pinned_id_without_its_key_fails() {
        // The hint names a pinned device, but this process holds another secret,
        // so KK cannot finish: the impersonation fails on both ends.
        let expected = client_key();
        let impostor = DeviceKey::from_seed([33u8; 32]);
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server = accept_task(server_io, &expected, Arc::new(FlightGuard::default()));
        let impostor_static = impostor.noise_static();
        let server_identity = server_key().public();
        let client = SecureChannel::connect(
            client_io,
            &impostor_static,
            &client_id(), // claim the pinned device
            &server_identity,
        )
        .await;
        let server = server.await.expect("join");
        assert!(
            server.is_err(),
            "the server accepted an unprovable identity"
        );
        assert!(client.is_err(), "the impostor's handshake succeeded");
    }

    #[tokio::test]
    async fn pinning_the_wrong_server_fails() {
        let other_server = DeviceKey::from_seed([99u8; 32]);
        let expected = client_key();
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server = accept_task(server_io, &expected, Arc::new(FlightGuard::default()));
        let local = client_key().noise_static();
        let client = SecureChannel::connect(
            client_io,
            &local,
            &client_id(),
            &other_server.public(), // pin somebody else
        )
        .await;
        let server = server.await.expect("join");
        assert!(client.is_err(), "a mismatched server pin must fail");
        // The server completed a legitimate handshake with a pinned device: it
        // cannot know the client pinned the wrong key, and does not need to —
        // the client refuses and drops the connection, so the session carries
        // nothing.
        if let Ok(mut channel) = server {
            let mut buf = [0u8; 1];
            let read = tokio::time::timeout(Duration::from_secs(1), channel.read(&mut buf)).await;
            assert!(
                matches!(read, Ok(Ok(0)) | Err(_)),
                "the abandoned session must carry no data: {read:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_flipped_byte_in_the_handshake_prevents_a_session() {
        // A relay that corrupts one byte of the client's first flight.
        let (client_io, mut relay_in) = tokio::io::duplex(64 * 1024);
        let (mut relay_out, server_io) = tokio::io::duplex(64 * 1024);
        let relay = tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            let n = relay_in.read(&mut buf).await.expect("a flight");
            assert!(n > 8, "expected a handshake flight, got {n} bytes");
            buf[n / 2] ^= 0x01;
            let _ = relay_out.write_all(&buf[..n]).await;
        });
        let server = accept_task(server_io, &client_key(), Arc::new(FlightGuard::default()));
        let client = connect_future(client_io).await;
        let server = server.await.expect("join");
        assert!(
            client.is_err(),
            "the client's view of the handshake must fail"
        );
        assert!(server.is_err(), "the server's view must fail too");
        let _ = relay.await;
    }

    #[tokio::test]
    async fn a_flipped_byte_in_a_transport_frame_is_never_delivered_as_plaintext() {
        // The tamper criterion, one layer up: corrupt bytes *after* the
        // handshake while real payload is in flight, and prove the server never
        // receives the payload.
        let (client_io, relay_in) = tokio::io::duplex(256 * 1024);
        let (relay_out, server_io) = tokio::io::duplex(256 * 1024);
        let (mut relay_in_r, mut relay_in_w) = tokio::io::split(relay_in);
        let (mut relay_out_r, mut relay_out_w) = tokio::io::split(relay_out);
        // The relay corrupts every byte it forwards *after* the handshake. A
        // fixed byte offset would rot silently: the handshake (32-byte hint plus
        // two 144-byte flights) is 320 bytes, so any cutoff chosen by hand
        // either eats the handshake or misses the payload. The flag is raised
        // once both ends report a completed handshake — the boundary this test
        // is actually about — and the relay is blocked in `read` by then, so the
        // next bytes it sees are payload.
        //
        // Both directions are pumped: a relay that only carried client→server
        // would starve the handshake it is supposed to leave intact.
        let tamper = Arc::new(AtomicBool::new(false));
        let relay = tokio::spawn({
            let tamper = Arc::clone(&tamper);
            let forward = async move {
                let mut buf = vec![0u8; 8192];
                let mut tampered_bytes = 0usize;
                loop {
                    let n = match relay_in_r.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    if tamper.load(Ordering::SeqCst) {
                        for byte in &mut buf[..n] {
                            // The first two bytes are the frame's length prefix,
                            // and the criterion is about a flipped byte of the
                            // *frame*. Leaving the prefix alone keeps this
                            // deterministic: corrupting it could shorten the
                            // frame instead of failing its authentication, and
                            // a stream cannot tell a short frame from one still
                            // in flight. The prefix case has its own test below.
                            if tampered_bytes >= 2 {
                                *byte ^= 0x01;
                            }
                            tampered_bytes += 1;
                        }
                    }
                    if relay_out_w.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
            };
            let backward = async move {
                let _ = tokio::io::copy(&mut relay_out_r, &mut relay_in_w).await;
            };
            async move {
                tokio::join!(forward, backward);
            }
        });

        let expected = client_key();
        let server = accept_task(server_io, &expected, Arc::new(FlightGuard::default()));
        let client = connect_future(client_io).await;
        let server = server.await.expect("join");
        // Nothing was corrupted yet, so both ends must have connected.
        let mut client = client.expect("handshake should survive: corruption is post-handshake");
        let mut server = server.expect("handshake should survive");

        // From here on every forwarded byte is corrupted, and the payload is
        // sealed into one frame, so the peer must never see any of it.
        tamper.store(true, Ordering::SeqCst);
        let payload = vec![7u8; 4096];
        client.write_all(&payload).await.expect("write");
        client.flush().await.expect("flush");

        let mut got = Vec::new();
        let read =
            tokio::time::timeout(Duration::from_millis(750), server.read_to_end(&mut got)).await;
        assert!(
            got.is_empty(),
            "tampered ciphertext was delivered as plaintext ({} bytes arrived)",
            got.len()
        );
        // And the caller is told *why* the stream ended: the typed error, not a
        // bare close it would have to guess about.
        let error = read
            .expect("the tampered stream ends promptly")
            .expect_err("a tampered stream must fail, not end cleanly");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData, "{error}");
        let typed = error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<TransportError>())
            .expect("the failure must carry the typed transport error");
        assert!(
            matches!(typed, TransportError::Decrypt),
            "a flipped byte must be reported as Decrypt, got {typed:?}"
        );
        relay.abort();
    }

    #[tokio::test]
    async fn an_impossible_frame_length_is_refused_instead_of_waited_on() {
        // The length prefix cannot be authenticated — it is what tells us how
        // much to authenticate — so a peer can lie about it. A length no seal
        // can produce must end the channel at once: waiting for the rest of a
        // frame that will never arrive would hold the session open until the
        // transport gave up.
        let (client_io, relay_in) = tokio::io::duplex(256 * 1024);
        let (relay_out, server_io) = tokio::io::duplex(256 * 1024);
        let (mut relay_in_r, mut relay_in_w) = tokio::io::split(relay_in);
        let (mut relay_out_r, mut relay_out_w) = tokio::io::split(relay_out);
        let tamper = Arc::new(AtomicBool::new(false));
        let relay = tokio::spawn({
            let tamper = Arc::clone(&tamper);
            let forward = async move {
                let mut buf = vec![0u8; 8192];
                let mut rewritten = false;
                loop {
                    let n = match relay_in_r.read(&mut buf).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    if tamper.load(Ordering::SeqCst) && !rewritten && n >= 2 {
                        // 0xFFFF is larger than any sealed frame.
                        buf[0] = 0xFF;
                        buf[1] = 0xFF;
                        rewritten = true;
                    }
                    if relay_out_w.write_all(&buf[..n]).await.is_err() {
                        return;
                    }
                }
            };
            let backward = async move {
                let _ = tokio::io::copy(&mut relay_out_r, &mut relay_in_w).await;
            };
            async move {
                tokio::join!(forward, backward);
            }
        });

        let expected = client_key();
        let server = accept_task(server_io, &expected, Arc::new(FlightGuard::default()));
        let client = connect_future(client_io).await;
        let server = server.await.expect("join");
        let mut client = client.expect("handshake survives: corruption is post-handshake");
        let mut server = server.expect("handshake survives");

        tamper.store(true, Ordering::SeqCst);
        client.write_all(&[7u8; 64]).await.expect("write");
        client.flush().await.expect("flush");

        let mut got = Vec::new();
        let read = tokio::time::timeout(EXCHANGE_TIMEOUT, server.read_to_end(&mut got)).await;
        assert!(got.is_empty(), "no plaintext may arrive");
        let error = read
            .expect("an impossible length must fail fast, not stall")
            .expect_err("the stream must fail");
        let typed = error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<TransportError>())
            .expect("the failure must carry the typed transport error");
        assert!(
            matches!(typed, TransportError::Decrypt),
            "an impossible frame length must be reported as corruption, got {typed:?}"
        );
        relay.abort();
    }

    #[tokio::test]
    async fn a_replayed_handshake_flight_does_not_establish_a_session() {
        // Record the client's first flight, then replay it into a fresh server.
        // The responder's ephemeral differs, so the recorded flight cannot
        // produce shared keys.
        let (client_io, recorder) = tokio::io::duplex(64 * 1024);
        let (sink, server_io) = tokio::io::duplex(64 * 1024);
        let (mut rec_r, mut rec_w) = tokio::io::split(recorder);
        let (mut snk_r, mut snk_w) = tokio::io::split(sink);
        let capture = tokio::spawn(async move {
            // Read exactly the announcement and the framed flight: a single
            // `read` can stop at the hint, and replaying half a flight proves
            // nothing about replaying a real one.
            let mut hint = [0u8; HINT_LEN];
            rec_r.read_exact(&mut hint).await.expect("hint");
            let mut header = [0u8; 2];
            rec_r.read_exact(&mut header).await.expect("flight header");
            let mut flight = vec![0u8; u16::from_be_bytes(header) as usize];
            rec_r.read_exact(&mut flight).await.expect("flight");
            let mut recorded = hint.to_vec();
            recorded.extend_from_slice(&header);
            recorded.extend_from_slice(&flight);
            // Forward it so the recorded client also completes its handshake.
            snk_w.write_all(&recorded).await.expect("forward");
            recorded
        });
        // The server's flight travels the other way; without it the recorded
        // handshake could never complete and the test would prove nothing.
        let reverse = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut snk_r, &mut rec_w).await;
        });
        // One guard for both servers: it is the state that makes the replay
        // detectable, so a fresh one per connection would prove nothing.
        let guard = Arc::new(FlightGuard::default());
        let server = accept_task(server_io, &client_key(), Arc::clone(&guard));
        let client = connect_future(client_io).await;
        let recorded = capture.await.expect("capture");
        // The recorded session is a real one — otherwise the replay below would
        // be refused for the wrong reason.
        client.expect("the recorded handshake must complete");
        server
            .await
            .expect("join")
            .expect("the recorded session must be accepted");

        let (mut replayer, fresh_io) = tokio::io::duplex(64 * 1024);
        let fresh_server = accept_task(fresh_io, &client_key(), Arc::clone(&guard));
        // Replay the *whole* recorded prefix (hint + flight) into a new server.
        replayer.write_all(&recorded).await.expect("replay");
        let outcome = tokio::time::timeout(Duration::from_secs(2), fresh_server).await;
        // The replayed flight is *authentic* — KK's first message depends only on
        // the responder's static key, which does not change — so decryption
        // cannot catch it; the guard must. Anything else is a failure.
        match outcome {
            Ok(Ok(Err(TransportError::Replay))) => {}
            Ok(Ok(Ok(_))) => panic!("a replayed flight established a session"),
            other => panic!("expected a replay refusal, got {other:?}"),
        }
        reverse.abort();
    }
}
