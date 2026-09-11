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

    /// Poll the cgroup guard for breach. On breach: feed a synthetic
    /// error-shape line through the ENGINE (not the pane — never types into
    /// the shell), so the next `wait --state blocked` fires; audit the
    /// event; kill when the spawn policy says so. Returns the breach, if any.
    /// Idempotent per breach episode (engine dedups: already-Blocked stays).
    fn poll_breach(&self, id: &str, db: &std::path::Path) -> Option<arreo_core::enforce::Breach> {
        let guard = self.guard.as_ref()?;
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
        // Audit (you get told — `arreo audit` shows it).
        if let Ok(store) = arreo_core::store::SessionStore::open(db) {
            let _ = store.audit(arreo_core::store::AuditEvent {
                ts_ms: now_ms(),
                device: "daemon".to_string(),
                agent: id.to_string(),
                prompt: format!("[enforce] {label} budget breached"),
            });
        }
        // Kill switch (only when configured — default is notify-only).
        if self.kill_on_breach {
            let _ = self.pane.kill_shared();
        }
        Some(breach)
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
        if let Ok((message, consumed)) = codec::decode_frame(buf) {
            buf.drain(..consumed);
            return Ok(message);
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
        }
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

    loop {
        let message = match read_message(&mut reader, &mut buf).await {
            Ok(message) => message,
            Err(_) => return Ok(()),
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
        Message::Metrics { .. } | Message::MetricsReq { .. } => Verb::Metrics,
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

fn not_found(id: &str) -> Message {
    Message::Error {
        v: VERSION,
        message: format!("pane {id:?} not found"),
    }
}

/// Append one audit row for a sent prompt (best-effort: failures logged).
fn audit_send(db: &Path, id: &str, data: &str) {
    if let Ok(store) = arreo_core::store::SessionStore::open(db) {
        let now = now_ms();
        let _ = store.audit(arreo_core::store::AuditEvent {
            ts_ms: now,
            device: "cli".to_string(),
            agent: id.to_string(),
            prompt: data.to_string(),
        });
    }
}

/// One-shot verbs. Returns `None` when the verb streams instead (handled by
/// the caller). Every arm checks the version first — loud, never silent.
async fn dispatch(message: &Message, registry: &Registry, db: &Path) -> Option<Message> {
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
                .map(|(id, entry)| PaneInfo {
                    id: id.clone(),
                    alive: matches!(entry.pane.try_wait(), ExitState::Running),
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
                        // Audit every prompt (device="cli" pre-auth; device
                        // certs land with pairing). Redaction happens inside
                        // `audit()` — key material never touches disk.
                        audit_send(db, id, data);
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
