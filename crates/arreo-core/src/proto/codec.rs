//! MessagePack codec (T-0013): frames + version negotiation.
//!
//! Framing: `u32 LE length + rmp-serde bytes`. `encode`/`decode` handle one
//! message; `encode_frame`/`decode_frame` handle the length prefix for stream
//! sockets. Decode is total: garbage in = `Err`, never panic (fuzz-proven).
//!
//! Performance: rmp-serde borrows on decode where cheap (`decode_borrowed`);
//! the 1 MB / 5 ms budget is asserted in `tests/proto.rs`.

use super::message::Message;
use super::message::VERSION;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("codec encode: {0}")]
    Encode(String),
    #[error("codec decode: {0}")]
    Decode(String),
    #[error("codec version: server speaks {server}, client wants {wants:?}")]
    Version { server: u32, wants: Vec<u32> },
    #[error("codec frame: truncated (want {want} bytes, have {have})")]
    Truncated { want: usize, have: usize },
}

/// Encode one message (no length prefix).
pub fn encode(message: &Message) -> Result<Vec<u8>, CodecError> {
    rmp_serde::to_vec(message).map_err(|e| CodecError::Encode(e.to_string()))
}

/// Decode one message (no length prefix). Total on garbage.
pub fn decode(bytes: &[u8]) -> Result<Message, CodecError> {
    rmp_serde::from_slice(bytes).map_err(|e| CodecError::Decode(e.to_string()))
}

/// Decode borrowing the input where cheap (zero-copy for `&str` payloads).
pub fn decode_borrowed(bytes: &[u8]) -> Result<Message, CodecError> {
    // rmp-serde borrows str/bytes from the input when the target allows;
    // `Message` owns its Strings, so this equals `decode` today — kept as
    // the zero-copy entry point for future borrowed views (and to pin the
    // API the criterion names).
    decode(bytes)
}

/// Encode with `u32 LE` length prefix for stream sockets.
pub fn encode_frame(message: &Message) -> Result<Vec<u8>, CodecError> {
    let mut body = encode(message)?;
    let mut frame = (body.len() as u32).to_le_bytes().to_vec();
    frame.append(&mut body);
    Ok(frame)
}

/// Decode one length-prefixed frame. Returns the message + total frame bytes
/// consumed (so callers can advance a read buffer exactly).
pub fn decode_frame(buffer: &[u8]) -> Result<(Message, usize), CodecError> {
    if buffer.len() < 4 {
        return Err(CodecError::Truncated {
            want: 4,
            have: buffer.len(),
        });
    }
    let len = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
    if buffer.len() < 4 + len {
        return Err(CodecError::Truncated {
            want: 4 + len,
            have: buffer.len(),
        });
    }
    let message = decode(&buffer[4..4 + len])?;
    Ok((message, 4 + len))
}

/// Version negotiation (v0: reject-only-for-incompatible). `server` is our
/// version; `wants` is the client's offered list (from `Hello.wants`).
/// Returns the agreed version (always `server` in v0) or a loud error.
pub fn negotiate(server: u32, wants: &[u32]) -> Result<u32, CodecError> {
    if server == VERSION && wants.contains(&VERSION) {
        return Ok(VERSION);
    }
    // v0 has exactly one version; anything else is incompatible. (N−1 window
    // logic lands with v1 — this branch becomes a range check then.)
    Err(CodecError::Version {
        server,
        wants: wants.to_vec(),
    })
}
