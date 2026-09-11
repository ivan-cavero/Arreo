//! The relay client (T-0029): dial the relay, prove who you are, route bytes.
//!
//! Apache-licensed on purpose (ROADMAP §7): this is the reference client for the
//! protocol in [`super`], so a third party can build against a self-hosted relay
//! without linking AGPL code, and the daemon (T-0050) gets a client without
//! depending on the relay crate.
//!
//! One sentence: connect over QUIC, complete the nonce handshake, then exchange
//! [`RelayEnvelope`]s whose payloads this side also never inspects.

use super::{
    decode_message, decode_payload, encode_message, encode_payload, proof_payload, read_envelope,
    read_frame, write_frame, Ack, Auth, AuthReply, ClientError, DrainReport, DrainRequest, Hello,
    HelloReply, Outcome, RelayEnvelope, RelayError, RelayHeader, RelayKind, MAX_HANDSHAKE_BYTES,
    RELAY_VERSION,
};
use crate::identity::{DeviceCert, DeviceId, DeviceKey};
use crate::transport::{client_endpoint, SERVER_NAME};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

/// How long a handshake may take before the attempt is abandoned.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// What arrived from the relay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incoming {
    /// An envelope for this device.
    Envelope(RelayEnvelope),
    /// The relay's report on one envelope this device sent.
    Status { seq: u64, outcome: Outcome },
    /// The relay's report on a drain (T-0030).
    Drain(DrainReport),
}

/// A live session with the relay.
pub struct RelayClient {
    io: tokio::io::Join<quinn::RecvStream, quinn::SendStream>,
    account_id: String,
    device_id: DeviceId,
    buf: Vec<u8>,
    next_seq: u64,
}

impl std::fmt::Debug for RelayClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayClient")
            .field("account_id", &self.account_id)
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

impl RelayClient {
    /// Dial `addr` and complete the handshake as `device`.
    ///
    /// The QUIC endpoint is created here and lives with the client. A daemon
    /// that multiplexes several sessions will want one endpoint shared across
    /// them (T-0050); a single session per client is what v1 needs and what
    /// keeps the ownership story obvious.
    pub async fn connect(
        addr: SocketAddr,
        account_id: &str,
        device: &DeviceKey,
        cert: &DeviceCert,
    ) -> Result<Self, ClientError> {
        let endpoint = client_endpoint().map_err(|e| ClientError::Transport(e.to_string()))?;
        let connecting = endpoint
            .connect(addr, SERVER_NAME)
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let connection = tokio::time::timeout(HANDSHAKE_TIMEOUT, connecting)
            .await
            .map_err(|_| ClientError::Timeout)?
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let (send, recv) = connection
            .open_bi()
            .await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        let mut io = tokio::io::join(recv, send);

        let device_id = cert.device().clone();
        let handshake = Self::handshake(&mut io, account_id, device, cert, &device_id);
        let (account_id, device_id) = match tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake).await
        {
            Ok(result) => result?,
            Err(_) => return Err(ClientError::Timeout),
        };
        Ok(Self {
            io,
            account_id,
            device_id,
            buf: Vec::new(),
            next_seq: 1,
        })
    }

    /// Hello → Challenge → Auth → Welcome.
    async fn handshake<S>(
        io: &mut S,
        account_id: &str,
        device: &DeviceKey,
        cert: &DeviceCert,
        device_id: &DeviceId,
    ) -> Result<(String, DeviceId), ClientError>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let hello = Hello {
            v: RELAY_VERSION,
            account_id: account_id.to_string(),
            device_id: device_id.display_id(),
        };
        write_frame(io, &encode_message(&hello)?).await?;

        let mut buf = Vec::new();
        let body = read_frame(io, &mut buf, MAX_HANDSHAKE_BYTES).await?;
        let nonce = match decode_message::<HelloReply>(&body)? {
            HelloReply::Challenge { v, nonce } => {
                if v != RELAY_VERSION {
                    return Err(ClientError::Protocol(RelayError::Version { found: v }));
                }
                nonce
            }
            HelloReply::Refused { reason, .. } => return Err(ClientError::Refused { reason }),
        };

        // The canonical id, not the announced spelling: the relay checks the
        // proof against the certificate's id, so both sides must agree on one
        // form regardless of how the announcement was written.
        let signature = device.sign(&proof_payload(&nonce, account_id, device_id.as_str()));
        let auth = Auth {
            v: RELAY_VERSION,
            cert: cert
                .encode()
                .map_err(|e| ClientError::Protocol(RelayError::Frame(e.to_string())))?,
            signature: signature.to_bytes().to_vec(),
        };
        write_frame(io, &encode_message(&auth)?).await?;

        let body = read_frame(io, &mut buf, MAX_HANDSHAKE_BYTES).await?;
        match decode_message::<AuthReply>(&body)? {
            AuthReply::Welcome {
                v,
                account_id,
                device_id,
            } => {
                if v != RELAY_VERSION {
                    return Err(ClientError::Protocol(RelayError::Version { found: v }));
                }
                let parsed = DeviceId::parse(&device_id)
                    .map_err(|e| ClientError::Protocol(RelayError::Cert(e.to_string())))?;
                Ok((account_id, parsed))
            }
            AuthReply::Refused { reason, .. } => Err(ClientError::Refused { reason }),
        }
    }

    #[must_use]
    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    #[must_use]
    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    /// Send one payload to `dst`, returning the sequence number the relay will
    /// report on.
    pub async fn send(&mut self, dst: &DeviceId, payload: &[u8]) -> Result<u64, ClientError> {
        let seq = self.next_seq;
        self.next_seq += 1;
        let envelope = RelayEnvelope {
            header: RelayHeader {
                v: RELAY_VERSION,
                account_id: self.account_id.clone(),
                src_device: self.device_id.display_id(),
                dst: dst.display_id(),
                seq,
                kind: RelayKind::Frame,
            },
            payload: payload.to_vec(),
        };
        write_frame(&mut self.io, &envelope.encode()?).await?;
        Ok(seq)
    }

    /// Ask the relay to drain this device's durable inbox from `from_seq`.
    ///
    /// The messages arrive as ordinary envelopes (so the caller's existing
    /// handling works), followed by an [`Incoming::Drain`] report carrying the
    /// per-drain counts. Draining does *not* remove anything: only [`Self::ack`]
    /// does, which is what makes a disconnect mid-drain safe.
    pub async fn drain(&mut self, from_seq: u64) -> Result<(), ClientError> {
        self.control(
            RelayKind::Drain,
            &DrainRequest {
                v: RELAY_VERSION,
                from_seq,
            },
        )
        .await
    }

    /// Acknowledge everything up to and including `seq`, removing those rows.
    pub async fn ack(&mut self, seq: u64) -> Result<(), ClientError> {
        self.control(
            RelayKind::Ack,
            &Ack {
                v: RELAY_VERSION,
                seq,
            },
        )
        .await
    }

    /// Send one control message (a drain or an ack) as a frame envelope.
    async fn control<T: serde::Serialize>(
        &mut self,
        kind: RelayKind,
        payload: &T,
    ) -> Result<(), ClientError> {
        let seq = self.next_seq;
        self.next_seq += 1;
        let envelope = RelayEnvelope {
            header: RelayHeader {
                v: RELAY_VERSION,
                account_id: self.account_id.clone(),
                src_device: self.device_id.display_id(),
                dst: self.device_id.display_id(),
                seq,
                kind,
            },
            payload: encode_payload(payload)?,
        };
        write_frame(&mut self.io, &envelope.encode()?).await?;
        Ok(())
    }

    /// Drain and collect: the request, the messages, and the report.
    ///
    /// Convenient for a caller that wants the batch, and honest about the shape
    /// — the report is returned even when it is all zeroes, because "nothing was
    /// dropped" is information.
    pub async fn drain_all(
        &mut self,
        from_seq: u64,
    ) -> Result<(Vec<RelayEnvelope>, DrainReport), ClientError> {
        self.drain(from_seq).await?;
        let mut messages = Vec::new();
        loop {
            match self.next().await? {
                Incoming::Envelope(envelope) => messages.push(envelope),
                Incoming::Drain(report) => return Ok((messages, report)),
                Incoming::Status { .. } => continue,
            }
        }
    }

    /// Read the next message: an envelope for this device, or the relay's report
    /// on something this device sent.
    ///
    /// One framing, one decode, and the branch is the header's `kind` — so
    /// there is no window in which the client is guessing which shape arrived.
    pub async fn next(&mut self) -> Result<Incoming, ClientError> {
        let envelope = read_envelope(&mut self.io, &mut self.buf).await?;
        match envelope.header.kind {
            RelayKind::Frame => Ok(Incoming::Envelope(envelope)),
            RelayKind::Status => {
                // A status payload is either a per-envelope outcome or a drain
                // report; the drain report has a different field count, so try
                // the shape the caller is in the middle of before the other.
                if let Ok(report) = decode_payload::<DrainReport>(&envelope.payload) {
                    if report.v == RELAY_VERSION && report.next_seq != 0 {
                        return Ok(Incoming::Drain(report));
                    }
                }
                let outcome: Outcome = decode_payload(&envelope.payload)?;
                Ok(Incoming::Status {
                    seq: envelope.header.seq,
                    outcome,
                })
            }
            // The relay never originates these, and a device reading them would
            // mean the relay echoed a request back.
            RelayKind::Drain | RelayKind::Ack => Err(ClientError::Protocol(RelayError::Frame(
                format!("the relay sent a {:?} envelope", envelope.header.kind),
            ))),
        }
    }

    /// Read until one envelope arrives, skipping the delivery reports of what
    /// this device sent. Convenient for a request/response exchange.
    pub async fn recv_envelope(&mut self) -> Result<RelayEnvelope, ClientError> {
        loop {
            match self.next().await? {
                Incoming::Envelope(envelope) => return Ok(envelope),
                Incoming::Status { .. } | Incoming::Drain(_) => continue,
            }
        }
    }
}
