//! The protocol codec (T-0104).
//!
//! One sentence: every surface speaks length-delimited MessagePack frames of one
//! `Message` enum, so a phone that can build a `Message` can talk to the daemon —
//! and this module is the whole enum, not a subset of it.
//!
//! **Why the whole enum, and not the client's half.** A codec that could only
//! *build* requests would be a codec that cannot *read* the reply, and one that
//! knew only the variants a phone happens to send would decode a daemon's answer
//! as garbage. The protocol is one enum with a `v` on every variant and an
//! `#[serde(default)]` discipline for the N−1 window (ADR 0017); splitting it
//! here would put a second, divergent definition of it in the generated API.
//! `WireMessage` therefore mirrors `Message` variant for variant, and the
//! conversions are exhaustive — a new core variant is a compile error here.
//!
//! **Two shapes that are not the core's.** A `(usize, usize)` cursor becomes the
//! [`WireCursor`] record, because UniFFI has no tuple type; and a `usize` field
//! becomes `u64`, because UniFFI has no pointer-width integer either. A `u64`
//! that will not fit this platform's `usize` is refused rather than truncated
//! (see [`CodecFfiError::OutOfRange`]) — the product ships 64-bit targets where
//! that cannot happen, and a silent wrap on a 32-bit one would be a bug found in
//! the field rather than at the boundary.

use arreo_core::notify::NotifyAction;
use arreo_core::proto::codec;
use arreo_core::proto::{
    AgentState, Message, MetricsPoint, PaneDetail, PaneInfo, SpawnSpec, SyncExchange, SyncOutcome,
    SyncStatus, MAX_FRAME_BYTES, MIN_VERSION, VERSION,
};
use arreo_core::theme::{Color, ThemeTokens, Variant};

use crate::errors::CodecFfiError;
use crate::theme::{color_parse, FfiVariant, ThemeToken};

/// The protocol version this build speaks.
#[uniffi::export]
#[must_use]
pub fn codec_protocol_version() -> u32 {
    VERSION
}

/// The oldest version the N−1 window still accepts.
#[uniffi::export]
#[must_use]
pub fn codec_min_version() -> u32 {
    MIN_VERSION
}

/// The largest frame body the codec will read, in bytes.
///
/// A length prefix naming more is corruption or a hostile peer, not a large
/// message: it is refused before any allocation, so no caller has to decide what
/// "too big" means.
#[uniffi::export]
#[must_use]
pub fn codec_max_frame_bytes() -> u64 {
    MAX_FRAME_BYTES as u64
}

/// The versions a client offers, newest first — the one place a client's offer is
/// written.
#[uniffi::export]
#[must_use]
pub fn codec_client_versions() -> Vec<u32> {
    codec::client_versions()
}

/// Which side of the conversation a wire variant belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDirection {
    /// Client → server: something the client asks the daemon to do. An unknown
    /// one is refused loudly with the connection left open.
    Request,
    /// Server → client: something that happened. An unknown one is ignored and
    /// counted, never fatal.
    Event,
}

impl From<codec::Direction> for FfiDirection {
    fn from(direction: codec::Direction) -> Self {
        match direction {
            codec::Direction::Request => Self::Request,
            codec::Direction::Event => Self::Event,
        }
    }
}

/// Classify a frame body as a request or an event, from its `op` tag.
///
/// This is the compat rule's read path: an unknown variant from a newer peer is
/// classified *before* the typed decoder sees it, so a client can tell "refuse
/// this loudly" from "ignore this and count it" instead of treating every
/// undecodable frame as garbage.
#[uniffi::export]
#[must_use]
pub fn codec_classify_op(frame_body: Vec<u8>) -> Option<FfiDirection> {
    codec::classify_op(&frame_body).map(FfiDirection::from)
}

/// The `op` tag of a frame body, for an error message that names what arrived.
#[uniffi::export]
#[must_use]
pub fn codec_op_name(frame_body: Vec<u8>) -> Option<String> {
    codec::decode_op_for_error(&frame_body)
}

/// Negotiate the version to speak: the highest common to `[server − 1, server]`.
#[uniffi::export]
pub fn codec_negotiate(server: u32, wants: Vec<u32>) -> Result<u32, CodecFfiError> {
    Ok(codec::negotiate(server, &wants)?)
}

/// Encode one message (no length prefix).
#[uniffi::export]
pub fn codec_encode(message: WireMessage) -> Result<Vec<u8>, CodecFfiError> {
    Ok(codec::encode(&to_core(message)?)?)
}

/// Decode one message (no length prefix). Total on garbage: an `Err`, never a
/// panic.
#[uniffi::export]
pub fn codec_decode(bytes: Vec<u8>) -> Result<WireMessage, CodecFfiError> {
    Ok(from_core(&codec::decode(&bytes)?))
}

/// Encode with the `u32 LE` length prefix a stream socket needs.
#[uniffi::export]
pub fn codec_encode_frame(message: WireMessage) -> Result<Vec<u8>, CodecFfiError> {
    Ok(codec::encode_frame(&to_core(message)?)?)
}

/// One decoded frame: the message and how many bytes of the buffer it used.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct WireFrame {
    pub message: WireMessage,
    /// Total frame bytes consumed, prefix included — so a caller advances its
    /// read buffer exactly and a partial frame stays where it was.
    pub consumed: u64,
}

/// Decode one length-prefixed frame from the front of `buffer`.
#[uniffi::export]
pub fn codec_decode_frame(buffer: Vec<u8>) -> Result<WireFrame, CodecFfiError> {
    let (message, consumed) = codec::decode_frame(&buffer)?;
    Ok(WireFrame {
        message: from_core(&message),
        consumed: consumed as u64,
    })
}

/// Read a frame's declared body length without decoding it.
///
/// Bounds-checked and allocation-free, so a caller can decide whether the bytes
/// have arrived yet without trusting the prefix.
#[uniffi::export]
pub fn codec_frame_body_len(buffer: Vec<u8>) -> Result<u64, CodecFfiError> {
    Ok(codec::frame_body_len(&buffer)? as u64)
}

/// A pane's liveness, as the sidebar shows it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct WirePaneInfo {
    pub id: String,
    pub alive: bool,
    /// Graded-alert episode state: absent when no alert fired in the current
    /// episode, else `"warn"` / `"critical"` / `"breach"`.
    pub alert: Option<String>,
}

/// One pane with the derived detail a sidebar renders.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct WirePaneDetail {
    pub id: String,
    pub alive: bool,
    pub alert: Option<String>,
    pub state: WireAgentState,
    /// What a pane in `Question` is waiting on: the last non-empty line of its
    /// output, which *is* the question. Absent when the pane is not asking, or is
    /// asking by silence — quiet has no text, so nothing is invented.
    pub asking: Option<String>,
    /// Live RSS of the pane's process tree, KiB. Absent means **not measured**,
    /// never `0`, which would render as a real reading of zero.
    pub ram_kb: Option<u64>,
    /// Recent peak RSS samples for the sparkline, oldest first, KiB. Empty when
    /// the series is empty — a view renders nothing rather than a flat line
    /// claiming "steady".
    pub ram_history: Vec<u64>,
}

/// A pane's spawn parameters, as [`WireMessage::SpawnWorktree`] carries them.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct WireSpawnSpec {
    pub program: String,
    pub args: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    pub memory_max: Option<u64>,
    pub pids_max: Option<u32>,
    pub kill_on_breach: bool,
}

/// One history point: average and peak, so a graph draws the line and labels the
/// worst moment from the same row.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct WireMetricsPoint {
    pub ts_ms: u64,
    pub rss_avg: u64,
    pub rss_peak: u64,
    pub cpu_avg: f64,
    pub cpu_peak: f64,
    pub pids: u64,
}

impl WireMetricsPoint {
    /// The core's point as the boundary's record — one mapping, so the codec and
    /// the metrics read cannot disagree about which field is which.
    pub(crate) fn from_point(point: &MetricsPoint) -> Self {
        Self {
            ts_ms: point.ts_ms,
            rss_avg: point.rss_avg,
            rss_peak: point.rss_peak,
            cpu_avg: point.cpu_avg,
            cpu_peak: point.cpu_peak,
            pids: point.pids,
        }
    }
}

/// One file's payload, as a sync exchange carries it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct WireSyncExchange {
    /// The `SyncPayload` JSON, exactly as the local form builds it. Bounded by
    /// the frame budget, which is checked against the length prefix before the
    /// body is allocated.
    pub payload: Vec<u8>,
}

/// What the receiving machine did with a payload.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct WireSyncOutcome {
    /// The logical file name, echoed so one reply line is self-describing.
    pub file: String,
    pub status: WireSyncStatus,
    /// The machine whose revision this was: the **authenticated device id** of
    /// the sender, which is also the counter key.
    pub from: String,
    /// The sender's counter for this revision, as the receiver recorded it.
    pub counter: u64,
    /// The receiver's revision row id, when it recorded one.
    pub revision: i64,
    /// The conflict copy's file name, for a conflict outcome. No path crosses the
    /// wire — the directory it lives in is the receiver's layout.
    pub copy: String,
    /// The receiver's own words, for a refusal.
    pub reason: String,
}

/// The outcome of one sync exchange, as one word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WireSyncStatus {
    Applied,
    UpToDate,
    Conflict,
    Refused,
}

/// A pane's text cursor: row and column.
///
/// A record because UniFFI has no tuple type; the core's `(usize, usize)` is the
/// same two numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct WireCursor {
    pub row: u64,
    pub col: u64,
}

/// Agent semantic state on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WireAgentState {
    Unknown,
    Working,
    Idle,
    Question,
    Blocked,
    Done,
}

impl From<AgentState> for WireAgentState {
    fn from(state: AgentState) -> Self {
        match state {
            AgentState::Unknown => Self::Unknown,
            AgentState::Working => Self::Working,
            AgentState::Idle => Self::Idle,
            AgentState::Question => Self::Question,
            AgentState::Blocked => Self::Blocked,
            AgentState::Done => Self::Done,
        }
    }
}

impl From<WireAgentState> for AgentState {
    fn from(state: WireAgentState) -> Self {
        match state {
            WireAgentState::Unknown => Self::Unknown,
            WireAgentState::Working => Self::Working,
            WireAgentState::Idle => Self::Idle,
            WireAgentState::Question => Self::Question,
            WireAgentState::Blocked => Self::Blocked,
            WireAgentState::Done => Self::Done,
        }
    }
}

/// The three quick actions a notification offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WireNotifyAction {
    /// Send the operator's text + newline through the pane's send path.
    Reply,
    /// Dismiss the notification; write no pane bytes.
    Skip,
    /// End the pane through the kill path.
    Kill,
}

impl From<NotifyAction> for WireNotifyAction {
    fn from(action: NotifyAction) -> Self {
        match action {
            NotifyAction::Reply => Self::Reply,
            NotifyAction::Skip => Self::Skip,
            NotifyAction::Kill => Self::Kill,
        }
    }
}

impl From<WireNotifyAction> for NotifyAction {
    fn from(action: WireNotifyAction) -> Self {
        match action {
            WireNotifyAction::Reply => Self::Reply,
            WireNotifyAction::Skip => Self::Skip,
            WireNotifyAction::Kill => Self::Kill,
        }
    }
}

/// A theme's resolved tokens, as `Message::ThemeReply` carries them (T-0116).
///
/// **Resolved, not raw**: every value is a literal color the theme file's own
/// spelling round-trips (`#rrggbb`, a 0–255 index, `none`) — never a `defs`
/// reference, because the *server* resolves the file and the receiving surface owns
/// depth. A mirror that carried the document would make every client carry the
/// resolver, and each would be a place the resolution could diverge.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct WireThemeTokens {
    pub name: String,
    pub variant: FfiVariant,
    /// `token -> color`, every value a literal.
    pub tokens: Vec<ThemeToken>,
}

impl ThemeToken {
    /// This token's color in the theme file's own spelling (`#rrggbb`, a 0–255
    /// index, `none`) — the core's `Display`, so a caller that wants the text does
    /// not re-invent the format.
    #[must_use]
    pub fn color_as_text(&self) -> String {
        Color::from(self.color).to_string()
    }
}

/// The one message enum, mirroring `arreo_core::proto::Message` variant for
/// variant. Every variant carries `v`, the protocol version, exactly as the wire
/// does.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum WireMessage {
    /// Client → server: identify + offer versions. The first frame of every
    /// connection; the server answers `Welcome` or `Error`.
    Hello {
        v: u32,
        client: String,
        wants: Vec<u32>,
    },
    /// Server → client: the accepted version + the server identity.
    Welcome { v: u32, server: String },
    /// Either → either: full pane text + cursor (on attach, and on demand).
    Snapshot {
        v: u32,
        id: String,
        lines: Vec<String>,
        cursor: WireCursor,
    },
    /// Server → client: new lines since `from_line` — the hot path.
    Delta {
        v: u32,
        id: String,
        from_line: u64,
        lines: Vec<String>,
    },
    /// Client → server: resume a stream at a cursor.
    Resume { v: u32, id: String, from_line: u64 },
    /// Either → either: loud failure, never silent, never a hang.
    Error { v: u32, message: String },
    /// Server → client: an agent state transition, with confidence and pattern.
    StateEvent {
        v: u32,
        id: String,
        state: WireAgentState,
        /// `"direct"` or `"inferred:<rule>"` — honesty travels on the wire.
        confidence: String,
        matched_pattern: Option<String>,
    },
    /// Server → client: resource truth.
    Metrics {
        v: u32,
        id: String,
        rss_bytes: u64,
        cpu_percent: Option<f64>,
        pids: u64,
    },
    /// Client → server: spawn a pane.
    Spawn {
        v: u32,
        id: String,
        program: String,
        args: Vec<String>,
        cols: u16,
        rows: u16,
        /// Memory ceiling in bytes; absent means unlimited.
        memory_max: Option<u64>,
        /// Process-count ceiling; absent means unlimited.
        pids_max: Option<u32>,
        /// Kill the pane when its budget breaches (default: notify only).
        kill_on_breach: bool,
    },
    /// Client → server: spawn a pane **in a git worktree**, so two agents on one
    /// machine cannot touch each other's files.
    SpawnWorktree {
        v: u32,
        id: String,
        spec: WireSpawnSpec,
        /// Absent or empty means "use the pane id".
        worktree: Option<String>,
    },
    /// Server → client: the pane list — id, liveness, alert.
    Panes { v: u32, panes: Vec<WirePaneInfo> },
    /// Server → client: the pane list **with every pane's derived detail**, in
    /// one reply.
    PanesDetail { v: u32, panes: Vec<WirePaneDetail> },
    /// Client → server: attach to a pane's stream.
    Attach { v: u32, id: String, from_line: u64 },
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
    /// Server → client: the child exited.
    Exited {
        v: u32,
        id: String,
        code: Option<u32>,
    },
    /// Client → server: one-shot read — current text from `from_line`, exactly
    /// one `Snapshot` or `Delta` answered, then silence.
    Read { v: u32, id: String, from_line: u64 },
    /// Client → server: watch a pane until it reaches `state` or `timeout_ms`
    /// elapses. Answers exactly once: `StateEvent` on a match, `Error`
    /// ("timeout") on expiry. The agent-orchestration primitive.
    Wait {
        v: u32,
        id: String,
        state: WireAgentState,
        timeout_ms: u64,
    },
    /// Client → server: split a pane — spawn a sibling running the same program.
    Split {
        v: u32,
        id: String,
        new_id: String,
        cols: u16,
        rows: u16,
    },
    /// Client → server: resource truth for one pane's tree.
    MetricsReq { v: u32, id: String },
    /// Client → server: history for one pane — the durable series, not the live
    /// sample. `u64::MAX` for `until_ms` means "to now".
    MetricsHistory {
        v: u32,
        id: String,
        since_ms: u64,
        until_ms: u64,
        step_ms: u64,
    },
    /// Server → client: one window of the durable series, oldest first.
    /// `step_ms` is the tier actually read; `downshifted` says whether it is the
    /// one that was asked for.
    MetricsSeries {
        v: u32,
        id: String,
        step_ms: u64,
        downshifted: bool,
        rows: Vec<WireMetricsPoint>,
    },
    /// Client → server: ask the serving daemon to hand the socket to a
    /// replacement. `protocol` is the incoming daemon's own version, checked
    /// against the outgoing daemon's window before anything happens.
    Handoff {
        v: u32,
        protocol: u32,
        build: String,
    },
    /// Server → client: the outgoing daemon accepted the handoff and bound the
    /// transfer socket.
    HandoffReady {
        v: u32,
        protocol: u32,
        server_protocol: u32,
        panes: u64,
        /// 32 bytes of entropy the outgoing daemon minted, which the incoming one
        /// must present as the first bytes on the transfer connection. Empty from
        /// a peer that does not send one; the outgoing daemon never accepts an
        /// empty nonce.
        nonce: Vec<u8>,
        /// Whether the outgoing daemon will send a pane manifest on the transfer
        /// connection. The N−1 gate for the panes: a stage-1 peer omits it and it
        /// decodes as false.
        manifest: bool,
    },
    /// Client → server: one synced file's payload.
    Sync { v: u32, exchange: WireSyncExchange },
    /// Server → client: what the receiver did with a `Sync`.
    SyncReply { v: u32, outcome: WireSyncOutcome },
    /// Client → server: act on a notification in one verb — the single door a
    /// script and a phone share.
    NotifyAct {
        v: u32,
        pane: String,
        action: WireNotifyAction,
        /// The operator's reply text, for `Reply`. Absent for skip/kill.
        text: Option<String>,
    },
    /// Server → client: the outcome of a `NotifyAct`.
    NotifyActReply { v: u32, ok: bool, detail: String },
    /// Client → server: ask for a theme's resolved tokens (T-0116). An empty
    /// `name` is "this machine's own theme".
    Theme {
        v: u32,
        name: String,
        variant: FfiVariant,
    },
    /// Server → client: the resolved tokens.
    ThemeReply { v: u32, theme: WireThemeTokens },
}

/// `WireMessage` → the core's `Message`.
///
/// Fallible for one reason only: a `u64` line number that will not fit this
/// platform's `usize` is refused rather than truncated.
fn to_core(message: WireMessage) -> Result<Message, CodecFfiError> {
    Ok(match message {
        WireMessage::Hello { v, client, wants } => Message::Hello { v, client, wants },
        WireMessage::Welcome { v, server } => Message::Welcome { v, server },
        WireMessage::Snapshot {
            v,
            id,
            lines,
            cursor,
        } => Message::Snapshot {
            v,
            id,
            lines,
            cursor: (
                index("cursor.row", cursor.row)?,
                index("cursor.col", cursor.col)?,
            ),
        },
        WireMessage::Delta {
            v,
            id,
            from_line,
            lines,
        } => Message::Delta {
            v,
            id,
            from_line: index("from_line", from_line)?,
            lines,
        },
        WireMessage::Resume { v, id, from_line } => Message::Resume {
            v,
            id,
            from_line: index("from_line", from_line)?,
        },
        WireMessage::Error { v, message } => Message::Error { v, message },
        WireMessage::StateEvent {
            v,
            id,
            state,
            confidence,
            matched_pattern,
        } => Message::StateEvent {
            v,
            id,
            state: AgentState::from(state),
            confidence,
            matched_pattern,
        },
        WireMessage::Metrics {
            v,
            id,
            rss_bytes,
            cpu_percent,
            pids,
        } => Message::Metrics {
            v,
            id,
            rss_bytes,
            cpu_percent,
            pids: index("pids", pids)?,
        },
        WireMessage::Spawn {
            v,
            id,
            program,
            args,
            cols,
            rows,
            memory_max,
            pids_max,
            kill_on_breach,
        } => Message::Spawn {
            v,
            id,
            program,
            args,
            cols,
            rows,
            memory_max,
            pids_max,
            kill_on_breach,
        },
        WireMessage::SpawnWorktree {
            v,
            id,
            spec,
            worktree,
        } => Message::SpawnWorktree {
            v,
            id,
            spec: Box::new(SpawnSpec {
                program: spec.program,
                args: spec.args,
                cols: spec.cols,
                rows: spec.rows,
                memory_max: spec.memory_max,
                pids_max: spec.pids_max,
                kill_on_breach: spec.kill_on_breach,
            }),
            worktree,
        },
        WireMessage::Panes { v, panes } => Message::Panes {
            v,
            panes: panes
                .into_iter()
                .map(|pane| PaneInfo {
                    id: pane.id,
                    alive: pane.alive,
                    alert: pane.alert,
                })
                .collect(),
        },
        WireMessage::PanesDetail { v, panes } => Message::PanesDetail {
            v,
            panes: panes
                .into_iter()
                .map(|pane| PaneDetail {
                    id: pane.id,
                    alive: pane.alive,
                    alert: pane.alert,
                    state: AgentState::from(pane.state),
                    asking: pane.asking,
                    ram_kb: pane.ram_kb,
                    ram_history: pane.ram_history,
                })
                .collect(),
        },
        WireMessage::Attach { v, id, from_line } => Message::Attach {
            v,
            id,
            from_line: index("from_line", from_line)?,
        },
        WireMessage::Send { v, id, data } => Message::Send { v, id, data },
        WireMessage::Resize { v, id, cols, rows } => Message::Resize { v, id, cols, rows },
        WireMessage::Kill { v, id } => Message::Kill { v, id },
        WireMessage::Ok { v } => Message::Ok { v },
        WireMessage::Exited { v, id, code } => Message::Exited { v, id, code },
        WireMessage::Read { v, id, from_line } => Message::Read {
            v,
            id,
            from_line: index("from_line", from_line)?,
        },
        WireMessage::Wait {
            v,
            id,
            state,
            timeout_ms,
        } => Message::Wait {
            v,
            id,
            state: AgentState::from(state),
            timeout_ms,
        },
        WireMessage::Split {
            v,
            id,
            new_id,
            cols,
            rows,
        } => Message::Split {
            v,
            id,
            new_id,
            cols,
            rows,
        },
        WireMessage::MetricsReq { v, id } => Message::MetricsReq { v, id },
        WireMessage::MetricsHistory {
            v,
            id,
            since_ms,
            until_ms,
            step_ms,
        } => Message::MetricsHistory {
            v,
            id,
            since_ms,
            until_ms,
            step_ms,
        },
        WireMessage::MetricsSeries {
            v,
            id,
            step_ms,
            downshifted,
            rows,
        } => Message::MetricsSeries {
            v,
            id,
            step_ms,
            downshifted,
            rows: rows
                .into_iter()
                .map(|point| MetricsPoint {
                    ts_ms: point.ts_ms,
                    rss_avg: point.rss_avg,
                    rss_peak: point.rss_peak,
                    cpu_avg: point.cpu_avg,
                    cpu_peak: point.cpu_peak,
                    pids: point.pids,
                })
                .collect(),
        },
        WireMessage::Handoff { v, protocol, build } => Message::Handoff { v, protocol, build },
        WireMessage::HandoffReady {
            v,
            protocol,
            server_protocol,
            panes,
            nonce,
            manifest,
        } => Message::HandoffReady {
            v,
            protocol,
            server_protocol,
            panes,
            nonce,
            manifest,
        },
        WireMessage::Sync { v, exchange } => Message::Sync {
            v,
            exchange: SyncExchange {
                payload: exchange.payload,
            },
        },
        WireMessage::SyncReply { v, outcome } => Message::SyncReply {
            v,
            outcome: Box::new(SyncOutcome {
                file: outcome.file,
                status: match outcome.status {
                    WireSyncStatus::Applied => SyncStatus::Applied,
                    WireSyncStatus::UpToDate => SyncStatus::UpToDate,
                    WireSyncStatus::Conflict => SyncStatus::Conflict,
                    WireSyncStatus::Refused => SyncStatus::Refused,
                },
                from: outcome.from,
                counter: outcome.counter,
                revision: outcome.revision,
                copy: outcome.copy,
                reason: outcome.reason,
            }),
        },
        WireMessage::NotifyAct {
            v,
            pane,
            action,
            text,
        } => Message::NotifyAct {
            v,
            pane,
            action: NotifyAction::from(action),
            text,
        },
        WireMessage::NotifyActReply { v, ok, detail } => Message::NotifyActReply { v, ok, detail },
        WireMessage::Theme { v, name, variant } => Message::Theme {
            v,
            name,
            variant: Variant::from(variant),
        },
        WireMessage::ThemeReply { v, theme } => Message::ThemeReply {
            v,
            theme: theme_from_wire(&theme),
        },
    })
}

/// The wire's theme → the core's. A token whose color does not parse is dropped
/// rather than guessed at: this direction decodes bytes a peer sent, and a theme
/// with a missing token renders as the base default (the loader's own rule) while a
/// color invented here would be a rendering nobody chose.
fn theme_from_wire(wire: &WireThemeTokens) -> ThemeTokens {
    ThemeTokens {
        name: wire.name.clone(),
        variant: Variant::from(wire.variant),
        tokens: wire
            .tokens
            .iter()
            // The theme file's own spelling, from the core's `Display` — never a
            // format string written here.
            .map(|token| (token.name.clone(), Color::from(token.color).to_string()))
            .collect(),
    }
}

/// The core's theme → the wire's.
fn theme_to_wire(theme: &ThemeTokens) -> WireThemeTokens {
    WireThemeTokens {
        name: theme.name.clone(),
        variant: FfiVariant::from(theme.variant),
        tokens: theme
            .tokens
            .iter()
            .filter_map(|(name, text)| {
                color_parse(text.clone()).ok().map(|color| ThemeToken {
                    name: name.clone(),
                    color,
                })
            })
            .collect(),
    }
}

/// The core's `Message` → `WireMessage`. Infallible: `usize` widens to `u64`.
fn from_core(message: &Message) -> WireMessage {
    match message {
        Message::Hello { v, client, wants } => WireMessage::Hello {
            v: *v,
            client: client.clone(),
            wants: wants.clone(),
        },
        Message::Welcome { v, server } => WireMessage::Welcome {
            v: *v,
            server: server.clone(),
        },
        Message::Snapshot {
            v,
            id,
            lines,
            cursor,
        } => WireMessage::Snapshot {
            v: *v,
            id: id.clone(),
            lines: lines.clone(),
            cursor: WireCursor {
                row: cursor.0 as u64,
                col: cursor.1 as u64,
            },
        },
        Message::Delta {
            v,
            id,
            from_line,
            lines,
        } => WireMessage::Delta {
            v: *v,
            id: id.clone(),
            from_line: *from_line as u64,
            lines: lines.clone(),
        },
        Message::Resume { v, id, from_line } => WireMessage::Resume {
            v: *v,
            id: id.clone(),
            from_line: *from_line as u64,
        },
        Message::Error { v, message } => WireMessage::Error {
            v: *v,
            message: message.clone(),
        },
        Message::StateEvent {
            v,
            id,
            state,
            confidence,
            matched_pattern,
        } => WireMessage::StateEvent {
            v: *v,
            id: id.clone(),
            state: WireAgentState::from(*state),
            confidence: confidence.clone(),
            matched_pattern: matched_pattern.clone(),
        },
        Message::Metrics {
            v,
            id,
            rss_bytes,
            cpu_percent,
            pids,
        } => WireMessage::Metrics {
            v: *v,
            id: id.clone(),
            rss_bytes: *rss_bytes,
            cpu_percent: *cpu_percent,
            pids: *pids as u64,
        },
        Message::Spawn {
            v,
            id,
            program,
            args,
            cols,
            rows,
            memory_max,
            pids_max,
            kill_on_breach,
        } => WireMessage::Spawn {
            v: *v,
            id: id.clone(),
            program: program.clone(),
            args: args.clone(),
            cols: *cols,
            rows: *rows,
            memory_max: *memory_max,
            pids_max: *pids_max,
            kill_on_breach: *kill_on_breach,
        },
        Message::SpawnWorktree {
            v,
            id,
            spec,
            worktree,
        } => WireMessage::SpawnWorktree {
            v: *v,
            id: id.clone(),
            spec: WireSpawnSpec {
                program: spec.program.clone(),
                args: spec.args.clone(),
                cols: spec.cols,
                rows: spec.rows,
                memory_max: spec.memory_max,
                pids_max: spec.pids_max,
                kill_on_breach: spec.kill_on_breach,
            },
            worktree: worktree.clone(),
        },
        Message::Panes { v, panes } => WireMessage::Panes {
            v: *v,
            panes: panes
                .iter()
                .map(|pane| WirePaneInfo {
                    id: pane.id.clone(),
                    alive: pane.alive,
                    alert: pane.alert.clone(),
                })
                .collect(),
        },
        Message::PanesDetail { v, panes } => WireMessage::PanesDetail {
            v: *v,
            panes: panes
                .iter()
                .map(|pane| WirePaneDetail {
                    id: pane.id.clone(),
                    alive: pane.alive,
                    alert: pane.alert.clone(),
                    state: WireAgentState::from(pane.state),
                    asking: pane.asking.clone(),
                    ram_kb: pane.ram_kb,
                    ram_history: pane.ram_history.clone(),
                })
                .collect(),
        },
        Message::Attach { v, id, from_line } => WireMessage::Attach {
            v: *v,
            id: id.clone(),
            from_line: *from_line as u64,
        },
        Message::Send { v, id, data } => WireMessage::Send {
            v: *v,
            id: id.clone(),
            data: data.clone(),
        },
        Message::Resize { v, id, cols, rows } => WireMessage::Resize {
            v: *v,
            id: id.clone(),
            cols: *cols,
            rows: *rows,
        },
        Message::Kill { v, id } => WireMessage::Kill {
            v: *v,
            id: id.clone(),
        },
        Message::Ok { v } => WireMessage::Ok { v: *v },
        Message::Exited { v, id, code } => WireMessage::Exited {
            v: *v,
            id: id.clone(),
            code: *code,
        },
        Message::Read { v, id, from_line } => WireMessage::Read {
            v: *v,
            id: id.clone(),
            from_line: *from_line as u64,
        },
        Message::Wait {
            v,
            id,
            state,
            timeout_ms,
        } => WireMessage::Wait {
            v: *v,
            id: id.clone(),
            state: WireAgentState::from(*state),
            timeout_ms: *timeout_ms,
        },
        Message::Split {
            v,
            id,
            new_id,
            cols,
            rows,
        } => WireMessage::Split {
            v: *v,
            id: id.clone(),
            new_id: new_id.clone(),
            cols: *cols,
            rows: *rows,
        },
        Message::MetricsReq { v, id } => WireMessage::MetricsReq {
            v: *v,
            id: id.clone(),
        },
        Message::MetricsHistory {
            v,
            id,
            since_ms,
            until_ms,
            step_ms,
        } => WireMessage::MetricsHistory {
            v: *v,
            id: id.clone(),
            since_ms: *since_ms,
            until_ms: *until_ms,
            step_ms: *step_ms,
        },
        Message::MetricsSeries {
            v,
            id,
            step_ms,
            downshifted,
            rows,
        } => WireMessage::MetricsSeries {
            v: *v,
            id: id.clone(),
            step_ms: *step_ms,
            downshifted: *downshifted,
            rows: rows.iter().map(WireMetricsPoint::from_point).collect(),
        },
        Message::Handoff { v, protocol, build } => WireMessage::Handoff {
            v: *v,
            protocol: *protocol,
            build: build.clone(),
        },
        Message::HandoffReady {
            v,
            protocol,
            server_protocol,
            panes,
            nonce,
            manifest,
        } => WireMessage::HandoffReady {
            v: *v,
            protocol: *protocol,
            server_protocol: *server_protocol,
            panes: *panes,
            nonce: nonce.clone(),
            manifest: *manifest,
        },
        Message::Sync { v, exchange } => WireMessage::Sync {
            v: *v,
            exchange: WireSyncExchange {
                payload: exchange.payload.clone(),
            },
        },
        Message::SyncReply { v, outcome } => WireMessage::SyncReply {
            v: *v,
            outcome: WireSyncOutcome {
                file: outcome.file.clone(),
                status: match outcome.status {
                    SyncStatus::Applied => WireSyncStatus::Applied,
                    SyncStatus::UpToDate => WireSyncStatus::UpToDate,
                    SyncStatus::Conflict => WireSyncStatus::Conflict,
                    SyncStatus::Refused => WireSyncStatus::Refused,
                },
                from: outcome.from.clone(),
                counter: outcome.counter,
                revision: outcome.revision,
                copy: outcome.copy.clone(),
                reason: outcome.reason.clone(),
            },
        },
        Message::NotifyAct {
            v,
            pane,
            action,
            text,
        } => WireMessage::NotifyAct {
            v: *v,
            pane: pane.clone(),
            action: WireNotifyAction::from(*action),
            text: text.clone(),
        },
        Message::Theme { v, name, variant } => WireMessage::Theme {
            v: *v,
            name: name.clone(),
            variant: FfiVariant::from(*variant),
        },
        Message::ThemeReply { v, theme } => WireMessage::ThemeReply {
            v: *v,
            theme: theme_to_wire(theme),
        },
        Message::NotifyActReply { v, ok, detail } => WireMessage::NotifyActReply {
            v: *v,
            ok: *ok,
            detail: detail.clone(),
        },
    }
}

/// A `u64` the wire carries, as this platform's `usize`.
///
/// The branch is unreachable on the 64-bit targets the product ships, and it is
/// here rather than an `as` cast because a silent wrap would be a line number
/// pointing into the middle of a scrollback the caller never meant to read.
fn index(field: &str, value: u64) -> Result<usize, CodecFfiError> {
    usize::try_from(value).map_err(|_| CodecFfiError::OutOfRange {
        field: field.to_string(),
        value,
    })
}
