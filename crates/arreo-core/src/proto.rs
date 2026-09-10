//! v0 wire protocol (T-0005, owned by arreo-core so both server and CLI
//! depend on it without breaking the dependency direction — T-0001 gate).
//!
//! HONESTLY TEMPORARY: T-0013 replaces this with versioned MessagePack.
//! Every message carries `v: 0`; servers reject unknown versions loudly.
//! Request/response shapes mirror the future verbs so clients port cleanly.

use serde::{Deserialize, Serialize};

/// Client → daemon. One JSON object per line (`\n` terminated).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Spawn a pane: `{op: spawn, v, id, program, args, cols, rows}`.
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
    /// List panes: `{op: list, v}`.
    List { v: u32 },
    /// Attach: `{op: attach, v, id, from_line}` — streams `Output` deltas
    /// from `from_line`, then stays subscribed until EOF/close.
    Attach {
        v: u32,
        id: String,
        #[serde(default)]
        from_line: usize,
    },
    /// Send input: `{op: send, v, id, data}` (data is the raw string).
    Send { v: u32, id: String, data: String },
    /// Resize: `{op: resize, v, id, cols, rows}`.
    Resize {
        v: u32,
        id: String,
        cols: u16,
        rows: u16,
    },
    /// Kill: `{op: kill, v, id}`.
    Kill { v: u32, id: String },
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
pub enum Response {
    /// `{op: ok, v, id?}` — ack for spawn/send/resize/kill.
    Ok { v: u32 },
    /// `{op: panes, v, panes: [{id, alive}]}`.
    Panes { v: u32, panes: Vec<PaneInfo> },
    /// `{op: output, v, id, from_line, lines}` — append-delta: `lines` are
    /// the NEW lines since `from_line`; client advances its cursor.
    Output {
        v: u32,
        id: String,
        from_line: usize,
        lines: Vec<String>,
    },
    /// `{op: exited, v, id, code}` — child exited (code None = signal/unknown).
    Exited {
        v: u32,
        id: String,
        code: Option<u32>,
    },
    /// `{op: error, v, message}` — never silent, never a hang.
    Error { v: u32, message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PaneInfo {
    pub id: String,
    pub alive: bool,
}

/// Protocol version we speak.
pub const VERSION: u32 = 0;
