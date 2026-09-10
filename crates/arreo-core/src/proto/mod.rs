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
//! - JSONL (`Request`/`Response` below) is still spoken by the daemon/CLI
//!   from T-0005 — kept working, tested, and NOT broken by this task.
//! - New surfaces SHOULD use `Message` + `codec`. The daemon gains a
//!   MessagePack port when T-0014 wires the socket API v1 (or later) —
//!   T-0013 delivers the codec + schema + negotiation, not the cutover.
//! - `negotiate(server, client_wants)`: reject-only-for-incompatible in v0
//!   (exact match required); N−1 window logic lands with v1.

pub mod codec;
pub mod message;

pub use codec::negotiate;
pub use message::{AgentState, Message, PaneInfo, VERSION};

// --- T-0005 JSONL compat: old names keep working for the daemon/CLI ---
// `Request`, `Response`, `PaneInfo` (singular, T-0005 shapes) still resolve
// for existing consumers; new code uses `Message`. When T-0014 cuts the
// daemon over, these aliases go away.
pub type Request = CompatRequest;
pub type Response = CompatResponse;

// --- T-0005 JSONL compat (unchanged, still the daemon's wire format) ---

use serde::{Deserialize, Serialize};

/// Client → daemon. One JSON object per line (`\n` terminated).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CompatRequest {
    Spawn {
        v: u32,
        id: String,
        program: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default = "default_cols")]
        cols: u16,
        #[serde(default = "default_rows")]
        rows: u16,
    },
    List {
        v: u32,
    },
    Attach {
        v: u32,
        id: String,
        #[serde(default)]
        from_line: usize,
    },
    Send {
        v: u32,
        id: String,
        data: String,
    },
    Resize {
        v: u32,
        id: String,
        cols: u16,
        rows: u16,
    },
    Kill {
        v: u32,
        id: String,
    },
}

fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}

/// Daemon → client. One JSON object per line.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum CompatResponse {
    Ok {
        v: u32,
    },
    Panes {
        v: u32,
        panes: Vec<message::PaneInfo>,
    },
    Output {
        v: u32,
        id: String,
        from_line: usize,
        lines: Vec<String>,
    },
    Exited {
        v: u32,
        id: String,
        code: Option<u32>,
    },
    Error {
        v: u32,
        message: String,
    },
}
