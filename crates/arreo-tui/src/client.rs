//! Socket client (T-0015): framed MessagePack over the daemon socket, or over
//! the relay to a daemon on another machine (T-0032).
//!
//! The client itself now lives in `arreo_core::mesh::session` (T-0045), because
//! the CLI and a daemon attaching to another machine need the same one — and
//! three copies of a handshake and its retry policy would be three places to
//! drift. This module is what the TUI adds on top: the sidebar's summary shape.

pub use arreo_core::mesh::session::{default_socket, Client, ClientError, RemoteTarget, Target};

use arreo_core::proto::{AgentState, Message, VERSION};

/// Pane summary for the sidebar (id + liveness + state + RAM + history + what
/// the pane is asking, if it is).
#[derive(Debug, Clone)]
pub struct PaneSummary {
    pub id: String,
    pub alive: bool,
    pub state: AgentState,
    pub ram_kb: u64,
    /// Recent peak RSS for the sparkline, oldest first, KiB (T-0040).
    /// Best-effort like `ram_kb`: empty when history is unavailable.
    pub ram_history: Vec<u64>,
    /// What a blocked agent is waiting for, when it is in `question` (T-0061).
    ///
    /// The last line of its output that said something — which *is* the question,
    /// because that is what an agent's prompt is: text at the end of its
    /// scrollback. Best-effort like the rest: `None` when the pane is not asking,
    /// or when reading its tail failed. Never invented.
    pub asking: Option<String>,
}

/// The line a `question` pane is waiting on: the last non-empty one in its hot
/// ring (T-0061). `None` when it is asking by *silence* — the state engine infers
/// `question` from quiet, and quiet has no text — so a caller shows "waiting"
/// rather than inventing a prompt.
///
/// `Read` is a **snapshot** of the pane's hot ring (`HOT_LINES`), not a consuming
/// read: the buffer is a ring and `from_line` indexes into it, so a second reader
/// sees the same lines. That is what makes a sidebar tail and a focused attach
/// able to coexist — if `Read` drained, one of them would eat the other's output.
///
/// It lives here rather than in the poller because it is the same read on either
/// transport: the sidebar's question is the same call over a socket or over the
/// relay, and a test can prove the remote case without a terminal.
pub async fn asking_line(conn: &mut Client, id: &str) -> Option<String> {
    match conn
        .call(&Message::Read {
            v: VERSION,
            id: id.to_string(),
            from_line: 0,
        })
        .await
    {
        Ok(Message::Delta { lines, .. }) | Ok(Message::Snapshot { lines, .. }) => lines
            .iter()
            .rev()
            .map(|line| line.trim())
            .find(|line| !line.is_empty())
            .map(str::to_string),
        _ => None,
    }
}
