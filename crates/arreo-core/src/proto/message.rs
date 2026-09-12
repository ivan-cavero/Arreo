//! v0 message set (T-0013): the one schema every surface speaks.
//!
//! Every variant carries `v` (schema version, currently 0) so decoders can
//! reject-or-adapt per the negotiation rule. New variants MUST be appended
//! (never renumbered) and new fields MUST be `#[serde(default)]`-optional —
//! that is the whole N−1 mechanism, enforced by the compat test in
//! `tests/proto.rs` and the rules in ADR 0017 (T-0028).

use serde::{Deserialize, Serialize};

/// Protocol version we speak.
///
/// Bumped exactly when a change cannot honor the append-only discipline below;
/// the N−1 window (`codec::negotiate`) keeps the previous version working, and
/// anything older is refused rather than guessed at.
pub const VERSION: u32 = 0;

/// Pane liveness (mirrors the daemon registry view).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PaneInfo {
    pub id: String,
    pub alive: bool,
    /// Graded-alert episode state (T-0041): `None` when no alert fired in the
    /// current episode, else `"warn"` / `"critical"` / `"breach"`. Optional so
    /// a v0 peer decodes it as absent (N−1 rule (c): silent downgrade, and the
    /// CLI renders no column rather than a lie).
    #[serde(default)]
    pub alert: Option<String>,
}

/// Agent semantic state on the wire (subset of the engine states that
/// clients render; `Unknown` included so absence is explicit, never null).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Unknown,
    Working,
    Idle,
    Question,
    Blocked,
    Done,
}

/// The one message enum. Direction notes per variant; over the socket both
/// sides frame with `codec::{encode_frame, decode_frame}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Message {
    /// Client → server: identify + offer versions. First frame of every
    /// connection; server answers `Welcome` or `Error`.
    Hello {
        v: u32,
        client: String,
        wants: Vec<u32>,
    },
    /// Server → client: accepted version + server identity.
    Welcome { v: u32, server: String },
    /// Either → either: full pane text + cursor (on attach, and on demand).
    Snapshot {
        v: u32,
        id: String,
        lines: Vec<String>,
        cursor: (usize, usize),
    },
    /// Server → client: new lines since `from_line` (the hot path — grid
    /// deltas in Phase 1 build on this shape via cell ranges).
    Delta {
        v: u32,
        id: String,
        from_line: usize,
        lines: Vec<String>,
    },
    /// Client → server: resume a stream at a cursor (subway-tunnel /
    /// sleep-survival primitive from ROADMAP §3.2).
    Resume {
        v: u32,
        id: String,
        from_line: usize,
    },
    /// Either → either: loud failure, never silent, never a hang.
    Error { v: u32, message: String },
    /// Server → client: agent state transition with confidence + pattern.
    StateEvent {
        v: u32,
        id: String,
        state: AgentState,
        /// "direct" or "inferred:<rule>" — honesty travels on the wire.
        confidence: String,
        matched_pattern: Option<String>,
    },
    /// Server → client: resource truth (Pillar P3 on the wire).
    Metrics {
        v: u32,
        id: String,
        rss_bytes: u64,
        cpu_percent: Option<f64>,
        pids: usize,
    },
    // --- Pane control (mirrors the T-0005 verbs so the cutover is 1:1) ---
    /// Client → server: spawn a pane.
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
        /// Optional enforcement budget (T-0019): memory bytes + pids ceiling.
        /// `None` = unlimited. `#[serde(default)]` = N−1 safe (old clients
        /// simply spawn unbudgeted panes).
        #[serde(default)]
        memory_max: Option<u64>,
        #[serde(default)]
        pids_max: Option<u32>,
        /// Kill the pane when its budget breaches (default false = notify
        /// only via a Blocked state event + audit row; the operator runs
        /// `arreo kill`). Configurable per the criterion.
        #[serde(default)]
        kill_on_breach: bool,
    },
    /// Server → client: pane list.
    Panes { v: u32, panes: Vec<PaneInfo> },
    /// Client → server: attach to a pane's stream.
    Attach {
        v: u32,
        id: String,
        #[serde(default)]
        from_line: usize,
    },
    /// Client → server: send input bytes (UTF-8 text).
    Send { v: u32, id: String, data: String },
    /// Client → server: resize a pane.
    Resize {
        v: u32,
        id: String,
        cols: u16,
        rows: u16,
    },
    /// Client → server: kill a pane.
    Kill { v: u32, id: String },
    /// Server → client: generic ack.
    Ok { v: u32 },
    /// Server → client: child exited.
    Exited {
        v: u32,
        id: String,
        code: Option<u32>,
    },
    /// Client → server: one-shot read — current text from `from_line`
    /// (no stream; exactly one `Snapshot` or `Delta` answers, then silence).
    Read {
        v: u32,
        id: String,
        #[serde(default)]
        from_line: usize,
    },
    /// Client → server: watch a pane until it reaches `state` or
    /// `timeout_ms` elapses. Answers exactly once: `StateEvent` on match,
    /// `Error` ("timeout") on expiry. The agent orchestration primitive.
    Wait {
        v: u32,
        id: String,
        state: AgentState,
        timeout_ms: u64,
    },
    /// Client → server: split a pane — spawn a sibling running the same
    /// program in `cols`×`rows` (cwd/shape inheritance is T-0015's TUI job;
    /// v1 split = same program, fresh shell, new id).
    Split {
        v: u32,
        id: String,
        new_id: String,
        #[serde(default = "default_cols")]
        cols: u16,
        #[serde(default = "default_rows")]
        rows: u16,
    },
    /// Client → server: resource truth for one pane's tree.
    MetricsReq { v: u32, id: String },
    /// Client → server: history for one pane — the durable series (T-0040),
    /// not the live sample. `since_ms`/`until_ms` bound the window (u64::MAX =
    /// "to now"); `step_ms` asks a tier, and the server downshifts to the
    /// nearest real step when the ask is finer than available, saying so in
    /// the reply's `step_ms`. All three fields are `#[serde(default)]` so a
    /// v0 client that never heard of history still decodes the variant — and a
    /// v0 server answers "history unavailable" rather than zeros, which would
    /// be a lie (N−1, ADR 0017 rule (c)).
    MetricsHistory {
        v: u32,
        id: String,
        #[serde(default)]
        since_ms: u64,
        #[serde(default)]
        until_ms: u64,
        #[serde(default)]
        step_ms: u64,
    },
    /// Server → client: one window of the durable series (T-0040), oldest
    /// first. `step_ms` is the tier actually read (== ask, or the downshift);
    /// `downshifted` says which. Empty `rows` with `downshifted == false` means
    /// "no data in this window", not "unknown pane" — the message carries that
    /// distinction, so the CLI can render it instead of guessing.
    MetricsSeries {
        v: u32,
        id: String,
        step_ms: u64,
        downshifted: bool,
        rows: Vec<MetricsPoint>,
    },
    /// Client → server: ask the serving daemon to hand the socket over to a
    /// replacement (T-0038 stage 1). `protocol` is the incoming daemon's own
    /// `VERSION` — the outgoing daemon checks it against *its* window before
    /// doing anything — and `build` is the incoming daemon's build version
    /// (its `CARGO_PKG_VERSION`), for the audit row and the operator's log.
    /// `#[serde(default)]` so a v0 peer decodes the *shape* rather than
    /// failing the whole frame; the daemon still answers with a typed refusal
    /// when the version is outside its window.
    Handoff {
        v: u32,
        #[serde(default)]
        protocol: u32,
        #[serde(default)]
        build: String,
    },
    /// Server → client: the outgoing daemon accepted the handoff and bound
    /// `<socket>.handoff` for the descriptor transfer. `protocol` is the
    /// agreed version (what the outgoing daemon will speak on the new
    /// daemon's session), `server_protocol` is the outgoing daemon's own
    /// `VERSION` (so the incoming daemon — and the audit row — can name both
    /// sides of the cut), and `panes` is the live pane count (stage 2 will
    /// transfer the panes themselves; stage 1 moves no pane process).
    HandoffReady {
        v: u32,
        protocol: u32,
        server_protocol: u32,
        panes: u64,
    },
}

/// One history point on the wire: average and peak, so the graph draws the line
/// and labels the worst moment from the same row.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MetricsPoint {
    pub ts_ms: u64,
    pub rss_avg: u64,
    pub rss_peak: u64,
    pub cpu_avg: f64,
    pub cpu_peak: f64,
    pub pids: u64,
}

fn default_cols() -> u16 {
    80
}
fn default_rows() -> u16 {
    24
}
