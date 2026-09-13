//! T-0079: the poll pass — one request for the whole wall, and the N−1
//! fallback that keeps an old daemon's sidebar correct.
//!
//! Nothing here is mocked *in the sense that matters*: the client under test is
//! `arreo_tui::client::poll_summaries`, speaking real framed `Message` frames
//! over a real Unix socket to a peer that answers them. What the fake peer buys
//! is the one thing a real daemon of this build cannot be asked to do: **behave
//! like an N−1 daemon** and answer a `panes` request without any detail. That is
//! the case the fallback exists for, and the case no amount of driving the
//! current daemon can produce.
//!
//! Both tests assert the **conversation**, not the code: a peer records every
//! verb it is sent, so "one round-trip for the whole wall" is an observation
//! about the wire rather than a claim about a call graph.

use arreo_core::proto::{codec, AgentState, Message, MetricsPoint, PaneDetail, PaneInfo, VERSION};
use arreo_tui::client::{poll_summaries, Client};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// A peer on a Unix socket that answers the poll pass, recording every op it
/// is sent. It is the daemon's shape without the daemon: one connection,
/// request→reply, and `Panes` answered in whichever shape the test asks for.
struct Peer {
    socket: PathBuf,
    seen: Arc<Mutex<Vec<String>>>,
    _task: tokio::task::JoinHandle<()>,
}

impl Peer {
    /// Start a peer that answers with `answer(message)`.
    async fn start(answer: fn(&Message) -> Message) -> Self {
        // A path nobody else in this process is using: the tests in this file
        // run in parallel, each with its own peer.
        let socket = unique_path();
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind the peer socket");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let task = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = Vec::new();
                while let Some((message, op)) = read_frame(&mut stream, &mut buf).await {
                    if let Ok(mut seen) = sink.lock() {
                        seen.push(op);
                    }
                    let reply = answer(&message);
                    let frame = codec::encode_frame(&reply).expect("encode the reply");
                    if tokio::io::AsyncWriteExt::write_all(&mut stream, &frame)
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            }
        });
        Self {
            socket,
            seen,
            _task: task,
        }
    }

    fn ops(&self) -> Vec<String> {
        self.seen.lock().map(|ops| ops.clone()).unwrap_or_default()
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self._task.abort();
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// A socket path no other peer in this process holds.
fn unique_path() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    std::env::temp_dir().join(format!(
        "arreo-poll-test-{}-{}.sock",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Read one framed message, returning it and the wire's own `op` tag — so the
/// recorded conversation is what the peer decoded, not a name this test
/// invented (a renamed variant would fail here, which is the point).
async fn read_frame(
    stream: &mut tokio::net::UnixStream,
    buf: &mut Vec<u8>,
) -> Option<(Message, String)> {
    use tokio::io::AsyncReadExt;
    loop {
        if let Ok((message, consumed)) = codec::decode_frame(buf) {
            // `decode_frame` consumed a `u32 LE` prefix plus the body; the body
            // alone is what the classifier reads.
            let op = codec::decode_op_for_error(&buf[4..consumed]).unwrap_or_else(|| "?".into());
            buf.drain(..consumed);
            return Some((message, op));
        }
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// Two panes, in the v0 shape: one asking, one working — enough to tell the two
/// paths apart in the summary, and enough that a per-pane path has to run twice.
fn bare_panes() -> Vec<PaneInfo> {
    vec![
        PaneInfo {
            id: "asking".to_string(),
            alive: true,
            alert: Some("warn".to_string()),
        },
        PaneInfo {
            id: "busy".to_string(),
            alive: true,
            alert: None,
        },
    ]
}

/// The same two panes with the detail a daemon of this build sends.
fn detailed_panes() -> Vec<PaneDetail> {
    vec![
        PaneDetail {
            id: "asking".to_string(),
            alive: true,
            alert: Some("warn".to_string()),
            state: AgentState::Question,
            asking: Some("Proceed? [y/n]".to_string()),
            ram_kb: Some(4096),
            ram_history: vec![1024, 4096],
        },
        PaneDetail {
            id: "busy".to_string(),
            alive: true,
            alert: None,
            state: AgentState::Working,
            asking: None,
            ram_kb: Some(2048),
            ram_history: vec![2048],
        },
    ]
}

/// **The batched path: one round-trip for the whole wall.**
///
/// The peer answers `panes_detail` with every pane's state, RAM and series in
/// one reply — the shape this build's daemon sends. The conversation must then
/// be exactly `hello`, `panes_detail`: no `metrics_req`, no `metrics_history`,
/// no `wait`, one per pane or otherwise. That is criterion 3's O(1)-per-pass
/// claim, and it is what the 30-pane wall's 300 ms budget rests on.
///
/// What removal turns red: re-introducing a per-pane request anywhere in the
/// detailed path — the recorded ops grow with the pane count.
#[tokio::test]
async fn the_detailed_reply_is_the_whole_pass_and_one_round_trip() {
    let peer = Peer::start(|message| match message {
        Message::Hello { .. } => Message::Welcome {
            v: VERSION,
            server: "peer".to_string(),
        },
        Message::PanesDetail { .. } => Message::PanesDetail {
            v: VERSION,
            panes: detailed_panes(),
        },
        other => panic!("the detailed path asked for {other:?}"),
    })
    .await;

    let mut conn = Client::connect(&peer.socket).await.expect("connect");
    let summaries = poll_summaries(&mut conn).await.expect("poll");

    assert_eq!(summaries.len(), 2);
    let asking = summaries.iter().find(|s| s.id == "asking").unwrap();
    assert_eq!(asking.state, AgentState::Question);
    assert_eq!(asking.asking.as_deref(), Some("Proceed? [y/n]"));
    assert_eq!(asking.ram_kb, 4096);
    assert_eq!(asking.ram_history, vec![1024, 4096]);
    let busy = summaries.iter().find(|s| s.id == "busy").unwrap();
    assert_eq!(busy.state, AgentState::Working);
    assert_eq!(busy.asking, None);
    assert_eq!(busy.ram_kb, 2048);

    let ops = peer.ops();
    assert_eq!(
        ops,
        vec!["hello".to_string(), "panes_detail".to_string()],
        "a detailed pass is one request for every pane, in every shape it needs"
    );
}

/// **The N−1 fallback: a daemon that refuses the new verb still renders the
/// sidebar.**
///
/// The peer answers `panes_detail` with the typed refusal a v0 daemon gives an
/// unknown request, then answers the per-pane verbs the old poll path used. The
/// sidebar must come back with the same *content* it would have had: the
/// asking pane's state resolved by waiting, its question read from its tail, RAM
/// from the metrics verb, and the series from history.
///
/// **The fallback is keyed on that refusal**, not on a flag in the reply: a peer
/// can only refuse a verb by not having it, where a flag could be echoed by a
/// peer that never did the work. What removal turns red: dropping the fallback
/// (every pane renders `Unknown`, and no per-pane verb is ever sent), or
/// keying it on anything but the refusal (a peer that refuses a *different* verb
/// would silently take the slow path).
#[tokio::test]
async fn a_peer_that_refuses_the_detail_verb_still_renders_the_sidebar() {
    let peer = Peer::start(|message| match message {
        Message::Hello { .. } => Message::Welcome {
            v: VERSION,
            server: "old-daemon".to_string(),
        },
        // **The N−1 daemon's answer to the new verb: a typed refusal.** It has
        // never heard of `panes_detail`, so its decode fails, and ADR 0017 says
        // a request it cannot decode is refused loudly by name with the
        // connection left open — which is what the client keys its fallback on.
        // Answered *before* the bare `Panes` arm below, because this is the
        // case the old daemon really is in.
        Message::PanesDetail { .. } => Message::Error {
            v: VERSION,
            message: "unknown request \"panes_detail\"".to_string(),
        },
        // The bare v0 listing: no state, no RAM, no asking line.
        Message::Panes { .. } => Message::Panes {
            v: VERSION,
            panes: bare_panes(),
        },
        Message::MetricsReq { id, .. } => Message::Metrics {
            v: VERSION,
            id: id.clone(),
            rss_bytes: if id == "asking" {
                4096 * 1024
            } else {
                2048 * 1024
            },
            cpu_percent: Some(1.0),
            pids: 1,
        },
        Message::MetricsHistory { id, .. } => Message::MetricsSeries {
            v: VERSION,
            id: id.clone(),
            step_ms: 60_000,
            downshifted: false,
            rows: vec![MetricsPoint {
                ts_ms: 1,
                rss_avg: 1024 * 1024,
                rss_peak: 3072 * 1024,
                cpu_avg: 0.0,
                cpu_peak: 0.0,
                pids: 1,
            }],
        },
        // The old state resolution: `question` answers for the pane that is
        // asking, and times out for every other pane and candidate state.
        Message::Wait { id, state, .. } if id == "asking" && *state == AgentState::Question => {
            Message::StateEvent {
                v: VERSION,
                id: id.clone(),
                state: AgentState::Question,
                confidence: "inferred".to_string(),
                matched_pattern: None,
            }
        }
        Message::Wait { .. } => Message::Error {
            v: VERSION,
            message: "timeout".to_string(),
        },
        Message::Read { id, .. } => Message::Delta {
            v: VERSION,
            id: id.clone(),
            from_line: 0,
            lines: vec!["".to_string(), "Proceed? [y/n] ".to_string()],
        },
        other => panic!("the fallback path asked for {other:?}"),
    })
    .await;

    let mut conn = Client::connect(&peer.socket).await.expect("connect");
    let summaries = poll_summaries(&mut conn).await.expect("poll");

    let asking = summaries.iter().find(|s| s.id == "asking").unwrap();
    assert_eq!(
        asking.state,
        AgentState::Question,
        "the pane that answered `wait` is the state it answered with"
    );
    assert_eq!(
        asking.asking.as_deref(),
        Some("Proceed? [y/n]"),
        "the question comes from the pane's own tail, over the old path"
    );
    assert_eq!(asking.ram_kb, 4096);
    assert_eq!(asking.ram_history, vec![3072], "ready to be a sparkline");
    let busy = summaries.iter().find(|s| s.id == "busy").unwrap();
    assert_eq!(
        busy.state,
        AgentState::Working,
        "a pane that never answered `wait` falls back to liveness, not Unknown"
    );
    assert_eq!(busy.ram_kb, 2048);

    // The conversation is the old one, per pane — which is exactly why the
    // fallback must not be the poll path for a daemon that answers in detail.
    let ops = peer.ops();
    assert_eq!(
        ops.iter().filter(|op| *op == "panes").count(),
        1,
        "one panes request, in either shape: {ops:?}"
    );
    assert_eq!(
        ops.iter().filter(|op| *op == "metrics_req").count(),
        2,
        "the old path asked per pane: {ops:?}"
    );
    assert_eq!(
        ops.iter().filter(|op| *op == "metrics_history").count(),
        2,
        "the old path asked per pane: {ops:?}"
    );
    assert_eq!(
        ops.iter().filter(|op| *op == "wait").count(),
        3,
        "two candidate states per pane, but the pane that is asking is resolved by \
         its first one — which is exactly the cost: every pane that is *not* in a \
         waited-for state pays both timeouts: {ops:?}"
    );
    assert_eq!(
        ops.iter().filter(|op| *op == "read").count(),
        1,
        "only the pane that turned out to be asking is read: {ops:?}"
    );
}
