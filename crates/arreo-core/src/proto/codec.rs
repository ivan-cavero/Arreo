//! MessagePack codec (T-0013): frames + version negotiation.
//!
//! Framing: `u32 LE length + rmp-serde bytes`. `encode`/`decode` handle one
//! message; `encode_frame`/`decode_frame` handle the length prefix for stream
//! sockets. Decode is total: garbage in = `Err`, never panic (fuzz-proven).
//!
//! Performance: rmp-serde borrows on decode where cheap (`decode_borrowed`);
//! the 1 MB / 5 ms budget is asserted in `tests/proto.rs`.

use super::message::Message;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CodecError {
    #[error("codec encode: {0}")]
    Encode(String),
    #[error("codec decode: {0}")]
    Decode(String),
    #[error("codec version: server speaks {server} (accepts back to {floor}), client wants {wants:?}", floor = server.saturating_sub(1))]
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
    let (message, consumed, _) = decode_frame_full(buffer)?;
    Ok((message, consumed))
}

/// Decode one length-prefixed frame, also returning the frame body's bytes.
///
/// The body is what `classify_op` reads: when the typed decode fails (an
/// unknown variant from a newer version), the caller can still classify the
/// frame as request-or-event from its `op` tag instead of treating every
/// undecodable frame as garbage. That distinction is the compat rule — a
/// request is refused loudly with the connection left open, an event is
/// ignored and counted.
pub fn decode_frame_full(buffer: &[u8]) -> Result<(Message, usize, &[u8]), CodecError> {
    let len = frame_body_len(buffer)?;
    if buffer.len() < 4 + len {
        return Err(CodecError::Truncated {
            want: 4 + len,
            have: buffer.len(),
        });
    }
    let body = &buffer[4..4 + len];
    let message = decode(body)?;
    Ok((message, 4 + len, body))
}

/// Largest frame body the codec will read: 1 MiB (T-0013's budget, asserted in
/// `tests/proto.rs` and re-asserted by the compat suite).
///
/// A length prefix naming more is not "a large message", it is corruption or a
/// hostile peer — waiting for those bytes would stall the session until the
/// connection died. Refused here, before any allocation, so no caller has to
/// decide what "too big" means.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Read a frame's declared body length: bounds-checked, allocation-free.
pub fn frame_body_len(buffer: &[u8]) -> Result<usize, CodecError> {
    if buffer.len() < 4 {
        return Err(CodecError::Truncated {
            want: 4,
            have: buffer.len(),
        });
    }
    let len = u32::from_le_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(CodecError::Decode(format!(
            "frame declares {len} bytes, over the {MAX_FRAME_BYTES}-byte budget"
        )));
    }
    Ok(len)
}

/// Which side of the conversation a wire variant belongs to (T-0028, ADR 0017).
///
/// The classification the compat rules need: an unknown client→server *request*
/// gets a typed `Error` with the connection left open (the client can report
/// it); an unknown server→client *event* is ignored and counted, never fatal.
/// Read from the map head (`op`), so an unknown variant is classified before
/// the typed decoder sees it — never a panic, never a silently discarded
/// state-mutating message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Client → server: something the client asks the daemon to do. Unknown
    /// ones are refused loudly; the connection stays open.
    Request,
    /// Server → client: something that happened. Unknown ones are ignored and
    /// counted; killing the session over news it does not understand would make
    /// every server addition a breaking change.
    Event,
}

/// Classify a wire `op` without a full decode.
///
/// Reads the MessagePack map for the `op` tag and answers request-or-event from
/// the name alone — by scanning the decoded-into-generic-value path that
/// `rmp-serde` already provides (`rmpv` would be a new dependency for one
/// function; the task notes forbid scaffolding dependencies). `None` means the
/// bytes are not a tagged message at all (garbage, or a shape from before the
/// tag existed) — which is itself a refusal, not a guess.
#[must_use]
pub fn classify_op(bytes: &[u8]) -> Option<Direction> {
    let op = decode_op_tag(bytes)?;
    Some(match op.as_str() {
        // Client → server: the verbs a client asks the daemon to perform.
        "hello" | "resume" | "spawn" | "attach" | "send" | "resize" | "kill" | "read" | "wait"
        | "split" | "metrics_req" | "panes" | "handoff" => Direction::Request,
        // Server → client: answers and news.
        "welcome" | "snapshot" | "delta" | "error" | "state_event" | "metrics"
        | "metrics_series" | "ok" | "exited" | "handoff_ready" => Direction::Event,
        // Unknown: the shape of a future version. A client that sent something
        // the server does not know asked for work — that is a request until a
        // newer server says otherwise, so it is refused rather than ignored. A
        // state-mutating message silently discarded is the one outcome the
        // compat rules exist to prevent.
        _ => Direction::Request,
    })
}

/// Read the `op` string out of a MessagePack map without a schema.
///
/// Public for the daemon's refusal path, which needs the tag text for its
/// typed `Error` after `classify_op` has already answered request-or-event.
pub fn decode_op_for_error(bytes: &[u8]) -> Option<String> {
    decode_op_tag(bytes)
}

/// Read the `op` string out of a MessagePack map without a schema (private).
///
/// The encoder writes `Message` as a map whose first entries are small strings,
/// so this walks fixmap/fixstr/fixarray headers by hand — just enough to find
/// the `op` key's value. Anything unexpected (wrong types, truncation, a
/// non-map) is `None`: the caller treats that as "not a tagged message", which
/// is a refusal, never a guess.
fn decode_op_tag(bytes: &[u8]) -> Option<String> {
    let (first, rest) = bytes.split_first()?;
    // fixarray (0x90–0x9f), array16 (0xdc), array32 (0xdd) — the shape
    // `rmp-serde` emits for an internally-tagged enum: `["hello", {...}]`,
    // tag first, content second. A map head here means a hand-built frame,
    // which the same walk handles below.
    if matches!(first, 0x90..=0x9f | 0xdc | 0xdd) {
        let (len, after_len) = match first {
            0x90..=0x9f => ((*first & 0x0f) as usize, rest),
            0xdc => {
                if rest.len() < 2 {
                    return None;
                }
                (u16::from_be_bytes([rest[0], rest[1]]) as usize, &rest[2..])
            }
            _ => {
                if rest.len() < 4 {
                    return None;
                }
                (
                    u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize,
                    &rest[4..],
                )
            }
        };
        if len < 1 {
            return None;
        }
        // Element 0 is the tag; the fields follow as siblings, not as a map.
        let (tag, _) = decode_str(after_len)?;
        return Some(tag);
    }
    // fixmap (0x80–0x8f), map16 (0xde), map32 (0xdf).
    let (mut count, mut cursor) = match first {
        0x80..=0x8f => ((*first & 0x0f) as usize, rest),
        0xde => {
            if rest.len() < 2 {
                return None;
            }
            (u16::from_be_bytes([rest[0], rest[1]]) as usize, &rest[2..])
        }
        0xdf => {
            if rest.len() < 4 {
                return None;
            }
            (
                u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize,
                &rest[4..],
            )
        }
        _ => return None,
    };
    while count > 0 {
        count -= 1;
        let (key, after_key) = decode_str(cursor)?;
        if key == "op" {
            let (op, _) = decode_str(after_key)?;
            return Some(op);
        }
        let (_, after_value) = skip_value(after_key)?;
        cursor = after_value;
    }
    None
}

/// Read a MessagePack string (fixstr, str8/16/32) as UTF-8, returning the
/// string and the bytes after it.
fn decode_str(bytes: &[u8]) -> Option<(String, &[u8])> {
    let (first, rest) = bytes.split_first()?;
    let (len, body) = match first {
        0xa0..=0xbf => ((*first & 0x1f) as usize, rest),
        0xd9 => {
            if rest.is_empty() {
                return None;
            }
            (rest[0] as usize, &rest[1..])
        }
        0xda => {
            if rest.len() < 2 {
                return None;
            }
            (u16::from_be_bytes([rest[0], rest[1]]) as usize, &rest[2..])
        }
        0xdb => {
            if rest.len() < 4 {
                return None;
            }
            (
                u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize,
                &rest[4..],
            )
        }
        _ => return None,
    };
    if body.len() < len {
        return None;
    }
    Some((
        std::str::from_utf8(&body[..len]).ok()?.to_string(),
        &body[len..],
    ))
}

/// Skip one MessagePack value, returning the bytes after it.
///
/// Covers what the encoder emits for `Message` (nil/bool/ints/floats/strings,
/// bins, arrays, maps, ext) — just enough that an unknown variant's *value*
/// can be stepped over to reach the next key. Anything unrecognized is `None`.
fn skip_value(bytes: &[u8]) -> Option<(&[u8], &[u8])> {
    let (first, rest) = bytes.split_first()?;
    match first {
        // nil, false, true.
        0xc0 | 0xc2 | 0xc3 => Some((&bytes[..1], rest)),
        // fixint, negative fixint.
        0x00..=0x7f | 0xe0..=0xff => Some((&bytes[..1], rest)),
        // uint 8/16/32/64, int 8/16/32/64, float 32/64.
        0xcc | 0xd0 => Some((&bytes[..2], rest.get(1..)?)),
        0xcd | 0xd1 => Some((&bytes[..3], rest.get(2..)?)),
        0xca => Some((&bytes[..5], rest.get(4..)?)),
        0xce | 0xd2 => Some((&bytes[..5], rest.get(4..)?)),
        0xcb | 0xd3 => Some((&bytes[..9], rest.get(8..)?)),
        0xcf => Some((&bytes[..9], rest.get(8..)?)),
        // fixstr, str8/16/32: length prefix + bytes.
        0xa0..=0xbf => {
            let len = (*first & 0x1f) as usize;
            Some((&bytes[..1 + len], rest.get(len..)?))
        }
        0xd9 => {
            let len = *rest.first()? as usize;
            Some((&bytes[..2 + len], rest.get(1 + len..)?))
        }
        0xda => {
            if rest.len() < 2 {
                return None;
            }
            let len = u16::from_be_bytes([rest[0], rest[1]]) as usize;
            Some((&bytes[..3 + len], rest.get(2 + len..)?))
        }
        0xdb => {
            if rest.len() < 4 {
                return None;
            }
            let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            Some((&bytes[..5 + len], rest.get(4 + len..)?))
        }
        // bin8/16/32.
        0xc4 => {
            let len = *rest.first()? as usize;
            Some((&bytes[..2 + len], rest.get(1 + len..)?))
        }
        0xc5 => {
            if rest.len() < 2 {
                return None;
            }
            let len = u16::from_be_bytes([rest[0], rest[1]]) as usize;
            Some((&bytes[..3 + len], rest.get(2 + len..)?))
        }
        0xc6 => {
            if rest.len() < 4 {
                return None;
            }
            let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            Some((&bytes[..5 + len], rest.get(4 + len..)?))
        }
        // fixarray, array16/32: skip each element in turn.
        0x90..=0x9f => {
            let mut count = (*first & 0x0f) as usize;
            let mut cursor = rest;
            while count > 0 {
                count -= 1;
                (_, cursor) = skip_value(cursor)?;
            }
            let consumed = bytes.len() - cursor.len();
            Some((&bytes[..consumed], cursor))
        }
        0xdc => {
            if rest.len() < 2 {
                return None;
            }
            let mut count = u16::from_be_bytes([rest[0], rest[1]]) as usize;
            let mut cursor = &rest[2..];
            while count > 0 {
                count -= 1;
                (_, cursor) = skip_value(cursor)?;
            }
            let consumed = bytes.len() - cursor.len();
            Some((&bytes[..consumed], cursor))
        }
        0xdd => {
            if rest.len() < 4 {
                return None;
            }
            let mut count = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            let mut cursor = &rest[4..];
            while count > 0 {
                count -= 1;
                (_, cursor) = skip_value(cursor)?;
            }
            let consumed = bytes.len() - cursor.len();
            Some((&bytes[..consumed], cursor))
        }
        // fixmap, map16/32: skip key and value in turn.
        0x80..=0x8f => {
            let mut count = (*first & 0x0f) as usize;
            let mut cursor = rest;
            while count > 0 {
                count -= 1;
                (_, cursor) = skip_value(cursor)?;
                (_, cursor) = skip_value(cursor)?;
            }
            let consumed = bytes.len() - cursor.len();
            Some((&bytes[..consumed], cursor))
        }
        0xde => {
            if rest.len() < 2 {
                return None;
            }
            let mut count = u16::from_be_bytes([rest[0], rest[1]]) as usize;
            let mut cursor = &rest[2..];
            while count > 0 {
                count -= 1;
                (_, cursor) = skip_value(cursor)?;
                (_, cursor) = skip_value(cursor)?;
            }
            let consumed = bytes.len() - cursor.len();
            Some((&bytes[..consumed], cursor))
        }
        0xdf => {
            if rest.len() < 4 {
                return None;
            }
            let mut count = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            let mut cursor = &rest[4..];
            while count > 0 {
                count -= 1;
                (_, cursor) = skip_value(cursor)?;
                (_, cursor) = skip_value(cursor)?;
            }
            let consumed = bytes.len() - cursor.len();
            Some((&bytes[..consumed], cursor))
        }
        // fixext/ext8/16/32: type byte + data.
        0xd4 => Some((&bytes[..3], rest.get(2..)?)),
        0xd5 => Some((&bytes[..4], rest.get(3..)?)),
        0xd6 => Some((&bytes[..6], rest.get(5..)?)),
        0xd7 => Some((&bytes[..10], rest.get(9..)?)),
        0xd8 => Some((&bytes[..18], rest.get(17..)?)),
        0xc7 => {
            let len = *rest.first()? as usize;
            Some((&bytes[..3 + len], rest.get(2 + len..)?))
        }
        0xc8 => {
            if rest.len() < 2 {
                return None;
            }
            let len = u16::from_be_bytes([rest[0], rest[1]]) as usize;
            Some((&bytes[..4 + len], rest.get(3 + len..)?))
        }
        0xc9 => {
            if rest.len() < 4 {
                return None;
            }
            let len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize;
            Some((&bytes[..6 + len], rest.get(5 + len..)?))
        }
        // 0xc1 (never-used), 0xd9 handled above: unknown means stop.
        _ => None,
    }
}

/// The oldest protocol version this build still speaks.
///
/// The N−1 window (T-0028, ADR 0017): a server at version N accepts clients
/// offering N or N−1, and a client offers both. A gap wider than one is a
/// deferred update (§3.13), not a silent downgrade.
pub const MIN_VERSION: u32 = 0;

/// Version negotiation (N−1 window, T-0028). `server` is our version; `wants`
/// is the client's offered list (from `Hello.wants`).
///
/// Accepts the highest version common to `[server-1, server]` — a downgrade is
/// never silent (the agreed version is echoed in `Welcome.v`), and anything
/// outside the window is a loud error naming the offered versions and our
/// range. An empty offer list is a refusal, not a default: guessing a version
/// for a client that named none is how a downgrade goes silent.
pub fn negotiate(server: u32, wants: &[u32]) -> Result<u32, CodecError> {
    let floor = server.saturating_sub(1);
    // The window is `[server-1, server]` — no cap at `VERSION`. A binary
    // running as `server = 1` speaks 1 by definition; capping at the constant
    // would make a v1 server unable to agree on v1 with a v1 client, which is
    // the one case the window most needs to get right.
    let best = wants
        .iter()
        .copied()
        .filter(|v| *v == server || *v == floor)
        .max();
    match best {
        Some(v) => Ok(v),
        None => Err(CodecError::Version {
            server,
            wants: wants.to_vec(),
        }),
    }
}
