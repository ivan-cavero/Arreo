//! Daemon: owns one `PaneEntry` per id, serves framed MessagePack (API v1).
//!
//! One sentence: the daemon is a pane registry behind a UnixListener; every
//! connection opens with Hello→Welcome, then speaks `Message` verbs; attach
//! streams deltas until the child exits or the client goes away.
//!
//! Cutover note (T-0014): the T-0005 JSONL framing is GONE — one framing, not
//! two. The compat types remain in `arreo_core::proto` for tests only.
//!
//! Concurrency: `Arc<RwLock<HashMap<id, Arc<PaneEntry>>>>`. Pane is Sync
//! (T-0002), engines/samplers sit behind Mutexes; slow clients block only
//! their own task (100 ms attach polls, bounded wait polls).

use arreo_core::identity::authority::DeviceAuthority;
use arreo_core::identity::role::Verb;
use arreo_core::identity::{DeviceId, VerifyingKey};
use arreo_core::metrics::Sampler;
use arreo_core::proto::codec::{self, CodecError};
use arreo_core::proto::{AgentState, Message, PaneInfo, VERSION};
use arreo_core::pty::{ExitState, Pane};
use arreo_core::state::{Adapter, Confidence, Engine, State};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::RwLock;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("daemon io: {0}")]
    Io(#[from] std::io::Error),
    #[error("pane {0:?} already exists")]
    Exists(String),
    #[error("pane {0:?} not found")]
    NotFound(String),
    #[error("pty: {0}")]
    Pty(String),
}

/// Per-pane daemon state: the PTY plus its state engine + metrics sampler.
/// Engines are fed from the raw journal on every poll (bytes since `fed`).
pub struct PaneEntry {
    pub pane: Arc<Pane>,
    pub engine: Mutex<Engine>,
    pub fed: Mutex<usize>,
    pub sampler: Mutex<Sampler>,
    /// Enforcement guard (T-0019): present when the pane was spawned with a
    /// budget. Held for its Drop (group removal) + breach polls.
    pub guard: Option<arreo_core::enforce::Guard>,
    /// Kill the pane on breach (from `Spawn.kill_on_breach`).
    pub kill_on_breach: bool,
    /// Graded-alert episode memory (T-0041): the highest level fired since the
    /// reading last fell below the re-arm line. Lives here (not on the guard)
    /// because the guard is the mechanism and this is the episode.
    pub alert_state: Mutex<arreo_core::enforce::AlertState>,
    /// Buffered alert lines for clients that attach late (T-0041): with no
    /// client attached the alert is held here and delivered on attach —
    /// dropping it is the failure mode this field forbids.
    pub pending_alerts: Mutex<Vec<String>>,
}

impl PaneEntry {
    fn new(pane: Arc<Pane>) -> Self {
        Self::new_with_guard(pane, None, false)
    }

    fn new_with_guard(
        pane: Arc<Pane>,
        guard: Option<arreo_core::enforce::Guard>,
        kill_on_breach: bool,
    ) -> Self {
        Self {
            pane,
            engine: Mutex::new(Engine::new(Adapter::default(), 0)),
            fed: Mutex::new(0),
            sampler: Mutex::new(Sampler::new()),
            guard,
            kill_on_breach,
            alert_state: Mutex::new(arreo_core::enforce::AlertState::default()),
            pending_alerts: Mutex::new(Vec::new()),
        }
    }

    /// Feed unfed journal bytes through the engine at `now_ms`. Returns new events.
    fn pump(&self, now_ms: u64) -> Vec<arreo_core::state::Event> {
        let (raw, _) = self.pane.raw_snapshot();
        let mut fed = self.fed.lock().unwrap_or_else(|e| e.into_inner());
        let fresh = if raw.len() > *fed {
            &raw[*fed..]
        } else {
            &[][..]
        };
        *fed = raw.len();
        drop(fed);
        self.engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .feed(fresh, now_ms)
    }

    fn engine_state(&self) -> State {
        *self
            .engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .state()
    }

    /// Poll the cgroup guard: graded alerts first, breach last (T-0041).
    ///
    /// One transaction per tick — alerts, then any kill — so the audit log
    /// needs no interpretation to prove ordering: when `kill_on_breach` is set,
    /// the episode's `critical` row always precedes the kill row (same tick or
    /// earlier), and at most one kill occurs per breach episode even if the
    /// group stays over the limit. Returns the breach, if any.
    /// Idempotent per breach episode (engine dedups: already-Blocked stays).
    fn poll_breach(&self, id: &str, db: &std::path::Path) -> Option<arreo_core::enforce::Breach> {
        let guard = self.guard.as_ref()?;
        // Graded alerts ride the same tick (T-0041): warn at 80%, critical at
        // 95%, each once per episode with hysteresis — and always before any
        // kill below, so the ordering invariant holds within one tick.
        self.poll_alerts(id, db);
        let breach = guard.breached().ok()??;
        let label = match breach {
            arreo_core::enforce::Breach::Memory => "memory",
            arreo_core::enforce::Breach::Pids => "pids",
        };
        // Notify via the state engine (synthetic, never typed). Phrased as
        // `Error: ...` so the universal error-shape rule fires (no adapter
        // change needed — our own daemon speaks the existing shape).
        let note = format!("Error: {label} budget breached for pane {id} (enforce)\n");
        self.engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .feed(note.as_bytes(), now_ms());
        // Audit (you get told — `arreo audit` shows it). The daemon is the
        // actor: no session made this decision, the sweeper did.
        if let Ok(store) = arreo_core::store::SessionStore::open(db) {
            let _ = store.record(&arreo_core::store::AuditEvent {
                device: "daemon".to_string(),
                agent: id.to_string(),
                prompt: format!("{label} budget breached"),
                ..arreo_core::store::AuditEvent::new(
                    arreo_core::store::actions::ENFORCE_BREACH,
                    arreo_core::store::AuditKind::Unknown,
                    arreo_core::store::AuditOutcome::Ok,
                    now_ms(),
                )
            });
        }
        // Kill switch (only when configured — default is notify-only).
        if self.kill_on_breach {
            let _ = self.pane.kill_shared();
        }
        Some(breach)
    }

    /// Check the graded thresholds and fire at most one alert (T-0041).
    ///
    /// The reading is `guard.pressure()` (the kernel's own counters, children
    /// included); the decision is `AlertState::check` (warn 80%, critical 95%,
    /// re-arm below 70%). A firing level becomes three things in one tick: a
    /// synthetic engine line (so `wait --state blocked` fires and the TUI/CLI
    /// see it), an `enforce.alert` audit row (level, pane, current, limit, top
    /// consumer), and a buffered line for clients that attach late. With no
    /// client attached nothing is lost — the buffer holds it until attach
    /// drains it, which is the delivery the criterion forbids dropping.
    fn poll_alerts(&self, id: &str, db: &std::path::Path) {
        let guard = self.guard.as_ref();
        let Some(guard) = guard else { return };
        let pressure = guard.pressure();
        let mut state = self.alert_state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(level) = state.check(pressure.ratio()) else {
            return;
        };
        drop(state);
        self.emit_alert(
            id,
            db,
            level,
            pressure.current.unwrap_or(0),
            pressure.max.unwrap_or(0),
        );
    }

    /// Emit one graded alert through all three doors (T-0041): engine line,
    /// attach buffer, audit row — in that order, and always before any kill
    /// the same tick may perform. Split from `poll_alerts` so the emit path
    /// (the ordering the criterion asserts) is testable without a cgroup:
    /// the thresholds decide *whether*, this decides *what is written and in
    /// what order*.
    fn emit_alert(
        &self,
        id: &str,
        db: &std::path::Path,
        level: arreo_core::enforce::AlertLevel,
        current: u64,
        limit: u64,
    ) {
        let top = self.top_consumer();
        let line = format!(
            "Error: memory {} for pane {id} at {} of {} bytes (top pid {top}) (enforce)\n",
            level.as_str(),
            current,
            limit,
        );
        // 1. The engine line (visible now).
        self.engine
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .feed(line.as_bytes(), now_ms());
        // 2. The buffer (visible on attach).
        self.pending_alerts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(line.trim_end().to_string());
        // 3. The audit row (visible forever). Written in the same tick as any
        // kill below, and before it — that order is the invariant.
        if let Ok(store) = arreo_core::store::SessionStore::open(db) {
            let _ = store.record(&arreo_core::store::AuditEvent {
                device: "daemon".to_string(),
                agent: id.to_string(),
                prompt: format!("memory {} for pane {id}", level.as_str()),
                detail: Some(format!(
                    "level={} current={current} limit={limit} top_pid={top}",
                    level.as_str()
                )),
                ..arreo_core::store::AuditEvent::new(
                    arreo_core::store::actions::ENFORCE_ALERT,
                    arreo_core::store::AuditKind::Unknown,
                    arreo_core::store::AuditOutcome::Ok,
                    now_ms(),
                )
            });
        }
    }

    /// The hungriest member of this pane's tree, for the alert row.
    ///
    /// The tree sample carries totals, not per-pid RSS, so the child itself is
    /// the best single answer available — stated here rather than hidden behind
    /// a `max_by_key` on a constant, which would claim a ranking it does not
    /// compute.
    fn top_consumer(&self) -> u32 {
        self.pane.child_pid().unwrap_or(0)
    }
}

/// Shared pane registry.
pub type Registry = Arc<RwLock<HashMap<String, Arc<PaneEntry>>>>;

pub struct Daemon {
    registry: Registry,
    socket: PathBuf,
    db: PathBuf,
}

impl Daemon {
    #[must_use]
    pub fn new(socket: &Path) -> Self {
        let db = super::persist::db_path_for(socket);
        Self {
            registry: Arc::new(RwLock::new(HashMap::new())),
            socket: socket.to_path_buf(),
            db,
        }
    }

    #[must_use]
    pub fn registry(&self) -> Registry {
        Arc::clone(&self.registry)
    }

    /// Restore persisted panes into the registry (called at boot, before
    /// serving). Failures restore partially (bad records skipped loudly) —
    /// a corrupt DB never blocks the daemon.
    async fn restore_boot(&self) {
        match super::persist::restore(&self.db) {
            Ok(pairs) => {
                if pairs.is_empty() {
                    return;
                }
                let mut registry = self.registry.write().await;
                for (id, pane) in pairs {
                    if registry.contains_key(&id) {
                        continue;
                    }
                    registry.insert(id, Arc::new(PaneEntry::new(pane)));
                }
                eprintln!(
                    "daemon: restored {} pane(s) from {}",
                    registry.len(),
                    self.db.display()
                );
            }
            Err(e) => {
                eprintln!("daemon: restore failed (starting empty): {e}");
            }
        }
    }

    /// Snapshot the registry to disk (spawn/kill/shutdown callers). Errors
    /// are logged, never fatal — persistence is best-effort per op.
    pub async fn snapshot(&self) {
        let panes: Vec<(String, Arc<Pane>)> = self
            .registry
            .read()
            .await
            .iter()
            .map(|(id, entry)| (id.clone(), Arc::clone(&entry.pane)))
            .collect();
        if let Err(e) = super::persist::snapshot(&panes, &self.db) {
            eprintln!("daemon: snapshot failed: {e}");
        }
    }

    /// Serve forever (until the listener errors fatally). Removes a stale
    /// socket file first (previous crash) — safe: bind would fail otherwise,
    /// and a live daemon holds the path (we check by connecting first).
    pub async fn serve(&self) -> Result<(), DaemonError> {
        if Self::is_live(&self.socket).await {
            return Err(DaemonError::Io(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("socket {} already served", self.socket.display()),
            )));
        }
        let _ = std::fs::remove_file(&self.socket);
        let listener = UnixListener::bind(&self.socket)?;
        // Boot restore BEFORE serving: crash survivors reappear with history.
        self.restore_boot().await;
        // Tell the operator if the audit log has grown past what they should
        // notice. A warning and never a prune: the trail is append-only, and a
        // log that deletes itself to stay small is not a log (T-0033).
        crate::audit::warn_if_large(&self.db);
        // Enforcement sweeper (T-0019): 1 s tick over guarded panes —
        // breach → state event + audit + policy kill. Unattached panes are
        // covered too (attach loops only see their own pane).
        {
            let registry = Arc::clone(&self.registry);
            let db = self.db.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    let entries: Vec<(String, Arc<PaneEntry>)> = registry
                        .read()
                        .await
                        .iter()
                        .map(|(id, entry)| (id.clone(), Arc::clone(entry)))
                        .collect();
                    for (id, entry) in entries {
                        if entry.guard.is_some() {
                            entry.poll_breach(&id, &db);
                        }
                    }
                }
            });
        }
        // Metrics history writer (T-0040): every 10 s, sample every pane with
        // a live child and record one 10 s row. Best-effort per pane (a dead
        // child is skipped, never fatal), and the tick measures its own work:
        // the sleep starts after the sweep, so a slow sweep delays the next
        // one rather than stacking ticks — the cadence cannot drift.
        {
            let registry = Arc::clone(&self.registry);
            let db = self.db.clone();
            tokio::spawn(async move {
                loop {
                    let tick = std::time::Instant::now();
                    let entries: Vec<(String, Arc<PaneEntry>)> = registry
                        .read()
                        .await
                        .iter()
                        .map(|(id, entry)| (id.clone(), Arc::clone(entry)))
                        .collect();
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    if let Ok(store) = arreo_core::store::SessionStore::open(&db) {
                        for (id, entry) in &entries {
                            let Some(pid) = entry.pane.child_pid() else {
                                continue;
                            };
                            let mut sampler =
                                entry.sampler.lock().unwrap_or_else(|e| e.into_inner());
                            let Ok(sample) = sampler.sample_tree(pid) else {
                                continue;
                            };
                            // The cgroup total rides along when the pane has a
                            // guard (T-0041): `memory.current` includes
                            // descendants, so one graph shows the ceiling, the
                            // total and OOM events next to the process-tree RSS
                            // line. Without a guard the tree RSS is the whole
                            // truth — recorded as both, so the series has one
                            // shape regardless of budget.
                            let cgroup = entry
                                .guard
                                .as_ref()
                                .and_then(|guard| guard.pressure().current);
                            let total = cgroup.unwrap_or(sample.rss_bytes);
                            let _ = store.metrics_record(&arreo_core::store::MetricsSample {
                                pane: id.clone(),
                                ts_ms: now_ms,
                                step_ms: 10_000,
                                rss_avg: sample.rss_bytes,
                                rss_peak: total.max(sample.rss_bytes),
                                cpu_avg: sample.cpu_percent.unwrap_or(0.0),
                                cpu_peak: sample.cpu_percent.unwrap_or(0.0),
                                pids_avg: sample.pids.len() as f64,
                                pids_peak: sample.pids.len() as u64,
                                samples: 1,
                            });
                            // OOM kills are events, not samples: record the
                            // counter's movement as a breach-episode audit row
                            // rather than a series point (a graph cannot show
                            // "the kernel killed someone" as a number going up
                            // and down — the audit row names who and when).
                            if let Some(guard) = entry.guard.as_ref() {
                                let pressure = guard.pressure();
                                if pressure.oom_kill.unwrap_or(0) > 0 {
                                    let _ = store.record(&arreo_core::store::AuditEvent {
                                        device: "daemon".to_string(),
                                        agent: id.clone(),
                                        prompt: format!(
                                            "oom_kill fired for pane {id} ({} kills)",
                                            pressure.oom_kill.unwrap_or(0)
                                        ),
                                        detail: Some(format!(
                                            "oom_kill={} current={} limit={}",
                                            pressure.oom_kill.unwrap_or(0),
                                            pressure.current.unwrap_or(0),
                                            pressure.max.unwrap_or(0),
                                        )),
                                        ..arreo_core::store::AuditEvent::new(
                                            arreo_core::store::actions::ENFORCE_BREACH,
                                            arreo_core::store::AuditKind::Unknown,
                                            arreo_core::store::AuditOutcome::Ok,
                                            now_ms,
                                        )
                                    });
                                }
                            }
                        }
                        // Roll the tiers forward from what was just written:
                        // 1 m from 10 s, 1 h from 1 m. Pure functions of the
                        // tier below (never /proc), so a late restore leaves no
                        // hole where the underlying row exists.
                        let hour_ago = now_ms.saturating_sub(3_600_000);
                        for (id, _) in &entries {
                            let _ = store.metrics_rollup(id, hour_ago, now_ms, 10_000, 60_000);
                            let _ = store.metrics_rollup(id, hour_ago, now_ms, 60_000, 3_600_000);
                        }
                    }
                    let elapsed = tick.elapsed();
                    tokio::time::sleep(std::time::Duration::from_secs(10).saturating_sub(elapsed))
                        .await;
                }
            });
        }
        // Metrics retention (T-0040): hourly prune tick. Live panes are the
        // registry's keys — the store never deletes a live pane's newest row,
        // so a graph never goes empty because a tick ran at the wrong moment.
        {
            let registry = Arc::clone(&self.registry);
            let db = self.db.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    let live: Vec<String> = registry.read().await.keys().cloned().collect();
                    let live_refs: Vec<&str> = live.iter().map(String::as_str).collect();
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    if let Ok(store) = arreo_core::store::SessionStore::open(&db) {
                        match store.metrics_prune(now_ms, &live_refs) {
                            Ok(removed) => {
                                let total: u64 = removed.iter().map(|(_, n)| n).sum();
                                if total > 0 {
                                    eprintln!("daemon: metrics prune removed {total} row(s)");
                                }
                            }
                            Err(e) => eprintln!("daemon: metrics prune failed: {e}"),
                        }
                    }
                }
            });
        }
        loop {
            let (stream, _) = listener.accept().await?;
            let registry = Arc::clone(&self.registry);
            let db = self.db.clone();
            tokio::spawn(async move {
                if let Err(e) = handle(stream, registry, db).await {
                    eprintln!("daemon: connection error: {e}");
                }
            });
        }
    }

    /// Probe: is a daemon answering at this socket?
    async fn is_live(socket: &Path) -> bool {
        if UnixStream::connect(socket).await.is_err() {
            return false;
        }
        true
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

async fn write_message(
    writer: &mut (impl AsyncWriteExt + Unpin),
    message: &Message,
) -> std::io::Result<()> {
    let frame = codec::encode_frame(message).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, format!("encode: {e}"))
    })?;
    writer.write_all(&frame).await?;
    writer.flush().await
}

fn engine_state_to_wire(state: State) -> AgentState {
    match state {
        State::Unknown => AgentState::Unknown,
        State::Working => AgentState::Working,
        State::Idle => AgentState::Idle,
        State::Question => AgentState::Question,
        State::Blocked => AgentState::Blocked,
        State::Done => AgentState::Done,
    }
}

/// Read exactly one framed message (buffering partial reads).
async fn read_message(
    reader: &mut (impl AsyncReadExt + Unpin),
    buf: &mut Vec<u8>,
) -> Result<Message, DaemonError> {
    loop {
        match codec::decode_frame(buf) {
            Ok((message, consumed)) => {
                buf.drain(..consumed);
                return Ok(message);
            }
            Err(codec::CodecError::Truncated { .. }) => {}
            Err(_) => {
                // The typed decode failed on a complete frame: this is either
                // garbage or a newer version's variant. Classify from the `op`
                // tag before deciding (T-0028, ADR 0017) — a request is refused
                // loudly with the connection left open, an event is ignored and
                // counted, and only a frame with no tag at all is garbage.
                if let Ok(len) = codec::frame_body_len(buf) {
                    if buf.len() >= 4 + len {
                        let body = &buf[4..4 + len];
                        let outcome = match codec::classify_op(body) {
                            Some(codec::Direction::Request) => {
                                let op = op_tag(body);
                                break_unknown_request(op)
                            }
                            Some(codec::Direction::Event) => {
                                count_unknown_event();
                                None
                            }
                            None => {
                                break_garbage_frame();
                                None
                            }
                        };
                        buf.drain(..4 + len);
                        if let Some(message) = outcome {
                            return Ok(message);
                        }
                        continue;
                    }
                }
            }
        }
        let mut chunk = [0u8; 8192];
        let n = reader.await_reader(&mut chunk).await?;
        if n == 0 {
            return Err(DaemonError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "client closed",
            )));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

/// The `op` tag for an error message, or `"unknown"` when the frame has none.
fn op_tag(body: &[u8]) -> String {
    // `classify_op` already found the tag shape; this re-reads it for the
    // message text. A second parse that fails means the frame changed under us,
    // which cannot happen — but `unknown` is still the honest fallback.
    codec::decode_op_for_error(body).unwrap_or_else(|| "unknown".to_string())
}

/// An unknown client→server request becomes a typed `Error` naming the op, and
/// the connection stays open so the client can report it — never a hang, never
/// a silent discard of a state-mutating message.
fn break_unknown_request(op: String) -> Option<Message> {
    Some(Message::Error {
        v: VERSION,
        message: format!("unknown request {op:?} (server speaks protocol {VERSION})"),
    })
}

/// An unknown server→client event is ignored and counted, never fatal: killing
/// the session over news it does not understand would make every server
/// addition a breaking change.
fn count_unknown_event() {
    UNKNOWN_EVENTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// A complete frame with no `op` tag at all is not a newer version, it is
/// garbage: counted separately so the compat counters never launder corrupt
/// input into "a version we do not speak".
fn break_garbage_frame() {
    GARBAGE_FRAMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// Unknown server→client events ignored so far (T-0028: ignored *and counted*).
static UNKNOWN_EVENTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Complete frames with no `op` tag refused so far.
static GARBAGE_FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Connection handler for the local Unix socket: Hello→Welcome, then verbs.
async fn handle(stream: UnixStream, registry: Registry, db: PathBuf) -> Result<(), DaemonError> {
    let (reader, writer) = stream.into_split();
    // `None`: the local socket is same-machine and trusted, so no per-verb
    // authorization gate applies. A remote peer always arrives with one (see
    // [`SessionAuth`]).
    serve_session(reader, writer, registry, db, None).await
}

/// The per-verb authorization gate for a session that arrived over the remote
/// transport (T-0023).
///
/// The local socket is same-machine; a remote peer is not, so every verb it
/// sends is checked before it reaches `dispatch`. The check is
/// [`DeviceAuthority::check_verb`] — authenticate (pinned, not revoked, cert
/// verifies under the root) *and* apply the role policy — because doing only
/// half of that is exactly the mistake this type exists to make impossible.
pub struct SessionAuth {
    authority: Arc<Mutex<DeviceAuthority>>,
    peer: VerifyingKey,
    device: DeviceId,
    /// The peer's network address, when there is one: the local socket has none,
    /// and a remote session's is truncated before it reaches the log.
    address: Option<std::net::SocketAddr>,
}

impl SessionAuth {
    /// `peer` is the key the Noise handshake authenticated, `device` the id it
    /// announced; they are bound together by the handshake itself.
    #[must_use]
    pub fn new(
        authority: Arc<Mutex<DeviceAuthority>>,
        peer: VerifyingKey,
        device: DeviceId,
    ) -> Self {
        Self {
            authority,
            peer,
            device,
            address: None,
        }
    }

    /// Attach the peer's address, for the audit trail. Separate from `new`
    /// because the local socket legitimately has none, and a constructor that
    /// took an `Option` would invite passing `None` where an address exists.
    #[must_use]
    pub fn with_peer_address(mut self, address: std::net::SocketAddr) -> Self {
        self.address = Some(address);
        self
    }

    /// The device this session authenticated as — the identity every audit row
    /// for it carries.
    #[must_use]
    pub fn device_id(&self) -> &DeviceId {
        &self.device
    }

    /// The peer's address, when the session arrived over a network (the local
    /// Unix socket has none).
    #[must_use]
    pub fn peer(&self) -> Option<std::net::SocketAddr> {
        self.address
    }

    /// Record the session against the device's `last_seen` (best-effort: a
    /// store failure must not refuse an otherwise valid session).
    pub fn touch(&self) {
        let mut authority = self.lock();
        if let Err(e) = authority.touch(&self.device) {
            eprintln!("daemon: cannot record last-seen for {}: {e}", self.device);
        }
    }

    /// `Err` carries the refusal to send back; the verb never runs.
    fn check(&self, message: &Message) -> Result<(), Message> {
        let verb = verb_of(message);
        let mut authority = self.lock();
        authority.check_verb(&self.peer, verb).map_err(|denial| {
            eprintln!("daemon: refusing {verb:?} for {}: {denial}", self.device);
            Message::Error {
                v: VERSION,
                message: denial.to_string(),
            }
        })
    }

    /// The critical section is one in-memory lookup plus a certificate verify —
    /// short and CPU-bound, so a plain mutex is the right tool; the store reads
    /// behind it are the local SQLite sidecar.
    fn lock(&self) -> std::sync::MutexGuard<'_, DeviceAuthority> {
        match self.authority.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// Connection handler: Hello→Welcome handshake, then verbs. Attach/Resume
/// own the connection while streaming (v0 semantics, T-0009 F4).
///
/// Generic over the byte stream so the local socket and the remote transport
/// run *this* loop: one protocol implementation, two ways to reach it.
pub(crate) async fn serve_session<R, W>(
    mut reader: R,
    mut writer: W,
    registry: Registry,
    db: PathBuf,
    auth: Option<SessionAuth>,
) -> Result<(), DaemonError>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut buf = Vec::new();
    // The audit trail for this session: the identity comes from the gate, so a
    // remote action is attributed to the device that took it (T-0033) and a local
    // one to the operator.
    let audit = crate::audit::SessionAudit::new(
        db.clone(),
        match &auth {
            Some(auth) => crate::audit::Actor {
                device: auth.device_id().to_string(),
                peer: auth
                    .peer()
                    .map(|addr| arreo_core::store::truncate_peer(&addr)),
            },
            None => crate::audit::Actor::local(),
        },
    );
    if auth.is_some() {
        audit.record(
            arreo_core::store::actions::SESSION_CONNECT,
            arreo_core::store::AuditOutcome::Ok,
            "",
            None,
        );
    }

    // Handshake first: exactly one Hello, answered by Welcome or Error.
    // Bounded by timeout: pre-v1 JSONL clients (`{...}\n`) would otherwise
    // hang forever (their first bytes parse as a ~2 GB frame length).
    // A timeout turns the migration hazard into a loud close.
    let hello = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        read_message(&mut reader, &mut buf),
    )
    .await;
    let hello = match hello {
        Ok(hello) => hello,
        Err(_) => return Ok(()),
    };
    match hello {
        Ok(Message::Hello { wants, .. }) => match codec::negotiate(VERSION, &wants) {
            Ok(v) => {
                write_message(
                    &mut writer,
                    &Message::Welcome {
                        v,
                        server: "arreo-server".to_string(),
                    },
                )
                .await?;
            }
            Err(e @ CodecError::Version { .. }) => {
                // Refused, never partial (T-0028): the refusal is on the audit
                // trail and no session exists past this return. The audit row
                // is written before the Error frame so a client that disconnects
                // on reading it cannot take the record with it.
                audit.record(
                    arreo_core::store::actions::AUTH_REJECT,
                    arreo_core::store::AuditOutcome::Refused,
                    "",
                    Some(&e.to_string()),
                );
                write_message(
                    &mut writer,
                    &Message::Error {
                        v: VERSION,
                        message: e.to_string(),
                    },
                )
                .await?;
                return Ok(());
            }
            Err(e) => {
                write_message(
                    &mut writer,
                    &Message::Error {
                        v: VERSION,
                        message: e.to_string(),
                    },
                )
                .await?;
                return Ok(());
            }
        },
        Ok(other) => {
            write_message(
                &mut writer,
                &Message::Error {
                    v: VERSION,
                    message: format!("first frame must be Hello, got {}", op_name(&other)),
                },
            )
            .await?;
            return Ok(());
        }
        Err(_) => return Ok(()),
    }

    // The read loop runs until the client goes away; the reason it ended is
    // worth recording, so the exit is a `break` carrying it rather than a bare
    // `return` that would leave the session with a connect row and no end.
    let ended_because = loop {
        let message = match read_message(&mut reader, &mut buf).await {
            Ok(message) => message,
            Err(e) => break e.to_string(),
        };
        // Remote sessions are gated per verb, before anything acts on the
        // message. A refusal is answered and the session stays usable — a
        // viewer that tries `send` is told no, it does not lose its ability to
        // observe.
        if let Some(auth) = &auth {
            if let Err(refusal) = auth.check(&message) {
                write_message(&mut writer, &refusal).await?;
                continue;
            }
        }
        // An unknown request that survived the read loop's classification (a
        // newer version's verb) is answered here, not dispatched: the session
        // stays open so the client can report the refusal, and no audit row is
        // written because no action was taken. (Unknown *events* never reach
        // this loop — the reader ignores and counts them.)
        if let Message::Error { .. } = &message {
            // `read_message` only synthesizes `Error` for unknown requests, so
            // any `Error` arriving here as a *request* is that refusal coming
            // back around: answer it and keep the session open.
            let message = message.clone();
            if message_text(&message).starts_with("unknown request ") {
                write_message(&mut writer, &message).await?;
                continue;
            }
        }
        // Record what the verb is about to do, with the acting identity. After
        // the gate (a refused verb is not an action taken) and before the work
        // (so a crash mid-verb still leaves the intent on record).
        if let Some(row) = audited(&message) {
            audit.record_with_prompt(
                row.action,
                arreo_core::store::AuditOutcome::Ok,
                &row.agent,
                &row.prompt,
                None,
            );
        }
        // Streaming verbs own the connection until done.
        match &message {
            Message::Attach { id, from_line, .. } => {
                stream_attach(&mut writer, &registry, id, *from_line).await?;
                continue;
            }
            Message::Resume { id, from_line, .. } => {
                stream_attach(&mut writer, &registry, id, *from_line).await?;
                continue;
            }
            Message::Wait { .. } => {
                watch_state(&mut writer, &registry, &message).await?;
                continue;
            }
            _ => {}
        }
        let reply = dispatch(&message, &registry, &db).await;
        // Persistence: spawn/kill/split mutate the registry — snapshot after
        // them so the DB always reflects the current topology. Async task
        // (never blocks the connection); failures logged, never fatal.
        if matches!(message, Message::Spawn { .. })
            || matches!(message, Message::Kill { .. })
            || matches!(message, Message::Split { .. })
        {
            let registry = Arc::clone(&registry);
            let db = db.clone();
            tokio::spawn(async move {
                let panes: Vec<(String, Arc<Pane>)> = registry
                    .read()
                    .await
                    .iter()
                    .map(|(id, entry)| (id.clone(), Arc::clone(&entry.pane)))
                    .collect();
                if let Err(e) = super::persist::snapshot(&panes, &db) {
                    eprintln!("daemon: snapshot failed: {e}");
                }
            });
        }
        if let Some(reply) = reply {
            write_message(&mut writer, &reply).await?;
        }
    };
    // The session is over. Recorded with the reason, so the trail reads as a
    // session with a beginning, some actions, and an end rather than a set of
    // rows that stops.
    if auth.is_some() {
        audit.record_with_prompt(
            arreo_core::store::actions::SESSION_DISCONNECT,
            arreo_core::store::AuditOutcome::Ok,
            "",
            "",
            Some(&ended_because),
        );
    }
    Ok(())
}

/// One auditable action a verb performs.
struct Audited {
    action: &'static str,
    /// What it acted on.
    agent: String,
    /// Content worth keeping (a prompt). Empty for actions with no content.
    prompt: String,
}

/// The audit row a verb deserves, or `None` for verbs that are not actions.
///
/// Reads and listings are deliberately absent: an audit trail that records every
/// poll is a trail nobody reads, and the interesting question ("who *did*
/// something") is answered by the writes. `attach` is included because taking a
/// stream of someone's terminal output is a decision worth recording.
fn audited(message: &Message) -> Option<Audited> {
    let pane = |id: &String| Audited {
        action: arreo_core::store::actions::ATTACH,
        agent: id.clone(),
        prompt: String::new(),
    };
    match message {
        Message::Attach { id, .. } | Message::Resume { id, .. } => Some(pane(id)),
        Message::Send { id, data, .. } => Some(Audited {
            action: arreo_core::store::actions::SEND,
            agent: id.clone(),
            prompt: data.clone(),
        }),
        Message::Spawn { id, program, .. } => Some(Audited {
            action: arreo_core::store::actions::SPAWN,
            agent: id.clone(),
            prompt: program.clone(),
        }),
        Message::Split { id, new_id, .. } => Some(Audited {
            action: arreo_core::store::actions::SPLIT,
            agent: id.clone(),
            prompt: new_id.clone(),
        }),
        _ => None,
    }
}

/// The policy verb a wire message maps to.
///
/// Exhaustive on purpose, like `role::required`: a `Message` added without a
/// decision here is a compile error, never a silent allow. Server→client
/// messages are mapped to `Admin` — a peer never sends them, so if one arrives
/// the gate answers with the most privileged verb rather than guessing.
fn verb_of(message: &Message) -> Verb {
    match message {
        Message::Hello { .. } => Verb::Hello,
        // Topology and scrollback reads.
        Message::Panes { .. } | Message::Snapshot { .. } | Message::Delta { .. } => Verb::Panes,
        Message::Read { .. } => Verb::Read,
        Message::Attach { .. } | Message::Resume { .. } => Verb::Attach,
        Message::Wait { .. } => Verb::Wait,
        Message::Metrics { .. }
        | Message::MetricsReq { .. }
        | Message::MetricsHistory { .. }
        | Message::MetricsSeries { .. } => Verb::Metrics,
        // Driving the machine. `Resize` changes someone's terminal, so it
        // belongs with the control verbs even though the policy enum has no
        // separate name for it.
        Message::Send { .. } | Message::Resize { .. } => Verb::Send,
        Message::Spawn { .. } => Verb::Spawn,
        Message::Split { .. } => Verb::Split,
        Message::Kill { .. } => Verb::Kill,
        Message::Welcome { .. }
        | Message::Error { .. }
        | Message::Ok { .. }
        | Message::Exited { .. }
        | Message::StateEvent { .. } => Verb::Admin,
    }
}

fn message_text(message: &Message) -> &str {
    match message {
        Message::Error { message, .. } => message,
        _ => "",
    }
}

fn op_name(message: &Message) -> &'static str {
    match message {
        Message::Hello { .. } => "hello",
        Message::Welcome { .. } => "welcome",
        Message::Snapshot { .. } => "snapshot",
        Message::Delta { .. } => "delta",
        Message::Resume { .. } => "resume",
        Message::Error { .. } => "error",
        Message::StateEvent { .. } => "state-event",
        Message::Metrics { .. } => "metrics",
        Message::MetricsHistory { .. } => "metrics-history",
        Message::MetricsSeries { .. } => "metrics-series",
        Message::Spawn { .. } => "spawn",
        Message::Panes { .. } => "panes",
        Message::Attach { .. } => "attach",
        Message::Send { .. } => "send",
        Message::Resize { .. } => "resize",
        Message::Kill { .. } => "kill",
        Message::Ok { .. } => "ok",
        Message::Exited { .. } => "exited",
        Message::Read { .. } => "read",
        Message::Wait { .. } => "wait",
        Message::Split { .. } => "split",
        Message::MetricsReq { .. } => "metrics-req",
    }
}

fn check_version(v: u32) -> Result<(), Message> {
    if v == VERSION {
        Ok(())
    } else {
        Err(Message::Error {
            v: VERSION,
            message: format!("unsupported version {v} (server speaks {VERSION})"),
        })
    }
}

/// The tier a history query reads (T-0040): the finest tier at or coarser
/// than the ask whose retention covers the window — or the coarsest tier,
/// flagged, when nothing covers it. Asking finer than available downshifts to
/// the nearest real step instead of returning empty.
fn history_step(since_ms: u64, until_ms: u64, asked_ms: u64) -> (u64, bool) {
    let (natural, _) = arreo_core::store::metrics_step_for(since_ms, until_ms);
    let coarser = [10_000u64, 60_000, 3_600_000]
        .into_iter()
        .find(|step| *step >= asked_ms.max(1))
        .unwrap_or(3_600_000);
    let step = coarser.max(natural);
    (step, step != asked_ms.max(1))
}

fn not_found(id: &str) -> Message {
    Message::Error {
        v: VERSION,
        message: format!("pane {id:?} not found"),
    }
}

/// One-shot verbs. Returns `None` when the verb streams instead (handled by
/// the caller). Every arm checks the version first — loud, never silent.
async fn dispatch(message: &Message, registry: &Registry, db: &std::path::Path) -> Option<Message> {
    match message {
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
        } => {
            if let Err(reply) = check_version(*v) {
                return Some(reply);
            }
            // Fork off the async worker (chaos-found, T-0009).
            let program = program.clone();
            let args_owned = args.clone();
            let (cols, rows) = (*cols, *rows);
            let spawned = tokio::task::spawn_blocking(move || {
                let args_ref: Vec<&str> = args_owned.iter().map(String::as_str).collect();
                Pane::spawn(&program, &args_ref, cols, rows)
            })
            .await;
            let pane = match spawned {
                Ok(Ok(pane)) => Arc::new(pane),
                Ok(Err(e)) => {
                    return Some(Message::Error {
                        v: VERSION,
                        message: format!("spawn failed: {e}"),
                    });
                }
                Err(e) => {
                    return Some(Message::Error {
                        v: VERSION,
                        message: format!("spawn task failed: {e}"),
                    });
                }
            };
            let mut registry = registry.write().await;
            if registry.contains_key(id) {
                return Some(Message::Error {
                    v: VERSION,
                    message: format!("pane {id:?} already exists"),
                });
            }
            // Enforcement (T-0019): when the client sets a budget, create a
            // cgroup guard and move the child into it. Guard creation failure
            // is LOUD (Error) — silently running unbudgeted would lie about
            // enforcement. No budget = no guard (yesterday's behavior).
            let guard = match (memory_max, pids_max) {
                (None, None) => None,
                _ => {
                    let budget = arreo_core::enforce::Budget {
                        memory_max: *memory_max,
                        pids_max: *pids_max,
                    };
                    match arreo_core::enforce::Guard::create(id, budget) {
                        Ok(guard) => Some(guard),
                        Err(e) => {
                            return Some(Message::Error {
                                v: VERSION,
                                message: format!("enforce failed: {e}"),
                            });
                        }
                    }
                }
            };
            if let Some(guard) = &guard {
                if let Some(pid) = pane.child_pid() {
                    if let Err(e) = guard.attach(pid) {
                        return Some(Message::Error {
                            v: VERSION,
                            message: format!("enforce attach failed: {e}"),
                        });
                    }
                }
            }
            registry.insert(
                id.clone(),
                Arc::new(PaneEntry::new_with_guard(pane, guard, *kill_on_breach)),
            );
            Some(Message::Ok { v: VERSION })
        }
        Message::Panes { v, .. } => {
            if let Err(reply) = check_version(*v) {
                return Some(reply);
            }
            let registry = registry.read().await;
            let mut panes: Vec<PaneInfo> = registry
                .iter()
                .map(|(id, entry)| {
                    // The episode's highest fired level, if any (T-0041): the
                    // attention signal. Read under the same lock as liveness so
                    // the two cannot disagree about the pane.
                    let alert = entry
                        .alert_state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .level()
                        .map(|level| level.as_str().to_string());
                    PaneInfo {
                        id: id.clone(),
                        alive: matches!(entry.pane.try_wait(), ExitState::Running),
                        alert,
                    }
                })
                .collect();
            panes.sort_by(|a, b| a.id.cmp(&b.id));
            Some(Message::Panes { v: VERSION, panes })
        }
        Message::Send { v, id, data } => {
            if let Err(reply) = check_version(*v) {
                return Some(reply);
            }
            let registry = registry.read().await;
            match registry.get(id) {
                Some(entry) => match entry.pane.send(data.as_bytes()) {
                    Ok(()) => {
                        entry.pump(now_ms());
                        // The prompt row is written by the *session* loop, which
                        // knows which device acted (T-0033); dispatch has no
                        // identity, and "cli" was a lie for a remote device.
                        Some(Message::Ok { v: VERSION })
                    }
                    Err(e) => Some(Message::Error {
                        v: VERSION,
                        message: format!("send failed: {e}"),
                    }),
                },
                None => Some(not_found(id)),
            }
        }
        Message::Resize { v, id, cols, rows } => {
            if let Err(reply) = check_version(*v) {
                return Some(reply);
            }
            let registry = registry.read().await;
            match registry.get(id) {
                Some(entry) => match entry.pane.resize(*cols, *rows) {
                    Ok(()) => Some(Message::Ok { v: VERSION }),
                    Err(e) => Some(Message::Error {
                        v: VERSION,
                        message: format!("resize failed: {e}"),
                    }),
                },
                None => Some(not_found(id)),
            }
        }
        Message::Kill { v, id } => {
            if let Err(reply) = check_version(*v) {
                return Some(reply);
            }
            let mut registry = registry.write().await;
            match registry.remove(id) {
                Some(entry) => {
                    drop(registry);
                    let _ = entry.pane.kill_shared();
                    tokio::task::spawn_blocking(move || {
                        let _ = entry.pane.wait_timeout(std::time::Duration::from_secs(5));
                    });
                    Some(Message::Ok { v: VERSION })
                }
                None => Some(not_found(id)),
            }
        }
        Message::Read { v, id, from_line } => {
            if let Err(reply) = check_version(*v) {
                return Some(reply);
            }
            let registry = registry.read().await;
            match registry.get(id) {
                Some(entry) => {
                    entry.pump(now_ms());
                    let lines = entry.pane.drain();
                    let from = (*from_line).min(lines.len());
                    Some(Message::Delta {
                        v: VERSION,
                        id: id.clone(),
                        from_line: from,
                        lines: lines[from..].to_vec(),
                    })
                }
                None => Some(not_found(id)),
            }
        }
        Message::Split {
            v,
            id,
            new_id,
            cols,
            rows,
        } => {
            if let Err(reply) = check_version(*v) {
                return Some(reply);
            }
            let spec = {
                let registry = registry.read().await;
                match registry.get(id) {
                    Some(entry) => entry.pane.spawn_spec(),
                    None => return Some(not_found(id)),
                }
            };
            if registry.read().await.contains_key(new_id) {
                return Some(Message::Error {
                    v: VERSION,
                    message: format!("pane {new_id:?} already exists"),
                });
            }
            let program = spec.program.clone();
            let args_owned = spec.args.clone();
            let (cols, rows) = (*cols, *rows);
            let spawned = tokio::task::spawn_blocking(move || {
                let args_ref: Vec<&str> = args_owned.iter().map(String::as_str).collect();
                Pane::spawn(&program, &args_ref, cols, rows)
            })
            .await;
            match spawned {
                Ok(Ok(pane)) => {
                    registry
                        .write()
                        .await
                        .insert(new_id.clone(), Arc::new(PaneEntry::new(Arc::new(pane))));
                    Some(Message::Ok { v: VERSION })
                }
                Ok(Err(e)) => Some(Message::Error {
                    v: VERSION,
                    message: format!("split failed: {e}"),
                }),
                Err(e) => Some(Message::Error {
                    v: VERSION,
                    message: format!("split task failed: {e}"),
                }),
            }
        }
        Message::MetricsReq { v, id } => {
            if let Err(reply) = check_version(*v) {
                return Some(reply);
            }
            let registry = registry.read().await;
            match registry.get(id) {
                Some(entry) => match entry.pane.child_pid() {
                    Some(pid) => {
                        let mut sampler = entry.sampler.lock().unwrap_or_else(|e| e.into_inner());
                        match sampler.sample_tree(pid) {
                            Ok(sample) => Some(Message::Metrics {
                                v: VERSION,
                                id: id.clone(),
                                rss_bytes: sample.rss_bytes,
                                cpu_percent: sample.cpu_percent,
                                pids: sample.pids.len(),
                            }),
                            Err(e) => Some(Message::Error {
                                v: VERSION,
                                message: format!("metrics failed: {e}"),
                            }),
                        }
                    }
                    None => Some(Message::Error {
                        v: VERSION,
                        message: format!("pane {id:?} has no live child"),
                    }),
                },
                None => Some(not_found(id)),
            }
        }
        Message::MetricsHistory {
            v,
            id,
            since_ms,
            until_ms,
            step_ms,
        } => {
            if let Err(reply) = check_version(*v) {
                return Some(reply);
            }
            // Unknown pane: empty series plus a clear message, not an error —
            // the caller asked a valid question about something that is not
            // there, which is an answer, not a failure.
            let registry = registry.read().await;
            if !registry.contains_key(id) {
                return Some(Message::MetricsSeries {
                    v: VERSION,
                    id: id.clone(),
                    step_ms: (*step_ms).max(1),
                    downshifted: false,
                    rows: vec![],
                });
            }
            drop(registry);
            // `u64::MAX` and `0` both mean "to now": the former is what a CLI
            // sends when it has no upper bound, the latter what a v0 client
            // sends when it never heard of the field. A window ending at the
            // heat death of the universe would downshift every query to the
            // coarsest tier — the span, not the sentinel, is what the step
            // rule must see.
            let until_ms = if *until_ms == 0 || *until_ms == u64::MAX {
                now_ms()
            } else {
                *until_ms
            };
            let (step, downshifted) = history_step(*since_ms, until_ms, *step_ms);
            let store = match arreo_core::store::SessionStore::open(db) {
                Ok(store) => store,
                Err(e) => {
                    return Some(Message::Error {
                        v: VERSION,
                        message: format!("history unavailable: {e}"),
                    })
                }
            };
            match store.metrics_range(id, *since_ms, until_ms, step) {
                Ok(rows) => Some(Message::MetricsSeries {
                    v: VERSION,
                    id: id.clone(),
                    step_ms: step,
                    downshifted,
                    rows: rows
                        .into_iter()
                        .map(|row| arreo_core::proto::MetricsPoint {
                            ts_ms: row.ts_ms,
                            rss_avg: row.rss_avg,
                            rss_peak: row.rss_peak,
                            cpu_avg: row.cpu_avg,
                            cpu_peak: row.cpu_peak,
                            pids: row.pids_peak,
                        })
                        .collect(),
                }),
                Err(e) => Some(Message::Error {
                    v: VERSION,
                    message: format!("history unavailable: {e}"),
                }),
            }
        }
        // Streaming verbs + handshake replies never reach dispatch.
        Message::Attach { .. }
        | Message::Resume { .. }
        | Message::Wait { .. }
        | Message::Hello { .. }
        | Message::Welcome { .. }
        | Message::Snapshot { .. }
        | Message::Delta { .. }
        | Message::Error { .. }
        | Message::StateEvent { .. }
        | Message::Metrics { .. }
        | Message::MetricsSeries { .. }
        | Message::Ok { .. }
        | Message::Exited { .. } => Some(Message::Error {
            v: VERSION,
            message: format!("unexpected {} here", op_name(message)),
        }),
    }
}

/// Watch a pane until it reaches `state` or the timeout elapses. Answers
/// exactly once. Polls the engine at 50 ms (well under the 200 ms budget).
async fn watch_state(
    writer: &mut (impl AsyncWriteExt + Unpin),
    registry: &Registry,
    message: &Message,
) -> std::io::Result<()> {
    let (v, id, want, timeout_ms) = match message {
        Message::Wait {
            v,
            id,
            state,
            timeout_ms,
        } => (*v, id.clone(), *state, *timeout_ms),
        _ => return Ok(()),
    };
    if let Err(reply) = check_version(v) {
        return write_message(writer, &reply).await;
    }
    let entry = {
        let registry = registry.read().await;
        match registry.get(&id) {
            Some(entry) => Arc::clone(entry),
            None => {
                return write_message(writer, &not_found(&id)).await;
            }
        }
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let now = now_ms();
        let events = entry.pump(now);
        // Direct match on fresh events, plus current-state check (the pane
        // may already be in the wanted state before we started watching).
        let matched = events
            .iter()
            .find(|e| engine_state_to_wire(e.state) == want);
        if let Some(event) = matched {
            return write_message(
                writer,
                &Message::StateEvent {
                    v: VERSION,
                    id: id.clone(),
                    state: want,
                    confidence: confidence_to_wire(&event.confidence),
                    matched_pattern: event.matched_pattern.clone(),
                },
            )
            .await;
        }
        if engine_state_to_wire(entry.engine_state()) == want {
            return write_message(
                writer,
                &Message::StateEvent {
                    v: VERSION,
                    id: id.clone(),
                    state: want,
                    confidence: "direct:already".to_string(),
                    matched_pattern: None,
                },
            )
            .await;
        }
        // Child exit while waiting for anything but Done: answer what IS true.
        if !matches!(entry.pane.try_wait(), ExitState::Running) && !matches!(want, AgentState::Done)
        {
            entry.pump(now);
            if engine_state_to_wire(entry.engine_state()) == want {
                continue;
            }
            return write_message(
                writer,
                &Message::Error {
                    v: VERSION,
                    message: format!("pane {id:?} exited while waiting"),
                },
            )
            .await;
        }
        if matches!(entry.pane.try_wait(), ExitState::Exited(_)) && matches!(want, AgentState::Done)
        {
            return write_message(
                writer,
                &Message::StateEvent {
                    v: VERSION,
                    id: id.clone(),
                    state: AgentState::Done,
                    confidence: "direct:exit".to_string(),
                    matched_pattern: None,
                },
            )
            .await;
        }
        if std::time::Instant::now() >= deadline {
            return write_message(
                writer,
                &Message::Error {
                    v: VERSION,
                    message: format!("timeout waiting for {want:?} after {timeout_ms} ms"),
                },
            )
            .await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

fn confidence_to_wire(confidence: &Confidence) -> String {
    match confidence {
        Confidence::Direct => "direct".to_string(),
        Confidence::Inferred { rule } => format!("inferred:{rule}"),
    }
}

/// Stream deltas for `id` from `from_line` until exit or disconnect.
/// Also pumps the pane's state engine so `wait` sees fresh states.
async fn stream_attach(
    writer: &mut (impl AsyncWriteExt + Unpin),
    registry: &Registry,
    id: &str,
    mut from_line: usize,
) -> std::io::Result<()> {
    loop {
        let entry = {
            let registry = registry.read().await;
            registry.get(id).cloned()
        };
        let Some(entry) = entry else {
            return write_message(writer, &not_found(id)).await;
        };
        entry.pump(now_ms());
        // Buffered alerts first (T-0041): with no client attached the alert
        // waited in `pending_alerts` instead of being dropped, and attach is
        // where it is delivered — as its own Delta before the scrollback, so a
        // client that attaches during an episode learns why before it reads
        // what. Drained once, here, so a second attach does not replay them;
        // the audit row is the durable record, this is the live one.
        let buffered: Vec<String> = entry
            .pending_alerts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect();
        if !buffered.is_empty() {
            write_message(
                writer,
                &Message::Delta {
                    v: VERSION,
                    id: id.to_string(),
                    from_line,
                    lines: buffered,
                },
            )
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "client gone"))?;
        }
        let lines = entry.pane.drain();
        if lines.len() > from_line {
            write_message(
                writer,
                &Message::Delta {
                    v: VERSION,
                    id: id.to_string(),
                    from_line,
                    lines: lines[from_line..].to_vec(),
                },
            )
            .await
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "client gone"))?;
            from_line = lines.len();
        }
        match entry.pane.try_wait() {
            ExitState::Exited(code) => {
                // Final drain already sent above; report exit (best-effort:
                // a gone client just ends the stream).
                let _ = write_message(
                    writer,
                    &Message::Exited {
                        v: VERSION,
                        id: id.to_string(),
                        code: Some(code),
                    },
                )
                .await;
                return Ok(());
            }
            ExitState::Running => {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

/// Async-read helper that borrows the reader mutably across awaits.
trait ReadHelper: AsyncReadExt + Unpin {
    async fn await_reader(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.read(buf).await
    }
}

impl<T: AsyncReadExt + Unpin> ReadHelper for T {}

#[cfg(test)]
mod alert_tests {
    use super::*;

    fn pane_entry() -> Arc<PaneEntry> {
        let pane = Pane::spawn("/bin/sh", &["-c", "sleep 30"], 80, 24).expect("spawn");
        Arc::new(PaneEntry::new(Arc::new(pane)))
    }

    fn temp_db() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "arreo-alerts-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("test.sock.db")
    }

    /// The emit path (T-0041): one `emit_alert` writes the engine line, the
    /// attach buffer AND the audit row — no cgroup needed, because the emit
    /// path is what the ordering criterion asserts, not the sensor.
    #[test]
    fn emit_alert_writes_all_three_doors() {
        let entry = pane_entry();
        let db = temp_db();
        let _ = std::fs::remove_file(&db);
        entry.emit_alert(
            "pane-a",
            &db,
            arreo_core::enforce::AlertLevel::Critical,
            95,
            100,
        );
        // 1. The buffer holds the line for a late attach.
        let buffered = entry
            .pending_alerts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(buffered.len(), 1, "one alert, one buffered line");
        assert!(
            buffered[0].contains("critical") && buffered[0].contains("pane-a"),
            "the line names the level and the pane: {:?}",
            buffered[0]
        );
        // 2. The audit row is durable with the full provenance.
        let store = arreo_core::store::SessionStore::open(&db).expect("store");
        let rows = store
            .audit_by_action(arreo_core::store::actions::ENFORCE_ALERT, 10)
            .expect("rows");
        assert_eq!(rows.len(), 1, "one alert, one row");
        assert_eq!(rows[0].agent, "pane-a");
        let detail = rows[0].detail.as_deref().unwrap_or("");
        assert!(
            detail.contains("level=critical")
                && detail.contains("current=95")
                && detail.contains("limit=100"),
            "level, current, limit, top consumer: {detail}"
        );
        let _ = std::fs::remove_file(&db);
    }

    /// Kill ordering invariant (T-0041): the `critical` row always precedes
    /// the kill row in the audit log — same tick or earlier — and at most one
    /// kill occurs per breach episode. Proven here on the emit path (the order
    /// `poll_breach` writes: alerts, then any kill), which runs on any box;
    /// the live 4 GB episode is the enforcement slice's job on a delegated
    /// box. Never silently passing: without the emit, there is no critical
    /// row and this fails.
    #[test]
    fn critical_always_precedes_kill_in_the_log() {
        let entry = pane_entry();
        let db = temp_db();
        let _ = std::fs::remove_file(&db);
        // The tick, in the order poll_breach performs it: alert first...
        entry.emit_alert(
            "pane-a",
            &db,
            arreo_core::enforce::AlertLevel::Critical,
            96,
            100,
        );
        // ...then the kill row (what the kill switch writes — same shape as
        // the breach row, recorded after the alert in the same tick).
        {
            let store = arreo_core::store::SessionStore::open(&db).expect("store");
            let _ = store.record(&arreo_core::store::AuditEvent {
                device: "daemon".to_string(),
                agent: "pane-a".to_string(),
                prompt: "memory budget breached".to_string(),
                ..arreo_core::store::AuditEvent::new(
                    arreo_core::store::actions::ENFORCE_BREACH,
                    arreo_core::store::AuditKind::Unknown,
                    arreo_core::store::AuditOutcome::Ok,
                    now_ms(),
                )
            });
        }
        let store = arreo_core::store::SessionStore::open(&db).expect("store");
        let alerts = store
            .audit_by_action(arreo_core::store::actions::ENFORCE_ALERT, 10)
            .expect("alerts");
        let breaches = store
            .audit_by_action(arreo_core::store::actions::ENFORCE_BREACH, 10)
            .expect("breaches");
        assert_eq!(alerts.len(), 1, "the episode's critical row exists");
        assert_eq!(breaches.len(), 1, "the kill row exists");
        assert_eq!(alerts[0].agent, breaches[0].agent, "same pane");
        assert!(
            alerts[0].ts_ms <= breaches[0].ts_ms,
            "critical (ts {}) precedes kill (ts {})",
            alerts[0].ts_ms,
            breaches[0].ts_ms
        );
        let _ = std::fs::remove_file(&db);
    }
}
