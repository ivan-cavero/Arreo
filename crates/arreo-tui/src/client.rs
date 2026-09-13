//! Socket client (T-0015): framed MessagePack over the daemon socket, or over
//! the relay to a daemon on another machine (T-0032).
//!
//! The client itself now lives in `arreo_core::mesh::session` (T-0045), because
//! the CLI and a daemon attaching to another machine need the same one — and
//! three copies of a handshake and its retry policy would be three places to
//! drift. This module is what the TUI adds on top: the sidebar's summary shape.

pub use arreo_core::mesh::session::{default_socket, Client, ClientError, RemoteTarget, Target};

use arreo_core::proto::{AgentState, Message, PaneDetail, VERSION};

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

/// One poll pass: every pane's summary in **one round-trip** (T-0079).
///
/// **Criterion 5's answer: the sidebar is *told* the state, it does not *ask*
/// for it.** The daemon already derives each pane's state from its journal
/// (`PaneEntry::pump` feeds the engine incrementally, `engine_state` returns the
/// result), so asking for it here — one `Panes { detail: true }` for the whole
/// wall — is what a sidebar is for. This used to be four round-trips per pane,
/// two of them blocking `Wait { timeout_ms: 150 }` calls from
/// [`state_by_waiting`], so a wall of 30 *working* panes — panes in neither
/// waited-for state, which is what a wall of agents is — paid 30 × 300 ms
/// before the first frame could be correct. Measured: 10.3 s for the waits
/// against a one-round-trip cost of ~20 ms, on a 300 ms budget.
///
/// **`Message::Wait` is not the bug and has not been removed.** It is the
/// orchestration primitive, and `arreo wait --state question` genuinely wants to
/// block until a state *occurs*. What a sidebar wants is the state *now*, and
/// the daemon has it; a per-pane wait is how a client asks for something it is
/// already being told. **Do not re-add one to this path.** The other honest
/// shape is a push — the daemon telling this client when a transition happens —
/// which would decouple the poll rate from state freshness; it is not needed
/// for correctness here, because the tick already bounds staleness by one
/// second and one request per tick is O(1) per pass, not O(panes).
///
/// **The fallback is keyed on the peer *refusing* the verb**, not on a flag it
/// might echo without acting on it. [`Message::PanesDetail`] is a variant an
/// N−1 daemon has never heard of, so its typed decode fails and it answers a
/// typed `Error` with the connection left open (ADR 0017) — the same signal
/// `arreo panes` has always understood, and the only one that cannot lie: a
/// peer can only refuse the verb by not having it.
///
/// **Why a new variant and not a flag on `Panes`**: `rmp-serde` writes a struct
/// as a positional array, so a `PaneInfo` that grew four fields would break an
/// old reader's decode of the whole reply rather than being skipped by it. See
/// [`PaneDetail`]'s doc.
pub async fn poll_summaries(conn: &mut Client) -> Result<Vec<PaneSummary>, String> {
    match conn
        .call(&Message::PanesDetail {
            v: VERSION,
            panes: vec![],
        })
        .await
    {
        // Attention order is the model's job; keep daemon order here.
        Ok(Message::PanesDetail { panes, .. }) => {
            Ok(panes.into_iter().map(summary_from_detail).collect())
        }
        // The N−1 path: a daemon that does not speak this verb refuses it by
        // name (see `refused_unknown_request`). Nothing else falls back — a real
        // failure (a closed connection, a codec error) is a failure, and
        // retrying it pane by pane would turn one honest error into thirty slow
        // ones.
        Ok(Message::Error { message, .. }) if refused_unknown_request(&message, "panes_detail") => {
            poll_summaries_per_pane(conn).await
        }
        Ok(Message::Error { message, .. }) => Err(format!("panes: {message}")),
        Ok(other) => Err(format!("panes: unexpected {other:?}")),
        Err(e) => Err(e.to_string()),
    }
}

/// Whether `message` is a peer refusing the request `op` because it has never
/// heard of it (T-0079).
///
/// **The one signal an N−1 peer can send that cannot lie.** It is the daemon's
/// own refusal shape — `unknown request "op" (server speaks protocol N)`, built
/// by `break_unknown_request` from the frame's `op` tag (ADR 0017) — so a peer
/// says it only by not having the verb, where a flag in a reply could be echoed
/// by a peer that never did the work. Version negotiation cannot answer this
/// either: `PanesDetail` is an append-only variant, so both sides agree on
/// version 0 whether or not the peer has it.
fn refused_unknown_request(message: &str, op: &str) -> bool {
    message.starts_with("unknown request") && message.contains(op)
}

/// Map one detailed [`PaneDetail`] into the sidebar's shape.
///
/// `ram_kb: None` becomes `0`, which is this struct's established word for "not
/// measured" — the sidebar renders `—` for it rather than a bar, so the absence
/// stays visible (`human_ram`/`ram_bar`). The state is not optional here: the
/// daemon derived one for every pane it listed.
fn summary_from_detail(pane: PaneDetail) -> PaneSummary {
    PaneSummary {
        id: pane.id,
        alive: pane.alive,
        state: pane.state,
        ram_kb: pane.ram_kb.unwrap_or(0),
        ram_history: pane.ram_history,
        asking: pane.asking,
    }
}

/// The N−1 path: the peer refused the detail verb, so ask pane by pane the way
/// this client always did. Reachable **only** from [`poll_summaries`]'s fallback.
///
/// Deliberately not the poll path: it is O(panes) round-trips with a blocking
/// wait per pane, which is exactly the cost the batched request replaces. It
/// exists so an old daemon still renders a correct sidebar — the N−1 promise —
/// and never so a new daemon can take it.
async fn poll_summaries_per_pane(conn: &mut Client) -> Result<Vec<PaneSummary>, String> {
    let panes = match conn
        .call(&Message::Panes {
            v: VERSION,
            panes: vec![],
        })
        .await
    {
        Ok(Message::Panes { panes, .. }) => panes,
        Ok(Message::Error { message, .. }) => return Err(format!("panes: {message}")),
        Ok(other) => return Err(format!("panes: unexpected {other:?}")),
        Err(e) => return Err(e.to_string()),
    };
    let mut out = Vec::with_capacity(panes.len());
    for pane in panes {
        // Metrics per pane (best-effort; unknown RAM on error).
        let ram_kb = match conn
            .call(&Message::MetricsReq {
                v: VERSION,
                id: pane.id.clone(),
            })
            .await
        {
            Ok(Message::Metrics { rss_bytes, .. }) => rss_bytes / 1024,
            _ => 0,
        };
        // History for the sparkline (T-0040): last hour at 1 m steps, peaks —
        // best-effort like `ram_kb`, empty when the daemon has no series yet.
        // The window, the tier and the `max(1)` floor are the ones the daemon
        // itself uses in the batched reply, so both paths render one series.
        let since_ms = now_ms().saturating_sub(3_600_000);
        let ram_history = match conn
            .call(&Message::MetricsHistory {
                v: VERSION,
                id: pane.id.clone(),
                since_ms,
                until_ms: u64::MAX,
                step_ms: 60_000,
            })
            .await
        {
            Ok(Message::MetricsSeries { rows, .. }) => rows
                .iter()
                .map(|row| (row.rss_peak / 1024).max(1))
                .collect(),
            _ => Vec::new(),
        };
        let state = state_by_waiting(conn, &pane.id, pane.alive).await;
        // **What it is asking, for a pane that is asking (T-0061).** Fetched
        // only in that state, so an ordinary cycle costs exactly what it cost
        // before. `Read` is a snapshot of the pane's hot ring (`HOT_LINES`),
        // not a consuming read — several readers see the same lines, which is
        // why the focused pane's attach and this cannot steal from each other.
        let asking = if state == AgentState::Question {
            asking_line(conn, &pane.id).await
        } else {
            None
        };
        out.push(PaneSummary {
            id: pane.id,
            alive: pane.alive,
            state,
            ram_kb,
            ram_history,
            asking,
        });
    }
    // Attention order is the model's job; keep daemon order here.
    Ok(out)
}

/// Resolve one pane's state by **asking**, the way an N−1 peer forces: wait ~0
/// for each actionable state in priority order (question → blocked → done),
/// else derive it from liveness.
///
/// Only reachable from [`poll_summaries_per_pane`]. A daemon of this build
/// answers every state at once, so this is the compatibility path, not the poll
/// path — see [`poll_summaries`] for why the sidebar must not ask.
async fn state_by_waiting(conn: &mut Client, id: &str, alive: bool) -> AgentState {
    for want in [AgentState::Question, AgentState::Blocked] {
        if let Ok(Message::StateEvent { .. }) = conn
            .call(&Message::Wait {
                v: VERSION,
                id: id.to_string(),
                state: want,
                timeout_ms: 150,
            })
            .await
        {
            return want;
        }
    }
    if !alive {
        return AgentState::Done;
    }
    AgentState::Working
}

/// Milliseconds since the Unix epoch, 0 when the clock is unreadable.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
