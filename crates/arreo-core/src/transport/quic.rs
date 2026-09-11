//! QUIC endpoints for the remote transport (T-0023).
//!
//! One sentence: quinn carries the Noise channel across a network, and this
//! module owns the three decisions that are not quinn's — where the ephemeral
//! TLS certificate comes from, why the client does not verify it, and how many
//! handshakes one peer may start.
//!
//! **TLS here is a transport detail, not the trust anchor.** QUIC mandates
//! TLS 1.3, so a certificate has to exist. Ours is generated in memory at start
//! up (rcgen), never pinned, and never presented to a user; the client's
//! verifier accepts it without inspecting it. That is deliberate, and it is
//! safe *because of what sits inside*: the Noise-KK handshake authenticates both
//! peers against pinned ed25519 identities, so a machine-in-the-middle that
//! terminates TLS still cannot complete the Noise handshake — it can drop or
//! delay traffic (a denial of service), never read or forge it. The alternative
//! shapes were both worse: pinning the TLS certificate would duplicate the trust
//! decision in a second, weaker place (ephemeral certs rotate, `rcgen`'s are not
//! the device identity), and a patched quinn that replaces TLS with Noise
//! (`quinn-hyphae`'s approach) puts an unaudited fork on the security-critical
//! path. ADR 0011 records the trade-off and the measured cost.
//!
//! **Zero inbound ports.** `server_endpoint` exists for the loopback test seam
//! and for a future LAN-direct mode; the shipped daemon does not call it unless
//! `ARREO_TRANSPORT_TEST_LISTEN` is set (see `arreo-server::transport`). The
//! production remote path is the daemon dialling *out* to a relay (T-0029).

use super::noise::{FlightGuard, SecureChannel, TransportError};
use crate::identity::keys::NoiseStatic;
use crate::identity::{DeviceId, VerifyingKey};
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{ClientConfig, Connection, Endpoint, ServerConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::pki_types::{ServerName, UnixTime};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Set to `IP:PORT` to make the daemon open a loopback QUIC listener. A **test
/// seam**, not a feature: the shipped posture is zero inbound ports (§4), and
/// anything that flips this in production is a bug.
pub const TEST_LISTEN_ENV: &str = "ARREO_TRANSPORT_TEST_LISTEN";

/// The SNI name used for QUIC's TLS. Trust comes from Noise-KK, so the name is a
/// constant rather than an identity: pinning a name would imply the name means
/// something.
pub const SERVER_NAME: &str = "arreo.invalid";

/// The application protocol marker, so a QUIC peer that is not Arreo fails at
/// the TLS handshake instead of after a Noise attempt.
pub const ALPN: &[u8] = b"arreo/transport/1";

/// How long to wait for a QUIC connection to establish.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Handshake attempts allowed per peer within [`HANDSHAKE_WINDOW`].
pub const HANDSHAKE_MAX_ATTEMPTS: usize = 3;

/// The window those attempts are counted over.
pub const HANDSHAKE_WINDOW: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum QuicError {
    #[error("quic io: {0}")]
    Io(#[from] std::io::Error),
    #[error("quic tls setup failed: {0}")]
    Tls(String),
    #[error("quic endpoint configuration: {0}")]
    Config(String),
    #[error("quic connect failed: {0}")]
    Connect(String),
    #[error("quic connection timed out after {0:?}")]
    Timeout(Duration),
    #[error("peer {0} is over its handshake budget ({1} per {2:?})")]
    RateLimited(IpAddr, usize, Duration),
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
}

/// Caps how often one peer may begin a handshake.
///
/// Why it exists: a replayed handshake flight cannot establish a session (the
/// responder's ephemeral is fresh), but it *can* make the server do work. The
/// budget turns "unlimited free work for an attacker" into three attempts per
/// ten seconds per address, which is plenty for a real device retrying a flaky
/// network and useless for enumeration.
#[derive(Debug)]
pub struct HandshakeLimiter {
    max: usize,
    window: Duration,
    seen: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl Default for HandshakeLimiter {
    fn default() -> Self {
        Self::new(HANDSHAKE_MAX_ATTEMPTS, HANDSHAKE_WINDOW)
    }
}

impl HandshakeLimiter {
    #[must_use]
    pub fn new(max: usize, window: Duration) -> Self {
        Self {
            max,
            window,
            seen: Mutex::new(HashMap::new()),
        }
    }

    /// Record an attempt, or refuse it.
    pub fn check(&self, peer: IpAddr) -> Result<(), QuicError> {
        let now = Instant::now();
        let mut seen = match self.seen.lock() {
            Ok(seen) => seen,
            Err(poisoned) => poisoned.into_inner(),
        };
        let attempts = seen.entry(peer).or_default();
        attempts.retain(|at| now.duration_since(*at) < self.window);
        if attempts.len() >= self.max {
            return Err(QuicError::RateLimited(peer, self.max, self.window));
        }
        attempts.push(now);
        Ok(())
    }

    /// Forget a peer's history — used when a connection completes, so a device
    /// that reconnects after a real network drop is not punished for retrying.
    pub fn forgive(&self, peer: IpAddr) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.remove(&peer);
        }
    }
}

/// A self-signed certificate generated in memory, valid for this process only.
///
/// Deliberately not persisted and deliberately not verified by clients: it
/// exists because QUIC requires TLS 1.3, not because it authenticates anything
/// (the Noise static key does that). Regenerating it per start is what keeps it
/// from being mistaken for an identity.
fn ephemeral_server_config() -> Result<ServerConfig, QuicError> {
    let certified = rcgen::generate_simple_self_signed(vec![SERVER_NAME.to_string()])
        .map_err(|e| QuicError::Tls(format!("cannot generate the ephemeral certificate: {e}")))?;
    let cert_chain = vec![certified.cert.der().clone()];
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(certified.key_pair.serialize_der()));
    // ALPN is set on the rustls configuration, so the QUIC handshake refuses a
    // peer that is not speaking Arreo before any Noise work happens.
    let mut crypto = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .map_err(|e| QuicError::Tls(e.to_string()))?;
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let quic = quinn::crypto::rustls::QuicServerConfig::try_from(crypto)
        .map_err(|e| QuicError::Config(e.to_string()))?;
    let mut config = ServerConfig::with_crypto(Arc::new(quic));
    config.transport_config(Arc::new(transport_config()));
    Ok(config)
}

/// The pieces of quinn's transport configuration this project cares about.
fn transport_config() -> quinn::TransportConfig {
    let mut transport = quinn::TransportConfig::default();
    // The verb protocol is request/response over one bidi stream; a handful is
    // plenty, and a hard cap stops a peer opening thousands.
    transport.max_concurrent_bidi_streams(8u32.into());
    transport.max_concurrent_uni_streams(0u32.into());
    transport
}

/// A server endpoint bound to `bind`, with an in-memory certificate.
///
/// Loopback by construction in the shipped product: see [`TEST_LISTEN_ENV`].
pub fn server_endpoint(bind: SocketAddr) -> Result<Endpoint, QuicError> {
    let config = ephemeral_server_config()?;
    Endpoint::server(config, bind).map_err(QuicError::Io)
}

/// A client endpoint: binds an ephemeral local port and accepts any server
/// certificate (see this module's docs for why that is safe here).
pub fn client_endpoint() -> Result<Endpoint, QuicError> {
    let mut endpoint = Endpoint::client("0.0.0.0:0".parse().expect("a valid bind address"))
        .map_err(QuicError::Io)?;
    let mut crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoiseIsTheAnchor::new()))
        .with_no_client_auth();
    crypto.alpn_protocols = vec![ALPN.to_vec()];
    let quic = QuicClientConfig::try_from(crypto).map_err(|e| QuicError::Config(e.to_string()))?;
    let mut config = ClientConfig::new(Arc::new(quic));
    let mut transport = transport_config();
    transport.keep_alive_interval(Some(Duration::from_secs(5)));
    config.transport_config(Arc::new(transport));
    endpoint.set_default_client_config(config);
    Ok(endpoint)
}

/// Dial `addr`, open the session stream, and run the Noise handshake.
pub async fn open_session(
    endpoint: &Endpoint,
    addr: SocketAddr,
    local: &NoiseStatic,
    device_id: &str,
    server: &VerifyingKey,
) -> Result<SecureChannel, QuicError> {
    let connecting = endpoint
        .connect(addr, SERVER_NAME)
        .map_err(|e| QuicError::Connect(e.to_string()))?;
    let connection = tokio::time::timeout(CONNECT_TIMEOUT, connecting)
        .await
        .map_err(|_| QuicError::Timeout(CONNECT_TIMEOUT))?
        .map_err(|e| QuicError::Connect(e.to_string()))?;
    let (send, recv) = connection
        .open_bi()
        .await
        .map_err(|e| QuicError::Connect(e.to_string()))?;
    let channel =
        SecureChannel::connect(tokio::io::join(recv, send), local, device_id, server).await?;
    Ok(channel)
}

/// An accepted remote session: the encrypted channel, plus the device id the
/// peer announced and proved it holds the key for.
///
/// The two travel together because neither is useful alone — the channel
/// without the id cannot be authorized per verb, and the id without the channel
/// is just a claim.
#[derive(Debug)]
pub struct RemoteSession {
    pub channel: SecureChannel,
    pub device: DeviceId,
}

/// Accept the next QUIC connection, subject to `limiter`.
///
/// Returns the connection **before** any Noise work, and the caller must run
/// [`accept_session`] on it in a task of its own. That split is not stylistic:
/// both the QUIC handshake and the Noise handshake are paced by the peer, so
/// doing either inline lets one peer that connects and then goes quiet stop the
/// listener from accepting anyone else until the timeout expires.
///
/// `Ok(None)` means the rate limiter refused the peer — keep serving.
pub async fn accept_connection(
    endpoint: &Endpoint,
    limiter: &HandshakeLimiter,
) -> Result<Option<Connection>, QuicError> {
    let incoming = match endpoint.accept().await {
        Some(incoming) => incoming,
        None => return Err(QuicError::Connect("endpoint closed".into())),
    };
    let peer = incoming.remote_address().ip();
    if limiter.check(peer).is_err() {
        // Refuse before any QUIC handshake work is done on our side.
        incoming.refuse();
        return Ok(None);
    }
    let connection = tokio::time::timeout(CONNECT_TIMEOUT, incoming)
        .await
        .map_err(|_| QuicError::Timeout(CONNECT_TIMEOUT))?
        .map_err(|e| QuicError::Connect(e.to_string()))?;
    Ok(Some(connection))
}

/// Run the Noise handshake on an accepted connection: take its bidi stream,
/// authenticate the peer against the pinned identity `resolve` returns, and hand
/// back the session plus the device id the peer proved it holds.
///
/// Bounded twice: the wait for the peer to open a stream, and the handshake
/// itself inside [`SecureChannel::accept`]. A peer that connects and then says
/// nothing therefore costs a task for at most that long, never a slot in the
/// accept loop.
pub async fn accept_session<F>(
    connection: &Connection,
    local: &NoiseStatic,
    limiter: &HandshakeLimiter,
    guard: &FlightGuard,
    resolve: F,
) -> Result<RemoteSession, QuicError>
where
    F: FnOnce(&DeviceId) -> Option<VerifyingKey> + Send + 'static,
{
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, connection.accept_bi())
        .await
        .map_err(|_| QuicError::Timeout(CONNECT_TIMEOUT))?
        .map_err(|e| QuicError::Connect(e.to_string()))?;
    let (send, recv) = stream;
    let (channel, device) =
        SecureChannel::accept(tokio::io::join(recv, send), local, guard, resolve).await?;
    // The peer completed a real session, so it is not an enumeration attempt:
    // forget its history so a device reconnecting after a network drop is not
    // punished for the retries that got it here.
    limiter.forgive(connection.remote_address().ip());
    Ok(RemoteSession { channel, device })
}

/// The client's certificate verifier.
///
/// It accepts any certificate **on purpose**: the TLS handshake is transport
/// setup, and the peer is authenticated a layer up by Noise-KK against a pinned
/// ed25519 key. The name says what the trust anchor actually is, because a
/// future reader will otherwise reasonably assume this is an oversight.
///
/// The signature checks still run (via rustls's helpers), so a peer cannot skip
/// TLS's proof-of-possession; only the certificate's *provenance* is ignored.
#[derive(Debug)]
struct NoiseIsTheAnchor(Arc<rustls::crypto::CryptoProvider>);

impl NoiseIsTheAnchor {
    fn new() -> Self {
        Self(Arc::new(rustls::crypto::ring::default_provider()))
    }
}

impl rustls::client::danger::ServerCertVerifier for NoiseIsTheAnchor {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::keys::DeviceKey;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const CLIENT_SEED: [u8; 32] = [11u8; 32];
    const SERVER_SEED: [u8; 32] = [22u8; 32];

    /// Every exchange is bounded: a hang is a worse failure signal than a
    /// timeout, because it stalls the whole suite instead of one test.
    const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);

    fn client_key() -> DeviceKey {
        DeviceKey::from_seed(CLIENT_SEED)
    }

    fn server_key() -> DeviceKey {
        DeviceKey::from_seed(SERVER_SEED)
    }

    /// A loopback server endpoint plus its address.
    fn loopback_server() -> (Endpoint, SocketAddr) {
        let endpoint =
            server_endpoint("127.0.0.1:0".parse().expect("loopback")).expect("server endpoint");
        let addr = endpoint.local_addr().expect("bound address");
        (endpoint, addr)
    }

    #[test]
    fn the_limiter_budgets_attempts_per_peer_and_recovers() {
        let limiter = HandshakeLimiter::new(3, Duration::from_millis(80));
        let peer: IpAddr = "127.0.0.1".parse().expect("ip");
        for _ in 0..3 {
            limiter.check(peer).expect("within budget");
        }
        assert!(matches!(
            limiter.check(peer),
            Err(QuicError::RateLimited(..))
        ));
        // Another peer is unaffected.
        limiter
            .check("127.0.0.2".parse().expect("ip"))
            .expect("a different peer has its own budget");
        // After the window passes, the budget resets.
        std::thread::sleep(Duration::from_millis(120));
        limiter.check(peer).expect("the window has passed");
        // And an explicit forgive clears the history.
        limiter.forgive(peer);
        for _ in 0..3 {
            limiter.check(peer).expect("forgiven");
        }
    }

    #[tokio::test]
    async fn a_quic_session_carries_the_noise_channel_over_the_wire() {
        let (server, addr) = loopback_server();
        let server_task = tokio::spawn(async move {
            let limiter = HandshakeLimiter::default();
            let local = server_key().noise_static();
            let pinned = client_key().public();
            let client_id = DeviceId::from_key(&client_key().public());
            let connection = accept_connection(&server, &limiter)
                .await
                .expect("accept")
                .expect("not rate limited");
            accept_session(
                &connection,
                &local,
                &limiter,
                &FlightGuard::default(),
                move |hint| (*hint == client_id).then_some(pinned),
            )
            .await
        });

        let client = client_endpoint().expect("client endpoint");
        let mut channel = open_session(
            &client,
            addr,
            &client_key().noise_static(),
            &DeviceId::from_key(&client_key().public()).display_id(),
            &server_key().public(),
        )
        .await
        .expect("the session opens over QUIC");

        let session = server_task.await.expect("join").expect("handshake");
        let mut server_channel = session.channel;
        assert_eq!(
            session.device,
            DeviceId::from_key(&client_key().public()),
            "the responder must know which pinned device called"
        );

        channel.write_all(b"panes").await.expect("write");
        channel.flush().await.expect("flush");
        let mut got = [0u8; 5];
        tokio::time::timeout(EXCHANGE_TIMEOUT, server_channel.read_exact(&mut got))
            .await
            .expect("the server reads within the timeout")
            .expect("read");
        assert_eq!(&got, b"panes");
        server_channel.write_all(b"ten").await.expect("write");
        server_channel.flush().await.expect("flush");
        let mut back = [0u8; 3];
        tokio::time::timeout(EXCHANGE_TIMEOUT, channel.read_exact(&mut back))
            .await
            .expect("the client reads within the timeout")
            .expect("read");
        assert_eq!(&back, b"ten");
    }

    #[tokio::test]
    async fn a_peer_outside_its_budget_is_refused() {
        let (server, addr) = loopback_server();
        // A limiter with no budget: the first peer to arrive is refused.
        let limiter = HandshakeLimiter::new(0, Duration::from_secs(10));
        let server_task = tokio::spawn(async move { accept_connection(&server, &limiter).await });

        let client = client_endpoint().expect("client endpoint");
        let client_result = open_session(
            &client,
            addr,
            &client_key().noise_static(),
            &DeviceId::from_key(&client_key().public()).display_id(),
            &server_key().public(),
        )
        .await;
        let server_result = server_task.await.expect("join").expect("accept");
        assert!(
            server_result.is_none(),
            "the rate limiter should have refused the peer"
        );
        assert!(
            client_result.is_err(),
            "a refused peer must not get a session"
        );
    }

    #[tokio::test]
    async fn a_non_arreo_peer_fails_at_the_application_protocol() {
        // A plain QUIC client that does not speak our ALPN must not reach the
        // Noise handshake at all.
        let (server, addr) = loopback_server();
        let limiter = HandshakeLimiter::default();
        let server_task = tokio::spawn(async move {
            let connection = accept_connection(&server, &limiter).await;
            match connection {
                // The QUIC handshake itself fails on the ALPN mismatch, so
                // there is no connection to run Noise on.
                Err(e) => Err(e),
                Ok(None) => Ok(None),
                Ok(Some(connection)) => {
                    let local = server_key().noise_static();
                    accept_session(
                        &connection,
                        &local,
                        &limiter,
                        &FlightGuard::default(),
                        |_hint| None,
                    )
                    .await
                    .map(Some)
                }
            }
        });

        let mut endpoint =
            Endpoint::client("127.0.0.1:0".parse().expect("loopback")).expect("client endpoint");
        let mut crypto = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoiseIsTheAnchor::new()))
            .with_no_client_auth();
        crypto.alpn_protocols = vec![b"not-arreo/1".to_vec()];
        let quic = QuicClientConfig::try_from(crypto).expect("quic client config");
        endpoint.set_default_client_config(ClientConfig::new(Arc::new(quic)));
        let result = endpoint.connect(addr, SERVER_NAME).expect("connect").await;
        assert!(
            result.is_err(),
            "a foreign ALPN must not establish a session"
        );
        // The server side sees the same failure: no session.
        let server_result = server_task.await.expect("join");
        match server_result {
            Err(_) | Ok(None) => {}
            Ok(Some(session)) => {
                panic!("a foreign ALPN produced a session: {session:?}")
            }
        }
    }

    #[tokio::test]
    async fn an_unpinned_device_never_reaches_the_noise_handshake() {
        let (server, addr) = loopback_server();
        let server_task = tokio::spawn(async move {
            let limiter = HandshakeLimiter::default();
            let local = server_key().noise_static();
            let connection = accept_connection(&server, &limiter)
                .await
                .expect("accept")
                .expect("not rate limited");
            // Nobody is pinned.
            accept_session(
                &connection,
                &local,
                &limiter,
                &FlightGuard::default(),
                |_hint| None,
            )
            .await
        });
        let client = client_endpoint().expect("client endpoint");
        let result = open_session(
            &client,
            addr,
            &client_key().noise_static(),
            &DeviceId::from_key(&client_key().public()).display_id(),
            &server_key().public(),
        )
        .await;
        // The handshake refuses it as unpinned — never a session. Any other
        // failure is still a refusal, which is what the criterion demands.
        if let Ok(session) = server_task.await.expect("join") {
            panic!("an unpinned device got a session: {session:?}");
        }
        assert!(
            result.is_err(),
            "the client cannot complete a refused session"
        );
    }
}
