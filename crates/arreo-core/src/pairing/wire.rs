//! The pairing mailbox: wire types and client (T-0024).
//!
//! One sentence: the mailbox is a public bulletin board for four opaque
//! flights, addressed by an unguessable session id — the relay can order
//! them, cannot read them, and cannot forge one.
//!
//! Four slots, written in this order:
//!
//! ```text
//!   slot a  server → phone   SPAKE2 flight A
//!   slot b  phone  → server  SPAKE2 flight B
//!   slot c  phone  → server  device public key + name, MAC'd under the shared key
//!   slot d  server → phone   the signed device certificate, MAC'd
//! ```
//!
//! Rules the relay enforces (see `arreo_relay::pairing`):
//! - a session id is **single use**: opening one that is live or burned fails;
//! - each slot is **write-once**, so nobody can replace a flight after the
//!   fact (a forged early write causes a failed confirmation, i.e. a DoS, not
//!   an impersonation — the MAC is keyed by the code);
//! - a session **expires** (`ttl_secs` is fixed at open) and its payloads are
//!   dropped then.
//!
//! Frames are one JSON object per line with base64 payloads. JSON because the
//! relay operator should be able to *see* that it is carrying opaque blobs —
//! "the relay cannot read this" is a claim about the payloads, not about the
//! protocol, and an inspectable mailbox makes that auditable.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
#[cfg(unix)]
use std::path::PathBuf;
use std::time::Duration;

use super::PairingError;

/// How long a mailbox request may take before the client gives up. Pairing is
/// interactive: a mailbox that does not answer promptly is broken, and waiting
/// forever would look like "the code is wrong".
pub const MAILBOX_TIMEOUT: Duration = Duration::from_secs(10);

/// One of the four flights.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    /// Server's SPAKE2 flight.
    A,
    /// Phone's SPAKE2 flight.
    B,
    /// Phone's device public key + name, authenticated under the shared key.
    C,
    /// Server's signed certificate, authenticated under the shared key.
    D,
}

impl Slot {
    /// Every slot, in protocol order (used for cleanup and tests).
    pub const ALL: [Slot; 4] = [Slot::A, Slot::B, Slot::C, Slot::D];
}

/// Where the mailbox lives. Unix socket for a relay on this machine (the
/// loopback shape T-0024 ships and tests); `host:port` for the relay across a
/// network, which is what the QR carries and the only shape Windows has (a
/// named-pipe mailbox is not part of T-0024).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailboxAddr {
    #[cfg(unix)]
    Unix(PathBuf),
    Tcp(String),
}

impl MailboxAddr {
    /// Parse an address from a URI query value: a path, or `host:port`.
    pub fn parse(raw: &str) -> Result<Self, PairingError> {
        let text = raw.trim();
        if text.is_empty() {
            return Err(PairingError::BadInvite("empty mailbox address".into()));
        }
        if text.starts_with('/') || text.starts_with('.') {
            // A filesystem path is a unix-socket address; on Windows there is
            // no such thing (yet), and saying so beats failing to connect.
            #[cfg(unix)]
            {
                return Ok(Self::Unix(PathBuf::from(text)));
            }
            #[cfg(not(unix))]
            {
                return Err(PairingError::BadInvite(format!(
                    "{text:?} is a unix socket path, which this build cannot use; \
                     give the mailbox as host:port"
                )));
            }
        }
        // `host:port`, with the port mandatory: a default port would silently
        // send a pairing to the wrong service.
        match text.rsplit_once(':') {
            Some((host, port)) if !host.is_empty() && port.parse::<u16>().is_ok() => {
                Ok(Self::Tcp(text.to_string()))
            }
            _ => Err(PairingError::BadInvite(format!(
                "mailbox address {text:?} is neither a path nor host:port"
            ))),
        }
    }

    /// The value that goes in the invite URI.
    #[must_use]
    pub fn as_str(&self) -> String {
        match self {
            #[cfg(unix)]
            Self::Unix(path) => path.display().to_string(),
            Self::Tcp(host) => host.clone(),
        }
    }
}

/// A mailbox request: exactly one per connection (the ADR 0007 convention —
/// no session state on the relay, so a relay restart cannot strand a client).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum MailboxRequest {
    /// Claim a session id for `ttl_secs`. Fails if it is live or burned.
    Open { session: String, ttl_secs: u64 },
    /// Publish one flight. Fails if the slot already holds something.
    Put {
        session: String,
        slot: Slot,
        /// Base64 of the flight bytes.
        payload: String,
    },
    /// Read one flight. `payload: null` means "not published yet".
    Get { session: String, slot: Slot },
    /// Close a session: drop the payloads and refuse the id forever.
    Burn { session: String },
    /// Is this session live, and how long has it left?
    Status { session: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailboxResponse {
    pub ok: bool,
    /// Base64 flight bytes (for `get`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<String>,
    /// Milliseconds left before expiry (for `status`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ttl_ms: Option<u64>,
    /// Why the request was refused (for any failure).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl MailboxResponse {
    #[must_use]
    pub fn ok() -> Self {
        Self {
            ok: true,
            payload: None,
            ttl_ms: None,
            error: None,
        }
    }

    #[must_use]
    pub fn with_payload(payload: Option<String>) -> Self {
        Self {
            ok: true,
            payload,
            ttl_ms: None,
            error: None,
        }
    }

    #[must_use]
    pub fn with_ttl(ttl_ms: u64) -> Self {
        Self {
            ok: true,
            payload: None,
            ttl_ms: Some(ttl_ms),
            error: None,
        }
    }

    #[must_use]
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            payload: None,
            ttl_ms: None,
            error: Some(message.into()),
        }
    }
}

/// Encode a flight for the wire.
#[must_use]
pub fn encode_payload(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decode a flight from the wire. Length is not limited here — the relay caps
/// frame size, the client trusts what it asked for.
pub fn decode_payload(text: &str) -> Result<Vec<u8>, PairingError> {
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .map_err(|e| PairingError::Mailbox(format!("malformed payload: {e}")))
}

/// Blocking mailbox client. One connection per operation (ADR 0007 style), so
/// a slow or dead mailbox cannot leave the pairing holding a stale socket.
pub struct MailboxClient {
    addr: MailboxAddr,
}

impl MailboxClient {
    #[must_use]
    pub fn new(addr: MailboxAddr) -> Self {
        Self { addr }
    }

    #[must_use]
    pub fn addr(&self) -> &MailboxAddr {
        &self.addr
    }

    /// Open a session, or fail because the id is taken/burned.
    pub fn open(&self, session: &str, ttl: Duration) -> Result<(), PairingError> {
        self.request(&MailboxRequest::Open {
            session: session.to_string(),
            ttl_secs: ttl.as_secs().max(1),
        })
        .map(|_| ())
    }

    /// Publish a flight into a slot.
    pub fn put(&self, session: &str, slot: Slot, payload: &[u8]) -> Result<(), PairingError> {
        self.request(&MailboxRequest::Put {
            session: session.to_string(),
            slot,
            payload: encode_payload(payload),
        })
        .map(|_| ())
    }

    /// Read a flight, or `None` while the peer has not published it yet.
    pub fn get(&self, session: &str, slot: Slot) -> Result<Option<Vec<u8>>, PairingError> {
        let response = self.request(&MailboxRequest::Get {
            session: session.to_string(),
            slot,
        })?;
        match response.payload {
            Some(text) => decode_payload(&text).map(Some),
            None => Ok(None),
        }
    }

    /// Close the session and refuse the id forever.
    pub fn burn(&self, session: &str) -> Result<(), PairingError> {
        self.request(&MailboxRequest::Burn {
            session: session.to_string(),
        })
        .map(|_| ())
    }

    /// Milliseconds left on the session (the relay is the clock of record for
    /// expiry, so both parties agree on when the window closes).
    pub fn ttl_ms(&self, session: &str) -> Result<u64, PairingError> {
        let response = self.request(&MailboxRequest::Status {
            session: session.to_string(),
        })?;
        response
            .ttl_ms
            .ok_or_else(|| PairingError::Mailbox("status reply carried no ttl".into()))
    }

    /// Poll a slot until it holds a flight or `deadline` passes. The poll
    /// interval is short (pairing is interactive) and the deadline is the
    /// caller's TTL, so a wrong code cannot extend the window.
    pub fn wait_for(
        &self,
        session: &str,
        slot: Slot,
        deadline: std::time::Instant,
        poll: Duration,
    ) -> Result<Vec<u8>, PairingError> {
        loop {
            if let Some(payload) = self.get(session, slot)? {
                return Ok(payload);
            }
            if std::time::Instant::now() >= deadline {
                return Err(PairingError::Timeout { waiting_for: slot });
            }
            std::thread::sleep(poll.min(Duration::from_millis(200)));
        }
    }

    /// One request → one response, on a fresh connection.
    fn request(&self, request: &MailboxRequest) -> Result<MailboxResponse, PairingError> {
        let line = frame(request)?;
        let response_line = match &self.addr {
            #[cfg(unix)]
            MailboxAddr::Unix(path) => {
                let stream = UnixStream::connect(path).map_err(|e| {
                    PairingError::Mailbox(format!(
                        "cannot reach the pairing mailbox at {}: {e}",
                        path.display()
                    ))
                })?;
                stream
                    .set_read_timeout(Some(MAILBOX_TIMEOUT))
                    .and_then(|()| stream.set_write_timeout(Some(MAILBOX_TIMEOUT)))
                    .map_err(|e| PairingError::Mailbox(e.to_string()))?;
                exchange(stream, &line)?
            }
            MailboxAddr::Tcp(host) => {
                let stream = TcpStream::connect(host.as_str()).map_err(|e| {
                    PairingError::Mailbox(format!(
                        "cannot reach the pairing mailbox at {host}: {e}"
                    ))
                })?;
                stream
                    .set_read_timeout(Some(MAILBOX_TIMEOUT))
                    .and_then(|()| stream.set_write_timeout(Some(MAILBOX_TIMEOUT)))
                    .map_err(|e| PairingError::Mailbox(e.to_string()))?;
                exchange(stream, &line)?
            }
        };
        let response: MailboxResponse = serde_json::from_str(&response_line)
            .map_err(|e| PairingError::Mailbox(format!("malformed mailbox reply: {e}")))?;
        if response.ok {
            Ok(response)
        } else {
            Err(PairingError::Mailbox(
                response
                    .error
                    .unwrap_or_else(|| "refused without a reason".to_string()),
            ))
        }
    }
}

/// Serialize one request frame (no trailing newline).
pub fn frame(request: &MailboxRequest) -> Result<String, PairingError> {
    serde_json::to_string(request)
        .map_err(|e| PairingError::Mailbox(format!("cannot encode the request: {e}")))
}

/// Write a frame, read the reply line. Shared by both address kinds.
fn exchange<S>(stream: S, line: &str) -> Result<String, PairingError>
where
    S: std::io::Read + Write,
{
    let mut stream = stream;
    stream
        .write_all(line.as_bytes())
        .and_then(|()| stream.write_all(b"\n"))
        .and_then(|()| stream.flush())
        .map_err(|e| PairingError::Mailbox(format!("cannot send to the mailbox: {e}")))?;
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    let read = reader
        .read_line(&mut reply)
        .map_err(|e| PairingError::Mailbox(format!("cannot read from the mailbox: {e}")))?;
    if read == 0 {
        return Err(PairingError::Mailbox(
            "the mailbox closed without answering".to_string(),
        ));
    }
    Ok(reply.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_parse_paths_and_hostports_only() {
        // A filesystem path is a unix-socket address where unix sockets exist.
        #[cfg(unix)]
        {
            assert_eq!(
                MailboxAddr::parse("/tmp/relay.sock").expect("path"),
                MailboxAddr::Unix(PathBuf::from("/tmp/relay.sock"))
            );
            assert_eq!(
                MailboxAddr::parse("./relay.sock").expect("relative path"),
                MailboxAddr::Unix(PathBuf::from("./relay.sock"))
            );
        }
        // Elsewhere they are refused *with a hint*, rather than turned into a
        // connection attempt that cannot work.
        #[cfg(not(unix))]
        for path in ["/tmp/relay.sock", "./relay.sock"] {
            match MailboxAddr::parse(path) {
                Err(PairingError::BadInvite(message)) => {
                    assert!(message.contains("host:port"), "{message}");
                }
                other => panic!("{path:?} was accepted on a non-unix build: {other:?}"),
            }
        }
        assert_eq!(
            MailboxAddr::parse("relay.example.com:8443").expect("hostport"),
            MailboxAddr::Tcp("relay.example.com:8443".to_string())
        );
        // A host with no port would silently target the wrong service.
        for bad in [
            "",
            "relay.example.com",
            "relay.example.com:",
            ":8443",
            "not a path",
        ] {
            assert!(
                matches!(MailboxAddr::parse(bad), Err(PairingError::BadInvite(_))),
                "{bad:?} was accepted"
            );
        }
    }

    #[test]
    fn a_request_round_trips_through_one_line_of_json() {
        let request = MailboxRequest::Put {
            session: "abc".into(),
            slot: Slot::B,
            payload: encode_payload(&[0u8, 1, 2, 255]),
        };
        let line = frame(&request).expect("encodes");
        assert!(!line.contains('\n'), "a frame must be one line: {line}");
        let decoded: MailboxRequest = serde_json::from_str(&line).expect("decodes");
        match decoded {
            MailboxRequest::Put { slot, payload, .. } => {
                assert_eq!(slot, Slot::B);
                assert_eq!(
                    decode_payload(&payload).expect("base64"),
                    vec![0, 1, 2, 255]
                );
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn a_malformed_payload_is_a_typed_error() {
        assert!(matches!(
            decode_payload("not base64!!"),
            Err(PairingError::Mailbox(_))
        ));
        // And a response that says no carries its reason through.
        let response = MailboxResponse::error("session already used");
        assert!(!response.ok);
        assert_eq!(response.error.as_deref(), Some("session already used"));
    }
}
