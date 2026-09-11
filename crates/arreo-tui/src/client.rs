//! Socket client (T-0015): framed MessagePack over the daemon socket, or over
//! the relay to a daemon on another machine (T-0032).
//!
//! The client itself now lives in `arreo_core::mesh::session` (T-0045), because
//! the CLI and a daemon attaching to another machine need the same one — and
//! three copies of a handshake and its retry policy would be three places to
//! drift. This module is what the TUI adds on top: the sidebar's summary shape.

pub use arreo_core::mesh::session::{default_socket, Client, ClientError, RemoteTarget, Target};

use arreo_core::proto::AgentState;

/// Pane summary for the sidebar (id + liveness + state + RAM + history).
#[derive(Debug, Clone)]
pub struct PaneSummary {
    pub id: String,
    pub alive: bool,
    pub state: AgentState,
    pub ram_kb: u64,
    /// Recent peak RSS for the sparkline, oldest first, KiB (T-0040).
    /// Best-effort like `ram_kb`: empty when history is unavailable.
    pub ram_history: Vec<u64>,
}
