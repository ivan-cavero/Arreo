//! Socket client (T-0015): framed MessagePack over the daemon socket.
//!
//! One connection per call (the CLI pattern): Hello→Welcome handshake, one
//! verb, read the answer(s). The TUI keeps one long-lived attach per focused
//! pane plus per-tick polls for the sidebar (panes + metrics).

use arreo_core::proto::codec;
use arreo_core::proto::{AgentState, Message, VERSION};
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("tui io: {0}")]
    Io(#[from] std::io::Error),
    #[error("tui codec: {0}")]
    Codec(String),
    #[error("tui daemon: {0}")]
    Daemon(String),
    #[error("tui handshake: {0}")]
    Handshake(String),
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

pub struct Client {
    reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    buf: Vec<u8>,
}

impl Client {
    pub async fn connect(socket: &Path) -> Result<Self, ClientError> {
        let stream = tokio::net::UnixStream::connect(socket)
            .await
            .map_err(|_| ClientError::Handshake(format!("no daemon at {}", socket.display())))?;
        let (reader, writer) = stream.into_split();
        let mut conn = Self {
            reader: tokio::io::BufReader::new(reader),
            writer,
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
        self.writer.write_all(&frame).await?;
        self.writer.flush().await?;
        Ok(())
    }

    pub async fn recv(&mut self) -> Result<Message, ClientError> {
        loop {
            if let Ok((message, consumed)) = codec::decode_frame(&self.buf) {
                self.buf.drain(..consumed);
                return Ok(message);
            }
            let mut chunk = [0u8; 8192];
            let n = self.reader.read(&mut chunk).await?;
            if n == 0 {
                return Err(ClientError::Daemon("server closed connection".to_string()));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// One-shot request/response.
    pub async fn request(socket: &Path, message: &Message) -> Result<Message, ClientError> {
        let mut conn = Self::connect(socket).await?;
        conn.send(message).await?;
        conn.recv().await
    }
}

/// Pane summary for the sidebar (id + liveness + state + RAM).
#[derive(Debug, Clone)]
pub struct PaneSummary {
    pub id: String,
    pub alive: bool,
    pub state: AgentState,
    pub ram_kb: u64,
}
