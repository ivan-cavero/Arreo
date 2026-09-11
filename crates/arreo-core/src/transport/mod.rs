//! Remote transport (T-0023): Noise-encrypted sessions between pinned devices.
//!
//! One sentence: a remote client gets the *same* protocol as a local one — the
//! [noise] channel is an `AsyncRead + AsyncWrite` byte stream, so everything
//! above it (framing, `Hello`/`Welcome`, every verb) is the code the unix socket
//! already uses, and the transport only has to get bytes to the right peer
//! securely.
//!
//! Layers:
//! - [`noise`] — the authenticated, encrypted byte stream (Noise-KK over the
//!   pinned ed25519 identities).
//! - [`quic`] — endpoints: what carries those bytes across a network, with the
//!   TLS-as-transport-detail configuration and the handshake rate limiter.
//!
//! Posture (ROADMAP §4, "zero inbound ports"): the shipped daemon opens no
//! inbound listener. The remote path in production is the daemon dialling *out*
//! to a relay (T-0029); until that lands, the only listener is the loopback test
//! seam behind `ARREO_TRANSPORT_TEST_LISTEN`.

pub mod noise;
pub mod quic;

pub use noise::{FlightGuard, Role, SecureChannel, TransportError, HANDSHAKE_TIMEOUT};
pub use quic::{
    accept_connection, accept_session, client_endpoint, open_session, server_endpoint,
    HandshakeLimiter, QuicError, RemoteSession, ALPN, SERVER_NAME, TEST_LISTEN_ENV,
};
/// The QUIC types, re-exported so a consumer of this transport (the daemon)
/// depends on *this* module rather than on quinn: one place to audit the
/// transport stack, and one place to swap it.
pub use quinn::{Connection, Endpoint};
