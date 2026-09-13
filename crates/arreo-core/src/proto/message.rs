//! v0 message set (T-0013): the one schema every surface speaks.
//!
//! Every variant carries `v` (schema version, currently 0) so decoders can
//! reject-or-adapt per the negotiation rule. New variants MUST be appended
//! (never renumbered) and new fields MUST be `#[serde(default)]`-optional —
//! that is the whole N−1 mechanism, enforced by the compat test in
//! `tests/proto.rs` and the rules in ADR 0017 (T-0028).
//!
//! **The rule above is about a variant's own fields, and it does not extend to
//! nested structs** (T-0079, measured). `rmp-serde`'s default is to encode a
//! struct as a positional array, so `PaneInfo` travels as `[id, alive, alert]`:
//! a *missing trailing* element decodes to its `#[serde(default)]` value (which
//! is why a new reader survives an old writer), but an old reader given a
//! longer array fails the whole frame rather than skipping what it does not
//! know. A shape that must carry more is therefore a **new struct behind a new
//! variant** — see [`PaneDetail`] and [`Message::PanesDetail`] — not an
//! extended one. Extending [`PaneInfo`] would have been invisible to a peer of
//! the same build and fatal to one a version behind, which is the opposite of
//! what this file is for.

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

/// One pane with the derived detail a sidebar renders (T-0079).
///
/// **A separate struct, and that is the N−1 mechanism, not a style choice.**
/// `rmp-serde` encodes a struct as a *positional array* (its default
/// `struct_map` config is off), so a struct's field list is its wire contract:
/// extending [`PaneInfo`] would put a 7-element array in front of a reader that
/// expects 3, and that reader does not skip the extras — it fails to decode the
/// whole frame. `#[serde(default)]` covers a *missing trailing element* (which
/// is what makes a new reader survive an old writer) but nothing can make an
/// old reader survive a longer array. So a shape that carries more is a **new
/// shape**: this struct, behind [`Message::PanesDetail`], which a peer that has
/// never heard of it never asks for and therefore never has to decode.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PaneDetail {
    pub id: String,
    pub alive: bool,
    #[serde(default)]
    pub alert: Option<String>,
    /// The engine's derived state for this pane, as the daemon has it now.
    ///
    /// Not optional: this struct exists *only* to carry a state, so "no state"
    /// would be a contradiction rather than a value. `AgentState::Unknown` is
    /// the engine's own word for "it has not told me yet".
    pub state: AgentState,
    /// What a pane in `Question` is waiting on (T-0061): the last non-empty
    /// line of its output, which *is* the question. `None` when the pane is not
    /// asking, or is asking by silence — quiet has no text, so nothing is
    /// invented (absent, never a fabricated prompt).
    #[serde(default)]
    pub asking: Option<String>,
    /// Live RSS of the pane's process tree, KiB (T-0040). `None` means **not
    /// measured** (no live child, or the sample failed) — never `0`, which
    /// would render as a real reading of zero.
    #[serde(default)]
    pub ram_kb: Option<u64>,
    /// Recent peak RSS samples for the sparkline, oldest first, KiB (T-0040).
    /// Empty when the series is empty — the view renders nothing rather than a
    /// flat line claiming "steady".
    #[serde(default)]
    pub ram_history: Vec<u64>,
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
    /// Server → client: pane list — id, liveness, alert. Unchanged from v0.
    Panes { v: u32, panes: Vec<PaneInfo> },
    /// Client → server: the same list **with every pane's derived detail**
    /// (T-0079), in one reply.
    ///
    /// This is what a sidebar asks for, and the reason it exists is that the
    /// daemon already knows the answer: `PaneEntry::pump` feeds each pane's
    /// engine from its journal incrementally and `engine_state()` returns the
    /// derived state, so a client that wants "what is every pane doing right
    /// now" can be *told* it instead of asking pane by pane. The old sidebar
    /// asked with two blocking `Wait { timeout_ms: 150 }` calls per pane, which
    /// on a wall of working panes — panes in neither waited-for state — cost
    /// 30 × 300 ms before the first frame could be correct.
    ///
    /// **A new variant rather than more fields on [`PaneInfo`]**: see
    /// [`PaneDetail`] for why an extended nested struct is a wire break in the
    /// one direction `#[serde(default)]` cannot cover. An N−1 daemon answers an
    /// unknown request with a typed `Error` and keeps the connection open (ADR
    /// 0017), which is exactly the signal a client needs to fall back to the
    /// per-pane path — so the fallback is keyed on the *refusal*, not on a flag
    /// a peer might echo without acting on it.
    PanesDetail { v: u32, panes: Vec<PaneDetail> },
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
    ///
    /// `nonce` (T-0038 stage 1 security review, F1) is 32 bytes of entropy the
    /// outgoing daemon minted for this handoff, and the incoming daemon must
    /// present it as the **first bytes** on the transfer connection before any
    /// descriptor moves. It is what binds the transfer to the process that
    /// asked for the handoff on this socket, rather than to any process that
    /// noticed `<socket>.handoff` exist. Empty from a peer that does not send
    /// one (an older build); the outgoing daemon never accepts an empty nonce.
    /// `#[serde(default)]` keeps the enum append-only (ADR 0017).
    HandoffReady {
        v: u32,
        protocol: u32,
        server_protocol: u32,
        panes: u64,
        #[serde(default)]
        nonce: Vec<u8>,
        /// Whether the outgoing daemon will send a pane manifest (T-0038 stage
        /// 2) on the transfer connection after the listener and the lock.
        ///
        /// **This is the N−1 gate for the panes, and its absence is why it
        /// exists.** A stage-1 outgoing daemon sends `HandoffReady` with no
        /// manifest and then transfers no pane: it exits on the commit and its
        /// children are orphaned. A stage-2 incoming daemon that assumed a
        /// manifest would read the descriptor marker bytes as a length and
        /// refuse; one that *committed* without one would kill every agent on
        /// the machine. So the incoming daemon requires this flag before it
        /// commits a cut that has panes, and a peer that does not set it is
        /// refused — a deferred update (the old daemon keeps serving), which is
        /// ADR 0021 §5's answer to a protocol break, and never a half-handoff.
        ///
        /// `#[serde(default)]` keeps the enum append-only (ADR 0017): a stage-1
        /// peer omits the field and it decodes as `false`.
        #[serde(default)]
        manifest: bool,
    },
}

/// One pane in the handoff manifest (T-0038 stage 2).
///
/// **Not a `Message` variant, and that is deliberate.** The manifest carries the
/// panes' scrollback and raw journals, and a pane's journal is capped at
/// `pty::MAX_RAW_JOURNAL` (1 MiB) by the ring's own design — so a full machine's
/// manifest is several megabytes, while `codec::MAX_FRAME_BYTES` (1 MiB) is the
/// ceiling on any framed message. It therefore travels on the transfer
/// connection, length-prefixed by the grammar in `arreo_server::handoff`, where
/// the length is checked against [`MAX_MANIFEST_BYTES`] before it becomes an
/// allocation.
///
/// The fields are exactly what the receiving daemon needs to rebuild the pane it
/// will serve. What is *not* here is deliberate: the state engine's state and
/// its `fed` counter (derived — the incoming daemon re-derives them by feeding
/// `raw` through a fresh engine, which is why `raw` travels at all), the metrics
/// sampler (derived, and it samples `/proc`), and the guard itself (a cgroup
/// *directory*, so it travels as a path and is re-opened on the other side).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct HandoffPane {
    pub id: String,
    /// The spawn spec, for `split` and the audit trail.
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    /// The pid the sender forked, or `None` when it could not resolve one. A pid
    /// the inherited terminal contradicts refuses the adoption (ADR 0021 §2b).
    #[serde(default)]
    pub child_pid: Option<u32>,
    /// The lines the sender had already read (oldest first) — the scrollback a
    /// client reads after the cut.
    #[serde(default)]
    pub lines: Vec<String>,
    /// The sender's unterminated trailing partial line, carried as a partial so
    /// that neither side has to flush it.
    #[serde(default)]
    pub pending: String,
    /// The raw byte journal: the state engine's input, without which a pane that
    /// was `Question` arrives `Unknown`.
    #[serde(default)]
    pub raw: Vec<u8>,
    #[serde(default)]
    pub raw_truncated: bool,
    /// What the bounded ring already evicted/truncated. Not derivable from
    /// `lines`, so it travels rather than resetting to zero — a fresh zero would
    /// report "0 dropped" about a pane whose history was evicted.
    #[serde(default)]
    pub dropped: u64,
    #[serde(default)]
    pub dropped_bytes: u64,
    /// The cgroup directory this pane's guard lives in, when it has one. The
    /// incoming daemon re-opens it by path; a guard that cannot be re-opened is
    /// a refusal, never a pane served without its memory ceiling.
    #[serde(default)]
    pub guard_path: Option<String>,
    #[serde(default)]
    pub kill_on_breach: bool,
    /// The graded-alert episode (T-0041) the sender had fired: `"warn"`,
    /// `"critical"` or `"breach"`. Carried so the episode does not restart at
    /// the cut — hysteresis exists so a hovering reading emits one row per
    /// crossing, and a cut is not a crossing.
    #[serde(default)]
    pub alert: Option<String>,
    /// The synthetic alert line the sender had already fed into its engine, fed
    /// into the incoming daemon's engine in the same order (after the journal).
    /// Without it a pane that was showing `Blocked` because of an alert would
    /// arrive `Unknown`.
    #[serde(default)]
    pub alert_line: Option<String>,
}

/// The largest encoded manifest the transfer will carry.
///
/// Generous against what the panes can actually hold (a pane's ring is built to
/// stay inside a ≤ 3 MB budget) and **finite**, because the manifest's length is
/// peer-supplied: a length prefix is not an allocation, and a peer that names a
/// gigabyte must be refused rather than believed. A refusal past it is a
/// deferred update, not a lost agent — the outgoing daemon keeps serving.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024 * 1024;

/// The largest number of panes a manifest may name.
///
/// The **structure** bound, where [`MAX_MANIFEST_BYTES`] is the byte bound
/// (F3 of the stage-2 security review): a hostile side that cannot fit a
/// gigabyte of bytes can still fit 200,000 minimal entries in 64 MiB — and
/// each entry decodes to a `HandoffPane` worth ~275 bytes of RSS in the
/// receiving daemon, so the full byte budget of minimal entries is roughly a
/// gigabyte of allocation before the count is even read. The count is read
/// from the encoded array header **before** `rmp_serde` allocates anything, so
/// a manifest over this bound is refused, never allocated, and the refusal
/// names both numbers — a refusal, never a truncation.
///
/// The bound is also what makes N finite (F5): N adopted panes cost N reader
/// threads and ~5–6 descriptors each, and with the entry bound N ≤ 4096, so
/// the transfer's resource cost is bounded by the same number that bounds its
/// allocation. 4096 panes is a machine that shape is not a supported handoff;
/// every real machine is orders of magnitude below it.
pub const MAX_MANIFEST_ENTRIES: usize = 4096;

/// Encode a pane manifest for the transfer connection.
///
/// MessagePack, the same wire format the framed protocol uses, so the manifest
/// needs no second codec — and, unlike JSON, a `Vec<u8>` journal travels as
/// bytes rather than as an array of numbers.
pub fn encode_manifest(panes: &[HandoffPane]) -> Result<Vec<u8>, super::codec::CodecError> {
    rmp_serde::to_vec(panes).map_err(|e| super::codec::CodecError::Encode(e.to_string()))
}

/// Decode a pane manifest from the transfer connection.
///
/// Total on garbage, like every other decoder here: a manifest that does not
/// decode is a refusal, never a panic and never a partially-applied transfer.
///
/// A manifest naming more panes than [`MAX_MANIFEST_ENTRIES`] is refused
/// **before** the body is decoded — see [`manifest_entry_count`] — naming both
/// the count and the bound (F3).
pub fn decode_manifest(bytes: &[u8]) -> Result<Vec<HandoffPane>, super::codec::CodecError> {
    let count = manifest_entry_count(bytes)?;
    if count > MAX_MANIFEST_ENTRIES {
        return Err(super::codec::CodecError::Decode(format!(
            "the manifest names {count} panes (limit {MAX_MANIFEST_ENTRIES})"
        )));
    }
    rmp_serde::from_slice(bytes).map_err(|e| super::codec::CodecError::Decode(e.to_string()))
}

/// The top-level array length of an encoded manifest, read from the MessagePack
/// header alone.
///
/// This is the F3 count check's raw material: the decoder must refuse a
/// manifest that names more panes than [`MAX_MANIFEST_ENTRIES`] **before**
/// `rmp_serde` turns the body into a `Vec<HandoffPane>` — a count check that
/// ran after the decode would be a check on allocation already done. MessagePack
/// puts an array's length in its header (fixarray/array16/array32) before any
/// element, so the count is on the wire before the work. Anything that is not
/// an array is garbage, and a manifest that is not an array is a refusal like
/// any other.
fn manifest_entry_count(bytes: &[u8]) -> Result<usize, super::codec::CodecError> {
    let Some((first, rest)) = bytes.split_first() else {
        return Err(super::codec::CodecError::Decode(
            "an encoded manifest is empty".to_string(),
        ));
    };
    let count = match first {
        0x90..=0x9f => (*first & 0x0f) as usize,
        0xdc => {
            if rest.len() < 2 {
                return Err(super::codec::CodecError::Decode(
                    "an encoded manifest ends inside its array header".to_string(),
                ));
            }
            u16::from_be_bytes([rest[0], rest[1]]) as usize
        }
        0xdd => {
            if rest.len() < 4 {
                return Err(super::codec::CodecError::Decode(
                    "an encoded manifest ends inside its array header".to_string(),
                ));
            }
            u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]) as usize
        }
        _ => {
            return Err(super::codec::CodecError::Decode(
                "an encoded manifest is not an array".to_string(),
            ))
        }
    };
    Ok(count)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// F3 (stage-2 review): a manifest over the entry bound is refused by
    /// name+count **before** the decode can allocate. 4097 minimal entries are
    /// a few hundred kilobytes on the wire — far under [`MAX_MANIFEST_BYTES`],
    /// which is exactly the point: the byte bound alone did not stop 200,000
    /// minimal entries decoding to ~55 MB of RSS, and the structure bound does.
    /// What removal turns red: dropping the [`manifest_entry_count`] check in
    /// [`decode_manifest`] — the manifest decodes into a `Vec` of 4097 panes
    /// instead of being refused.
    #[test]
    fn a_manifest_over_the_entry_bound_is_refused_by_name_and_count() {
        let panes = vec![HandoffPane::default(); MAX_MANIFEST_ENTRIES + 1];
        let encoded = encode_manifest(&panes).expect("encodes");
        assert!(
            encoded.len() < MAX_MANIFEST_BYTES,
            "the bound must be reachable within the byte budget for the test to mean anything"
        );
        let err = decode_manifest(&encoded)
            .expect_err("a manifest over the entry bound is refused, never allocated");
        let text = err.to_string();
        assert!(
            text.contains(&(MAX_MANIFEST_ENTRIES + 1).to_string()),
            "the refusal names the count that arrived: {text}"
        );
        assert!(
            text.contains(&MAX_MANIFEST_ENTRIES.to_string()),
            "and the limit it exceeded: {text}"
        );
        assert!(
            text.contains("limit"),
            "the refusal reads as a bound, not a decode confusion: {text}"
        );
    }

    /// The bound is a refusal, never a truncation: exactly
    /// [`MAX_MANIFEST_ENTRIES`] entries still decode in full.
    #[test]
    fn the_entry_bound_is_a_refusal_not_a_truncation() {
        let panes = vec![HandoffPane::default(); MAX_MANIFEST_ENTRIES];
        let encoded = encode_manifest(&panes).expect("encodes");
        let decoded = decode_manifest(&encoded).expect("at the bound, the manifest decodes");
        assert_eq!(decoded.len(), MAX_MANIFEST_ENTRIES, "nothing is truncated");
    }

    /// A manifest whose header is not an array header is garbage, refused like
    /// any other — the count check must not mistake a map for an empty array.
    #[test]
    fn a_non_array_manifest_is_refused_by_the_count_check() {
        // A MessagePack map `{}` where the manifest's array must be.
        let map = [0x80u8];
        let err = decode_manifest(&map).expect_err("a map is not a manifest");
        assert!(
            err.to_string().contains("not an array"),
            "the refusal says what the shape was: {err}"
        );
    }

    /// **The T-0079 N−1 window, in both directions, over bytes a peer of this
    /// build would send.**
    ///
    /// The shapes are written by *mirror* types — an old daemon's schema,
    /// declared here and serialized with the same encoder — so the bytes are
    /// what a v0 peer really puts on the wire, not a hand-drawn approximation
    /// of them. This is the case the whole design rests on:
    ///
    /// 1. **Old client → new daemon.** A v0 `panes` request (three keys, no
    ///    detail of any kind) decodes to the bare [`Message::Panes`], which is
    ///    the listing `arreo panes` asks for. The new verb is not something a
    ///    v0 client can accidentally send, because it is a different variant.
    /// 2. **Old daemon → new client.** A v0 `panes` reply — `PaneInfo` as the
    ///    3-element array the encoder emits — decodes into the current
    ///    [`PaneInfo`] unchanged. The client's fallback then asks pane by pane
    ///    (see `arreo_tui::client::poll_summaries`), which is the only correct
    ///    reading of a peer that has no state to give.
    /// 3. **New client → old daemon.** A [`Message::PanesDetail`] request fails
    ///    an old daemon's typed decode, which answers a typed `Error` and keeps
    ///    the connection open (ADR 0017) — the signal the fallback keys on.
    /// 4. And the new shape round-trips, so it is a real verb rather than a
    ///    frame only one side can read.
    ///
    /// What removal turns red: extending [`PaneInfo`] instead of adding
    /// [`PaneDetail`] (case 2 stops decoding at all — the old 3-element array
    /// hits a struct that wants 7, and serde refuses the frame rather than
    /// padding it), or making `PanesDetail` a field on `Panes` (case 1 would
    /// then depend on a v0 peer's tolerance of a key it does not know, which
    /// is true for a map entry but not for the nested array it also has to
    /// decode).
    #[test]
    fn the_panes_detail_verb_is_new_while_the_bare_listing_is_unchanged() {
        use crate::proto::codec;

        /// A v0 peer's schema: exactly what shipped before T-0079.
        #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
        #[serde(tag = "op", rename_all = "snake_case")]
        enum V0Message {
            Panes { v: u32, panes: Vec<V0PaneInfo> },
        }
        #[derive(serde::Serialize, serde::Deserialize, Debug, PartialEq)]
        struct V0PaneInfo {
            id: String,
            alive: bool,
            alert: Option<String>,
        }

        // 1. A v0 request decodes to the bare listing, byte-for-byte the same
        //    variant it always was.
        let v0_request = rmp_serde::to_vec(&V0Message::Panes {
            v: 0,
            panes: vec![],
        })
        .expect("encode a v0 request");
        assert_eq!(
            codec::decode(&v0_request).expect("a v0 panes request decodes"),
            Message::Panes {
                v: 0,
                panes: vec![],
            }
        );

        // 2. A v0 reply decodes into the current struct — the pane shape is
        //    untouched, so nothing about it can surprise an old reader or a new
        //    one.
        let v0_reply = rmp_serde::to_vec(&V0Message::Panes {
            v: 0,
            panes: vec![V0PaneInfo {
                id: "p".into(),
                alive: true,
                alert: Some("warn".into()),
            }],
        })
        .expect("encode a v0 reply");
        let decoded = codec::decode(&v0_reply).expect("a v0 panes reply decodes");
        let Message::Panes { panes, .. } = decoded else {
            panic!("a panes reply must decode as Panes");
        };
        assert_eq!(
            panes,
            vec![PaneInfo {
                id: "p".into(),
                alive: true,
                alert: Some("warn".into()),
            }],
            "the v0 pane shape is the current one, field for field"
        );

        // 3. The new verb is a *different variant*, so a v0 reader never sees
        //    it: it only ever arrives in answer to a request a v0 client does
        //    not make. Proving that structurally — the old schema cannot decode
        //    it at all — is the point: this is why the detail did not go into
        //    `PaneInfo`.
        let detail = Message::PanesDetail {
            v: VERSION,
            panes: vec![PaneDetail {
                id: "p".into(),
                alive: true,
                alert: Some("warn".into()),
                state: AgentState::Question,
                asking: Some("Proceed? [y/n]".into()),
                ram_kb: Some(2048),
                ram_history: vec![1024, 2048],
            }],
        };
        let bytes = codec::encode(&detail).expect("encode");
        assert!(
            rmp_serde::from_slice::<V0Message>(&bytes).is_err(),
            "a v0 reader must not be handed a verb it cannot decode — which is \
             exactly why the detail is a new variant and not more fields"
        );

        // 4. And it round-trips for a peer that does speak it.
        assert_eq!(codec::decode(&bytes).expect("decode"), detail);
    }
}
