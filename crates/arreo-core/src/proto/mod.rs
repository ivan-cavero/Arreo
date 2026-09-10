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
//! - `negotiate(server, client_wants)`: reject-only-for-incompatible in v0
//!   (exact match required); N−1 window logic lands with v1.

pub mod codec;
pub mod message;

pub use codec::negotiate;
pub use message::{AgentState, Message, PaneInfo, VERSION};
