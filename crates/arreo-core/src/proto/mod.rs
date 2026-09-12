//! v0 wire protocol, take 2 (T-0013): versioned MessagePack messages.
//!
//! One sentence: every surface speaks length-delimited MessagePack frames of
//! one `Message` enum, versioned from the first commit so N−1 compat is
//! possible later.
//!
//! Framing (ROADMAP §3.2): `u32 LE length + msgpack bytes`. The length prefix
//! keeps decode boundaries exact over stream sockets (QUIC in Phase 2, Unix
//! socket now) — no JSONL newline-scanning, no ambiguity with binary payloads.
//!
//! Compatibility story:
//! - T-0014 cut the daemon over: JSONL is gone, one framing (MessagePack).
//!   The compat types were deleted with it — no dual-stack to maintain.
//! - `negotiate(server, client_wants)`: the N−1 window (T-0028, ADR 0017) —
//!   accept the highest version common to `[server-1, server]`, echo it in
//!   `Welcome.v`, refuse anything outside with a typed error naming the range.
//! - `client_versions()`: the other half of that window, and the **one** place
//!   a client's offer is written. The server's fallback only exists for a
//!   version the client offered, so a client announcing `[VERSION]` alone is
//!   refused by every older daemon — the forward direction §3.13 promises.

pub mod codec;
pub mod message;

pub use codec::{
    classify_op, client_versions, client_versions_from, decode_op_for_error, frame_body_len,
    negotiate, CodecError, Direction, MAX_FRAME_BYTES, MIN_VERSION,
};
pub use message::{AgentState, Message, MetricsPoint, PaneInfo, VERSION};
