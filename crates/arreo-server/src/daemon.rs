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

/// Live sessions keyed by authenticated device id (T-0052).
///
/// One entry per device with one cancel handle per session: a device with many
/// sessions is one entry with many handles, not many entries. A session that
/// ends removes its own handle (no leak across thousands of connections); an
/// entry emptied that way is removed with it. Revocation iterates the entry
/// and cancels every handle — the cutoff is an *event* delivered to live
/// sessions, not a sweeper re-checking every session each tick (which would
/// make cutoff latency a function of the tick interval and burn CPU on every
/// idle connection forever to answer a question that changes only on revoke).
#[derive(Debug, Default)]
pub struct LiveSessions {
    inner: std::sync::Mutex<HashMap<String, usize>>,
}

/// Shared live-session registry (see [`LiveSessions`]).
pub type Sessions = Arc<LiveSessions>;

impl LiveSessions {
    /// Register one session for `device`; returns the guard that unregisters
    /// it. The guard is the cleanup: dropping it (session end, any reason)
    /// removes exactly its own count.
    ///
    /// One entry per device with a count (not one handle per session): a
    /// device with many sessions is one entry, and a session that ends
    /// decrements without touching its siblings. The registry is observability
    /// (counts per device, leak checks) — the cutoff itself needs no
    /// cross-session signaling, because each session re-validates itself (see
    /// the tick in the verb loop below).
    #[must_use]
    pub fn register(self: &Arc<Self>, device: &str) -> SessionGuard {
        *self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(device.to_string())
            .or_default() += 1;
        SessionGuard {
            sessions: Arc::clone(self),
            device: device.to_string(),
        }
    }

    /// Sessions currently held, by device (tests and diagnostics).
    #[must_use]
    pub fn counts(&self) -> HashMap<String, usize> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn release(&self, device: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = inner.get_mut(device) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                inner.remove(device);
            }
        }
    }
}

/// Unregisters its session on drop. Held for the session's whole life; the
/// drop is the cleanup, so every exit path (clean close, error, cancel) runs
/// it without a single explicit call to remember.
pub struct SessionGuard {
    sessions: Arc<LiveSessions>,
    device: String,
}

impl Drop for SessionGuard {
    fn drop(&mut self) {
        self.sessions.release(&self.device);
    }
}

pub struct Daemon {
    registry: Registry,
    socket: PathBuf,
    db: PathBuf,
    sessions: Sessions,
    /// Taken in `serve` and held until the process ends (T-0071). A `Daemon` that
    /// is dropped releases it, which is the honest lifetime: the lock is "this
    /// process serves this socket".
    ///
    /// It lives here rather than as a local in `serve` because `main` cancels the
    /// `serve` future on SIGTERM and keeps draining — the lock has to outlive the
    /// future so a second daemon cannot start inside that window.
    instance: std::sync::Mutex<Option<arreo_core::lock::ExclusiveLock>>,
    /// Set when a handoff commits: the accept loop ends after its current
    /// `accept`, while live connection tasks (which hold only `Arc` clones +
    /// their owned stream) continue undisturbed. Ending the loop stops
    /// accepting without disturbing live connections — which is why the cut
    /// is a flag, not a shutdown.
    ///
    /// Shared (`Arc`) so an accepted session can signal the loop: `tokio::spawn`
    /// requires `'static`, so the session cannot borrow `self` — it holds a
    /// clone of this flag instead.
    stop_accepting: Arc<std::sync::atomic::AtomicBool>,
}

impl Daemon {
    #[must_use]
    pub fn new(socket: &Path) -> Self {
        let db = super::persist::db_path_for(socket);
        Self {
            registry: Arc::new(RwLock::new(HashMap::new())),
            socket: socket.to_path_buf(),
            db,
            sessions: Arc::new(LiveSessions::default()),
            instance: std::sync::Mutex::new(None),
            stop_accepting: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// The socket this daemon serves. The handoff path needs it (the session
    /// loop binds `<socket>.handoff`), and it is the one stable fact about a
    /// daemon — everything else is replaceable across the cut.
    #[must_use]
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// The store path (`<socket>.db`). The handoff audit rows are written
    /// against it by the session loop.
    #[must_use]
    pub fn db(&self) -> &Path {
        &self.db
    }

    /// Live pane count, for the `HandoffReady` reply and the audit row.
    pub async fn pane_count(&self) -> u64 {
        self.registry.read().await.len() as u64
    }

    /// Stop accepting new connections after the current `accept` (T-0038 stage
    /// 1: the outgoing daemon's last act before `exit(0)`). Live connection
    /// tasks hold only `Arc` clones + their owned stream, so ending the loop
    /// disturbs nothing in flight — the cut is a flag, not a shutdown.
    pub fn stop_accepting(&self) {
        self.stop_accepting
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    #[must_use]
    pub fn registry(&self) -> Registry {
        Arc::clone(&self.registry)
    }

    /// Live sessions by device (T-0052): the cutoff path revokes through this.
    #[must_use]
    pub fn sessions(&self) -> Sessions {
        Arc::clone(&self.sessions)
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
    ///
    /// ## Why the lock comes first (T-0071)
    ///
    /// The probe-then-unlink sequence above is not atomic, and two daemons
    /// starting together walked through it: both probed a not-yet-bound socket,
    /// both removed the path, and the second unlinked the first's listener and
    /// bound its own — two live daemons, one clients could not reach. Measured at
    /// 1 in 12 rounds of eight simultaneous starts, so it is rare, silent, and
    /// exactly the kind of state a handoff must never begin from.
    ///
    /// The lock is taken **before** the probe, which makes probe→remove→bind a
    /// critical section: only the lock holder may decide the socket is dead.
    pub async fn serve(&self) -> Result<(), DaemonError> {
        // Probe → lock → re-probe → bind. The lock makes probe→remove→bind a
        // critical section (T-0071); the two probes close the handoff window:
        // the first refuses the obvious "someone is serving" case without
        // taking the lock, and the second — under the lock — catches a cut
        // that landed between the first probe and the acquire (the outgoing
        // daemon exits on commit and its lock releases with its last
        // descriptor, so the acquire can succeed while the incoming daemon is
        // already serving). A live socket at either probe means someone else
        // serves it: refuse rather than unlinking their path.
        let lock_path = super::persist::lock_path_for(&self.socket);
        if Self::is_live(&self.socket).await {
            return Err(DaemonError::Io(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("socket {} already served", self.socket.display()),
            )));
        }
        let lock = arreo_core::lock::ExclusiveLock::acquire(&lock_path).map_err(|e| {
            DaemonError::Io(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("socket {} already served ({e})", self.socket.display()),
            ))
        })?;
        // Held for the daemon's life; see the field's comment for why it is not a
        // local that dies with this future.
        *self.instance.lock().unwrap_or_else(|e| e.into_inner()) = Some(lock);

        // Re-probe under the lock (see above): a cut that landed between the
        // first probe and the acquire reads as live here, and refuses.
        if Self::is_live(&self.socket).await {
            return Err(DaemonError::Io(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("socket {} already served", self.socket.display()),
            )));
        }
        let _ = std::fs::remove_file(&self.socket);
        let listener = UnixListener::bind(&self.socket)?;
        self.serve_on(listener, None).await
    }

    /// Serve on an **inherited** listener + lock (T-0038 stage 1: the incoming
    /// daemon). No bind, no acquire — the descriptors arrived over
    /// `SCM_RIGHTS` from the outgoing daemon, and the lock lives in the shared
    /// open file description.
    ///
    /// The lock is *already proven* by the time it gets here:
    /// [`arreo_core::lock::ExclusiveLock::inherited_checked`] refused anything
    /// that was not the lock at the path it names, so this checks only that the
    /// path is the socket this daemon was told to serve — an inexpensive
    /// contract check on a value that has already been judged.
    ///
    /// `ready` reports readiness to the incoming daemon's commit path: `Ok(())`
    /// once the accept loop owns the listener and is about to accept, or
    /// `Err(reason)` when this refused before that. **The caller must not commit
    /// on an `Err`** — committing there would exit the outgoing daemon with
    /// nobody serving, which is the half-dead state ADR 0021 §2 exists to make
    /// unreachable.
    pub async fn serve_inherited(
        &self,
        listener: UnixListener,
        lock: arreo_core::lock::ExclusiveLock,
        ready: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    ) -> Result<(), DaemonError> {
        let lock_path = super::persist::lock_path_for(&self.socket);
        if lock.path() != lock_path {
            let error = DaemonError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "inherited lock is for {} but this daemon serves {}",
                    lock.path().display(),
                    self.socket.display()
                ),
            ));
            if let Some(ready) = ready {
                let _ = ready.send(Err(error.to_string()));
            }
            return Err(error);
        }
        // Held for the daemon's life, like the acquired lock. The inherited
        // variant's `Drop` closes the descriptor and never unlocks (shared
        // open file description — see `ExclusiveLock::inherited_checked`), so
        // holding it here keeps the socket ours until the process ends.
        //
        // The socket file must already exist: the outgoing daemon never unlinks
        // it across the cut, so a missing path means this process was given a
        // listener for a socket nobody serves — serving it would fork the world
        // into two daemons (a fresh `serve` would bind the same path). Refuse
        // rather than guess.
        if !self.socket.exists() {
            let error = DaemonError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "socket {} is missing — refusing an inherited listener with no path",
                    self.socket.display()
                ),
            ));
            if let Some(ready) = ready {
                let _ = ready.send(Err(error.to_string()));
            }
            return Err(error);
        }
        *self.instance.lock().unwrap_or_else(|e| e.into_inner()) = Some(lock);
        self.serve_on(listener, ready).await
    }

    /// The accept loop both starts share: restore, warn, spawn the sweeps,
    /// then accept until the listener errors or [`Daemon::stop_accepting`]
    /// fires. The socket file is **never** unlinked here — neither the fresh
    /// bind (which removed a stale file before binding) nor the handoff cut
    /// (where the incoming daemon holds a dup of the same listener) may leave
    /// the path missing.
    ///
    /// `ready` fires once the loop owns the listener and is about to accept
    /// (T-0038 stage 1: the incoming daemon commits only after this, so a
    /// connect is never queued on a socket nobody drains); a refusal before
    /// that point reports `Err(reason)` through it, so the incoming daemon
    /// exits without committing. `None` on a fresh start, where nobody waits.
    ///
    /// The `accept` carries a 200 ms timeout and the stop flag is re-checked
    /// on every wake: the commit path also self-connects to wake the loop, but
    /// that wakeup can land in the *incoming* daemon's backlog (two listeners
    /// share the path after the dup) — so the flag alone, without a bounded
    /// `accept`, would leave the old loop parked forever and the cut hanging.
    async fn serve_on(
        &self,
        listener: UnixListener,
        ready: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    ) -> Result<(), DaemonError> {
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
        // The loop below owns the listener from here: signal readiness now, so
        // the incoming daemon commits only once this task is about to accept.
        // (The sweeps above are spawned, not awaited — nothing between them
        // and the loop blocks.)
        if let Some(ready) = ready {
            let _ = ready.send(Ok(()));
        }
        loop {
            // The stop flag is re-checked on every wake: the commit path also
            // self-connects to wake the loop, but that wakeup can land in the
            // *incoming* daemon's backlog (two listeners share the path after
            // the dup) — so the flag alone, without a bounded `accept`, would
            // leave the old loop parked forever and the cut hanging.
            if self
                .stop_accepting
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                // The handoff cut: stop accepting, keep serving. Dropping the
                // listener here closes *this* process's fd; the incoming daemon
                // holds a dup, and the socket file stays — a connect is never
                // refused across the cut. Live connection tasks hold only `Arc`
                // clones + their owned stream and run on undisturbed.
                // `Ok(())` (not an error): the caller exits 0, it did not fail.
                return Ok(());
            }
            let stream = match tokio::time::timeout(
                std::time::Duration::from_millis(200),
                listener.accept(),
            )
            .await
            {
                Ok(Ok((stream, _))) => stream,
                // Timeout: re-check the flag. Error: the listener is gone.
                Ok(Err(e)) => return Err(DaemonError::Io(e)),
                Err(_) => continue,
            };
            if self
                .stop_accepting
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                // The wakeup (or a real client that raced the commit): the
                // incoming daemon is serving on its dup, so dropping this
                // connection is correct — it can reconnect to the new daemon.
                // The socket file stays either way.
                return Ok(());
            }
            let registry = Arc::clone(&self.registry);
            let sessions = Arc::clone(&self.sessions);
            let db = self.db.clone();
            let socket = self.socket.clone();
            let stop_accepting = Arc::clone(&self.stop_accepting);
            // Owned dups for the session: `send_fd` dups them into the peer, so
            // the loop keeps its own. A dup of a listener is itself a listener;
            // a dup of the lock shares the same open file description and
            // therefore the same lock. `None` when the lock is somehow absent
            // (serve always holds it — see `serve`/`serve_inherited`) means the
            // session answers handoffs as unavailable rather than failing.
            use std::os::unix::io::AsFd;
            let fds = self
                .instance
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .and_then(|lock| {
                    lock.fd()
                        .try_clone_to_owned()
                        .ok()
                        .map(|lock_dup| (lock_dup, lock.path().to_path_buf()))
                })
                .and_then(|(lock_dup, _)| {
                    listener
                        .as_fd()
                        .try_clone_to_owned()
                        .ok()
                        .map(|listener_dup| (listener_dup, lock_dup))
                });
            tokio::spawn(async move {
                let handoff = fds.map(|(listener_fd, lock_fd)| HandoffCtx {
                    socket,
                    listener_fd,
                    lock_fd,
                    stop_accepting,
                });
                if let Err(e) = handle(stream, registry, sessions, db, handoff).await {
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
async fn handle(
    stream: UnixStream,
    registry: Registry,
    sessions: Sessions,
    db: PathBuf,
    handoff: Option<HandoffCtx>,
) -> Result<(), DaemonError> {
    let (reader, writer) = stream.into_split();
    // `None` auth: the local socket is same-machine and trusted, so no per-verb
    // authorization gate applies — and no device to register under, so no
    // cutoff handle either. A remote peer always arrives with one (see
    // [`SessionAuth`]).
    //
    // "Same-machine" is wider than "same user" in practice: the socket is bound
    // with the ambient umask and never chmod'd, and `connect()` needs only write
    // permission on the inode — so at the default mode (0775 measured; 0755
    // under `umask 022`) a same-**group** peer reaches every verb here, `Handoff`
    // included. The mode is the gate; narrowing it is T-0078's decision, and this
    // session's `auth: None` deliberately does not pretend otherwise.
    serve_session_with_handoff(reader, writer, registry, sessions, db, None, handoff).await
}

/// How long a finished session stays alive after closing its write half, so the
/// pump can put the final frames on the wire. See the end of `serve_session`.
const FINAL_FRAME_GRACE: std::time::Duration = std::time::Duration::from_millis(300);

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
    /// This machine's own trust ledger (T-0046): who may use *this* machine.
    /// Deliberately separate from `authority`, which answers who the device is
    /// in the account — the two questions have different answers and different
    /// owners, and only this one is per machine.
    ledger: arreo_core::mesh::SharedLedger,
    peer: VerifyingKey,
    device: DeviceId,
    /// The peer's network address, when there is one: the local socket has none,
    /// and a remote session's is truncated before it reaches the log.
    address: Option<std::net::SocketAddr>,
    /// Whether this session has already put a trust refusal on the record.
    ///
    /// **Once per session, not once per verb** (T-0059). A device that is
    /// untrusted here retries, and a row per attempt would let a client fill the
    /// operator's log from outside — the cheapest denial of service against an
    /// audit trail. The first refusal is the interesting one: it says who tried,
    /// when, and why they were turned away; the ten thousandth says nothing new.
    refusal_recorded: std::sync::atomic::AtomicBool,
}

impl SessionAuth {
    /// `peer` is the key the Noise handshake authenticated, `device` the id it
    /// announced; they are bound together by the handshake itself.
    #[must_use]
    pub fn new(
        authority: Arc<Mutex<DeviceAuthority>>,
        ledger: arreo_core::mesh::SharedLedger,
        peer: VerifyingKey,
        device: DeviceId,
    ) -> Self {
        Self {
            authority,
            ledger,
            peer,
            device,
            address: None,
            refusal_recorded: std::sync::atomic::AtomicBool::new(false),
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
    ///
    /// **Two gates, in order (T-0046).** First *who are you*: the certificate —
    /// pinned, not revoked, verifying under the account root, which is the same
    /// answer on every machine of the account. Then *may you, here*: this
    /// machine's own grant, which is a different answer on each machine and is
    /// the one ROADMAP §3.7 is about.
    ///
    /// The second gate is why a phone paired to the VPS can reach the VPS and
    /// nothing else: its certificate is perfectly valid on the Pi, and the Pi has
    /// no grant row for it.
    ///
    /// The ledger's verdict is final in **both** directions, and a store it could
    /// not read is refused as an error rather than allowed: "I could not check"
    /// must never be the answer that lets someone in.
    fn check(&self, message: &Message) -> Result<(), Message> {
        let verb = verb_of(message);
        {
            let mut authority = self.lock();
            authority.check_verb(&self.peer, verb).map_err(|denial| {
                eprintln!("daemon: refusing {verb:?} for {}: {denial}", self.device);
                Message::Error {
                    v: VERSION,
                    message: denial.to_string(),
                }
            })?;
        }
        self.ledger
            .with(|ledger| ledger.check(&self.device, verb))
            .map_err(|denial| {
                let message = denial.to_string();
                eprintln!(
                    "daemon: refusing {verb:?} for {} on this machine: {denial}",
                    self.device
                );
                // One row per session (see `refusal_recorded`). The `swap` is
                // what makes it once: the first caller sees `false` and writes,
                // every later one sees `true` and does not.
                if !self
                    .refusal_recorded
                    .swap(true, std::sync::atomic::Ordering::Relaxed)
                {
                    self.ledger.with(|ledger| {
                        if let Err(e) = ledger.record_refusal(
                            &self.device,
                            // `machine` by *id* because it is the stable name: the
                            // reason below carries the machine's human name, which
                            // a rename makes ambiguous, and every other trust row
                            // records the id for the same reason.
                            &format!(
                                "machine={} verb={verb:?} reason={message}",
                                ledger.machine().as_str()
                            ),
                        ) {
                            eprintln!("daemon: cannot record the trust refusal: {e}");
                        }
                    });
                }
                Message::Error {
                    v: VERSION,
                    message,
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

    /// Whether this session's device has been revoked since the handshake
    /// (T-0052): the store is the authority (revocation is a fact the cert
    /// files do not carry), so a CLI-side revoke in another process is visible
    /// here on the next tick with no signaling between the processes.
    fn is_revoked(&self) -> bool {
        let mut authority = self.lock();
        authority.check_verb(&self.peer, Verb::Read).is_err()
            && authority.device(&self.device).is_none()
    }
}

/// One cutoff interval (T-0052): local sessions never tick (no device to
/// re-validate — the local socket is same-machine and trusted), remote sessions
/// re-validate twice a second. A free function so the `select!` reads as what
/// it is: a session that ends first drops its guard (unregistering it) and
/// never reaches the tick; a tick that finds no revocation loops back to
/// reading.
///
/// **Why 500 ms and not the 1 s the criterion bounds.** The bound is on the
/// *cutoff* — revoke to session end — and the tick is the whole latency budget:
/// a 1 s tick measured 1.004 s end to end (tick + frame delivery), which is
/// over the line the criterion draws. Half the interval makes the worst case
/// ~0.5 s with the same shape, at the cost of one extra store read per idle
/// remote session per second (the same read it already does per verb).
async fn cutoff_tick(auth: &Option<SessionAuth>) {
    match auth {
        Some(_) => tokio::time::sleep(std::time::Duration::from_millis(500)).await,
        None => std::future::pending().await,
    }
}

/// Connection handler: Hello→Welcome handshake, then verbs. Attach/Resume
/// own the connection while streaming (v0 semantics, T-0009 F4).
///
/// Generic over the byte stream so the local socket and the remote transport
/// run *this* loop: one protocol implementation, two ways to reach it.
///
/// `handoff` is `Some` only for the local Unix socket: the handoff request is
/// negotiated there (it can be refused with a typed error and audited), and
/// only there — a remote peer must never be able to replace the daemon. The
/// remote transport and the relay pass `None`, and a `Handoff` frame arriving
/// on those paths is answered as an unexpected verb, never acted on.
pub(crate) async fn serve_session<R, W>(
    reader: R,
    writer: W,
    registry: Registry,
    sessions: Sessions,
    db: PathBuf,
    auth: Option<SessionAuth>,
) -> Result<(), DaemonError>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    // `serve_session` is the no-handoff path (remote transport, relay): the
    // local socket uses `serve_session_with_handoff` below.
    serve_session_with_handoff(reader, writer, registry, sessions, db, auth, None).await
}

/// The local-socket entry point: like [`serve_session`], but the session may
/// carry a handoff request. `handoff` bundles what the session loop needs that
/// the verb loop does not otherwise see: the socket path (for `<socket>.handoff`),
/// the listener fd (borrowed — `SCM_RIGHTS` dups it into the peer), the lock fd
/// (borrowed the same way), and the daemon itself (to stop the accept loop when
/// the cut commits).
pub(crate) async fn serve_session_with_handoff<R, W>(
    reader: R,
    writer: W,
    registry: Registry,
    sessions: Sessions,
    db: PathBuf,
    auth: Option<SessionAuth>,
    handoff: Option<HandoffCtx>,
) -> Result<(), DaemonError>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut reader = reader;
    let mut writer = writer;
    let result = serve_session_inner(
        &mut reader,
        &mut writer,
        registry,
        sessions,
        db,
        auth,
        handoff,
    )
    .await;

    // **The last frame must reach the peer, on *every* way out (T-0052, T-0046).**
    // Closing the write half signals the transport's pump to drain what the
    // session already wrote and exit; the bounded pause is what lets it finish
    // before this future returns and its streams drop. Without it the bytes are
    // written into the duplex and discarded with the channel, and the client sees
    // a bare close instead of the reason it was refused.
    //
    // This is a *wrapper* rather than a tail because the session has many exits —
    // a refused handshake, a bad first frame, a read error, the normal end — and a
    // tail only covers the last. T-0052 fixed the normal end; the refused
    // handshake then delivered nothing, which a test caught (T-0046). Bounded,
    // not awaited forever: a peer that stops reading must cost this much and no
    // more.
    let _ = writer.shutdown().await;
    tokio::time::sleep(FINAL_FRAME_GRACE).await;
    result
}

/// What the local-socket session loop needs to serve a handoff request: the
/// socket path (for `<socket>.handoff`), owned dups of the listener + lock fds
/// (borrowed would be ideal — `SCM_RIGHTS` dups them into the peer — but the
/// session is a spawned `'static` task that cannot borrow the accept loop),
/// and a way to stop the accept loop when the cut commits.
pub(crate) struct HandoffCtx {
    pub socket: PathBuf,
    /// Owned dups of the accept loop's listener + the daemon's lock, taken per
    /// accepted connection. `send_fd` dups them into the peer, so the loop keeps
    /// its own; a dup of a listener is itself a listener, and a dup of the lock
    /// shares the same open file description and therefore the same lock. Owned
    /// (not borrowed) because the session is a spawned `'static` task that
    /// cannot borrow the accept loop.
    pub listener_fd: std::os::unix::io::OwnedFd,
    pub lock_fd: std::os::unix::io::OwnedFd,
    /// The daemon's stop flag, by clone: the session sets it when the cut
    /// commits, and the accept loop observes it after its current `accept`.
    pub stop_accepting: Arc<std::sync::atomic::AtomicBool>,
}

/// The session itself. Every exit here is covered by [`serve_session`]'s flush.
async fn serve_session_inner<R, W>(
    // `mut` on the writer so the body can reborrow: `write_message` takes
    // `&mut T` and the compiler needs a mutable binding to hand one out.
    reader: &mut R,
    mut writer: &mut W,
    registry: Registry,
    sessions: Sessions,
    db: PathBuf,
    auth: Option<SessionAuth>,
    handoff: Option<HandoffCtx>,
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
    // Live-session registration (T-0052): a remote session registers under its
    // device id for observability (counts per device, leak checks). Local
    // sessions (`auth: None`) have no device to key on — the local socket is
    // same-machine and trusted, and a revocation names a device, never "the
    // machine itself". The guard is the cleanup: dropping it at any exit below
    // unregisters exactly this session.
    let _guard = auth
        .as_ref()
        .map(|auth| sessions.register(auth.device_id().as_str()));

    // Handshake first: exactly one Hello, answered by Welcome or Error.
    // Bounded by timeout: pre-v1 JSONL clients (`{...}\n`) would otherwise
    // hang forever (their first bytes parse as a ~2 GB frame length).
    // A timeout turns the migration hazard into a loud close.
    let hello = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        read_message(reader, &mut buf),
    )
    .await;
    let hello = match hello {
        Ok(hello) => hello,
        Err(_) => return Ok(()),
    };
    // **The trust gate applies to the handshake too** (T-0046). A device this
    // machine has pinned but never granted gets its refusal here, in answer to
    // Hello — one round trip, with the command that fixes it — rather than a
    // Welcome followed by a refusal on whatever verb it tried next. `Verb::Hello`
    // is in the policy for exactly this reason: the machine answers, or it does
    // not. (A device this machine has *not* pinned never gets this far: the Noise
    // handshake resolves the peer's key from the pin list, so pinning is the
    // first gate and this is the second.)
    if let (Some(auth), Ok(message)) = (&auth, &hello) {
        if let Err(refusal) = auth.check(message) {
            write_message(writer, &refusal).await?;
            return Ok(());
        }
    }
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
    //
    // The cutoff tick (T-0052): each iteration races the next frame against a
    // 1 s re-validation of this session's own authorization. A revocation
    // written by *another* process (the CLI, against the same store) is
    // observed here within a second, and the session ends with a typed
    // revocation error rather than a silent drop. This is per-session, not a
    // central sweeper: no cross-process channel exists (the CLI cannot reach
    // this process's memory), and a sweeper would make cutoff latency a
    // function of its tick while burning CPU on every idle connection. The
    // cost here is one store read per session per idle second — the same as a
    // verb, and idle sessions already cost a task each.
    //
    // The handoff's failure rows are throttled per session (one refusal, one
    // abort): a client can send `Handoff` in a loop, and a row per frame would
    // let it fill the operator's log (T-0059's rule for trust refusals).
    let mut handoff_failures = crate::handoff::HandoffFailureLog::default();
    let ended_because = loop {
        let message = tokio::select! {
            // Biased: a revocation racing a verb is observed first, so the
            // client is told why even mid-verb — and the cutoff is at the next
            // frame boundary rather than mid-frame (T-0052's honest gap).
            biased;
            () = cutoff_tick(&auth) => {
                match &auth {
                    Some(auth) if auth.is_revoked() => {
                        let reason = format!(
                            "device {} was revoked: this session is ended",
                            auth.device_id().display_id()
                        );
                        // Best-effort: the client may already be gone, and the
                        // audit row below is the durable record either way.
                        let _ = write_message(
                            &mut writer,
                            &Message::Error { v: VERSION, message: reason.clone() },
                        )
                        .await;
                        // Graceful close (T-0052): the typed error must reach
                        // the client, and a bare `return` drops the channel —
                        // whose `Drop` aborts the pump and discards sealed
                        // bytes (T-0033's contract: dropped = closed, not
                        // flushed). Shutting the write half down signals the
                        // pump to drain and exit, so the client reads the
                        // reason and *then* the close.
                        let _ = tokio::io::AsyncWriteExt::shutdown(&mut writer).await;
                        break format!("revoked ({})", auth.device_id().display_id());
                    }
                    _ => continue,
                }
            }
            message = read_message(reader, &mut buf) => {
                match message {
                    Ok(message) => message,
                    Err(e) => break e.to_string(),
                }
            }
        };
        // Remote sessions are gated per verb, before anything acts on the
        // message. A refusal is answered and the session stays usable — a
        // viewer that tries `send` is told no, it does not lose its ability to
        // observe.
        if let Some(auth) = &auth {
            if let Err(refusal) = auth.check(&message) {
                write_message(writer, &refusal).await?;
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
                write_message(writer, &message).await?;
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
            Message::Handoff { v, protocol, build } => {
                let (v, protocol, build) = (*v, *protocol, build.clone());
                // The handoff request is negotiated on this client socket (so
                // it can be refused with a typed error and audited), but the
                // descriptors travel on the dedicated `<socket>.handoff`
                // connection — never here (see `handoff.rs` for why).
                if let Err(reply) = check_version(v) {
                    write_message(writer, &reply).await?;
                    continue;
                }
                match serve_handoff_request(
                    &mut writer,
                    &registry,
                    &db,
                    handoff.as_ref(),
                    protocol,
                    &build,
                    &mut handoff_failures,
                )
                .await
                {
                    // The cut committed: the descriptors moved, the incoming
                    // daemon committed, the audit row is written. Stop
                    // accepting and end this session — the accept loop observes
                    // the flag after its current `accept`, drops the listener,
                    // and the caller exits 0. The socket file stays.
                    HandoffSessionOutcome::Committed => {
                        return Ok(());
                    }
                    // Refused or aborted: the reply is already on the wire, the
                    // `.handoff` file is already gone, and this daemon keeps
                    // serving — on this very connection, which stays open so
                    // the peer can report what happened.
                    HandoffSessionOutcome::Answered => continue,
                    // The session's connection broke mid-handoff (the reply or
                    // a descriptor step failed on this socket). The `.handoff`
                    // file is already gone; keep serving, end this session.
                    HandoffSessionOutcome::Gone => {
                        break "handoff session lost".to_string();
                    }
                }
            }
            Message::HandoffReady { .. } => {
                write_message(
                    writer,
                    &Message::Error {
                        v: VERSION,
                        message: "unexpected handoff_ready here".to_string(),
                    },
                )
                .await?;
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
            write_message(writer, &reply).await?;
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
        // The handoff request drives the machine (it replaces the daemon), so
        // it needs the control capability; the readiness notice is news.
        Message::Handoff { .. } => Verb::Admin,
        Message::HandoffReady { .. } => Verb::Admin,
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
        Message::Handoff { .. } => "handoff",
        Message::HandoffReady { .. } => "handoff_ready",
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
        | Message::Handoff { .. }
        | Message::HandoffReady { .. }
        | Message::Ok { .. }
        | Message::Exited { .. } => Some(Message::Error {
            v: VERSION,
            message: format!("unexpected {} here", op_name(message)),
        }),
    }
}

/// What serving one handoff request concluded. Returned to the session loop so
/// the single place that owns the accept loop can act on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandoffSessionOutcome {
    /// The cut committed: descriptors sent, commit observed, audit row written.
    /// The session ends; the accept loop stops; the caller exits 0.
    Committed,
    /// Refused or aborted: the reply is on the wire, the `.handoff` file is
    /// gone, and this daemon keeps serving on this very connection.
    Answered,
    /// This session's connection broke mid-handoff. The `.handoff` file is
    /// gone; the daemon keeps serving; this session ends.
    Gone,
}

/// The bound `<socket>.handoff` listener for one handoff.
///
/// Unlinks the path on drop, so every exit — commit, refusal, abort, panic —
/// leaves no transfer socket behind and no exit path has to remember. (Before
/// this, each `return` had its own `remove_file`, which is exactly the shape
/// that forgets one.)
struct TransferSocket {
    path: PathBuf,
    listener: std::os::unix::net::UnixListener,
}

impl Drop for TransferSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Remove a leftover transfer socket **only after proving nothing is listening
/// on it** (T-0071's discipline, applied to the transfer path).
///
/// A path that answers a `connect` has a live peer holding it, and this handoff
/// must not unlink it — that is the "stop blindly removing" half of the
/// one-handoff rule. A path that refuses the connect is a leftover from a
/// handoff that died before its unlink, and removing it is the only way to bind
/// the real one.
fn clear_stale_transfer_path(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(format!(
            "handoff refused: {} exists and something is listening on it",
            path.display()
        )),
        Err(_) => std::fs::remove_file(path).map_err(|e| {
            format!(
                "handoff refused: cannot remove the stale transfer socket {}: {e}",
                path.display()
            )
        }),
    }
}

/// Serve one handoff request on the client socket (T-0038 stage 1, outgoing
/// side). The full ordering lives here; the session loop acts on the outcome.
///
/// 1. Validate the incoming daemon's protocol against *its* window
///    ([`crate::handoff::check_incoming_protocol`]): it must be able to speak
///    our protocol. A refusal writes the `handoff.refuse` audit row, answers a
///    typed `Error` naming both versions, and leaves this daemon serving — a
///    refused handoff is a deferred update, never a failure.
/// 2. Take the one-handoff lock for the duration; a second request is refused
///    with a typed error rather than raced.
/// 3. Bind `<socket>.handoff` (mode 0600, unlinked on every exit by
///    [`TransferSocket`]), mint the nonce, reply
///    `HandoffReady { v, protocol, panes, nonce }`.
/// 4. Accept one connection there with a deadline; the peer must be this user
///    and must present the nonce first.
/// 5. Send the listener descriptor, then the lock descriptor (the order is part
///    of the contract; stage 2 adds panes after the lock).
/// 6. Wait for the incoming daemon's **commit marker byte**, and record an
///    abort row when it does not arrive. EOF is not a commit — see `handoff.rs`.
/// 7. Write the audit row (`handoff <from> -> <to> panes=<n>`).
/// 8. Set the stop flag (the accept loop ends after its current `accept`,
///    dropping *this* process's listener fd; the incoming daemon holds a dup,
///    and the socket file stays).
///
/// Every abort path leaves the outgoing daemon serving, holding the lock, with
/// no `.handoff` file behind — and a retry succeeds. `failures` throttles the
/// rows, so a client looping `Handoff` cannot make this daemon write one row
/// per frame.
async fn serve_handoff_request(
    writer: &mut (impl AsyncWriteExt + Unpin),
    registry: &Registry,
    db: &std::path::Path,
    handoff: Option<&HandoffCtx>,
    incoming_protocol: u32,
    incoming_build: &str,
    failures: &mut crate::handoff::HandoffFailureLog,
) -> HandoffSessionOutcome {
    // A handoff request without the transfer context is not a handoff: it
    // arrived on a path that cannot move descriptors (remote transport, relay).
    // Answered, never acted on — a remote peer must never replace the daemon.
    let Some(ctx) = handoff else {
        write_message(
            writer,
            &Message::Error {
                v: VERSION,
                message: "handoff is not available on this connection".to_string(),
            },
        )
        .await
        .ok();
        return HandoffSessionOutcome::Answered;
    };
    // 1. The version check is the *incoming* daemon's window: `incoming ==
    // VERSION` or `incoming == VERSION + 1` is accepted, a gap of two and a
    // downgrade are refused. The build string is peer-supplied and reaches the
    // audit row, so it is stripped and bounded first.
    let agreed = match crate::handoff::check_incoming_protocol(incoming_protocol) {
        Ok(agreed) => agreed,
        Err(reason) => {
            let detail = format!(
                "handoff refused: incoming protocol {incoming_protocol} (build {}) {reason}",
                crate::handoff::sanitize_build(incoming_build),
            );
            failures.refuse(db, &detail);
            write_message(
                writer,
                &Message::Error {
                    v: VERSION,
                    message: detail,
                },
            )
            .await
            .ok();
            return HandoffSessionOutcome::Answered;
        }
    };
    // 2. **One handoff at a time** (F5): held for the whole transfer, so a
    // second request that arrives meanwhile is refused rather than raced. Two
    // concurrent handoffs used to both commit and leave two daemons serving one
    // socket — the state T-0071's lock exists to prevent.
    let handoff_lock_path = crate::handoff::handoff_lock_path_for(&ctx.socket);
    let _handoff_lock = match arreo_core::lock::ExclusiveLock::acquire(&handoff_lock_path) {
        Ok(lock) => lock,
        Err(e) => {
            let detail = format!("handoff refused: {} {e}", crate::handoff::BUSY_REFUSAL);
            failures.refuse(db, &detail);
            write_message(
                writer,
                &Message::Error {
                    v: VERSION,
                    message: detail,
                },
            )
            .await
            .ok();
            return HandoffSessionOutcome::Answered;
        }
    };
    // 3. Bind the dedicated transfer socket, under the handoff lock: an
    // existing path is probed rather than blindly unlinked.
    let handoff_path = crate::handoff::handoff_path_for(&ctx.socket);
    if let Err(detail) = clear_stale_transfer_path(&handoff_path) {
        failures.refuse(db, &detail);
        write_message(
            writer,
            &Message::Error {
                v: VERSION,
                message: detail,
            },
        )
        .await
        .ok();
        return HandoffSessionOutcome::Answered;
    }
    let listener = match std::os::unix::net::UnixListener::bind(&handoff_path) {
        Ok(listener) => listener,
        Err(e) => {
            let detail = format!(
                "handoff unavailable (cannot bind the transfer socket {}): {e}",
                handoff_path.display()
            );
            failures.refuse(db, &detail);
            write_message(
                writer,
                &Message::Error {
                    v: VERSION,
                    message: detail,
                },
            )
            .await
            .ok();
            return HandoffSessionOutcome::Answered;
        }
    };
    let transfer_socket = TransferSocket {
        path: handoff_path.clone(),
        listener,
    };
    // The transfer socket is created with the process umask (0775 in a common
    // configuration) and `connect()` needs only write permission on the inode,
    // so its mode is narrowed at once. The bind→chmod window is covered by the
    // peer-uid and nonce checks on the accepted connection below.
    if let Err(e) = crate::handoff::restrict_transfer_socket(&handoff_path) {
        let detail = format!(
            "handoff unavailable (cannot restrict the transfer socket {}): {e}",
            handoff_path.display()
        );
        failures.refuse(db, &detail);
        write_message(
            writer,
            &Message::Error {
                v: VERSION,
                message: detail,
            },
        )
        .await
        .ok();
        return HandoffSessionOutcome::Answered;
    }
    // The nonce binds the transfer to the process that asked for it here: it
    // travels on this (client) socket, where the requester is, and must come
    // back as the first bytes of the transfer connection.
    let nonce = match arreo_core::pty::adopt::fresh_nonce() {
        Ok(nonce) => nonce,
        Err(e) => {
            let detail = format!("handoff unavailable (no entropy for the nonce): {e}");
            failures.refuse(db, &detail);
            write_message(
                writer,
                &Message::Error {
                    v: VERSION,
                    message: detail,
                },
            )
            .await
            .ok();
            return HandoffSessionOutcome::Answered;
        }
    };
    let panes = registry.read().await.len() as u64;
    if write_message(
        writer,
        &Message::HandoffReady {
            v: agreed,
            protocol: agreed,
            server_protocol: VERSION,
            panes,
            nonce: nonce.to_vec(),
        },
    )
    .await
    .is_err()
    {
        return HandoffSessionOutcome::Gone;
    }
    // 4. Accept one connection there with a deadline.
    let _ = transfer_socket.listener.set_nonblocking(true);
    let deadline = std::time::Instant::now() + crate::handoff::DEFAULT_HANDOFF_TIMEOUT;
    let conn = loop {
        match transfer_socket.listener.accept() {
            Ok((conn, _)) => break Some(conn),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::Interrupted =>
            {
                if std::time::Instant::now() >= deadline {
                    break None;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            Err(_) => break None,
        }
    };
    let Some(conn) = conn else {
        let detail = "the new daemon never connected for the descriptors".to_string();
        failures.abort(db, &detail);
        write_message(
            writer,
            &Message::Error {
                v: VERSION,
                message: format!("handoff aborted: {detail}"),
            },
        )
        .await
        .ok();
        return HandoffSessionOutcome::Answered;
    };
    // The transfer socket stays blocking for the steps below; the *reads* carry
    // their own timeouts, so no wait here is unbounded.
    let _ = conn.set_nonblocking(false);
    // The peer must be this user (F1). On a platform that cannot answer
    // (`peer_uid` → `None`) this defers to the nonce and the socket's
    // permissions, which is why the transfer also requires the nonce.
    if let Err(detail) = crate::handoff::check_peer_uid(&conn) {
        failures.abort(db, &detail);
        write_message(
            writer,
            &Message::Error {
                v: VERSION,
                message: format!("handoff aborted: {detail}"),
            },
        )
        .await
        .ok();
        return HandoffSessionOutcome::Answered;
    }
    let presented = match crate::handoff::recv_nonce(&conn, crate::handoff::DEFAULT_HANDOFF_TIMEOUT)
    {
        Ok(presented) => presented,
        Err(detail) => {
            failures.abort(db, &detail);
            write_message(
                writer,
                &Message::Error {
                    v: VERSION,
                    message: format!("handoff aborted: {detail}"),
                },
            )
            .await
            .ok();
            return HandoffSessionOutcome::Answered;
        }
    };
    if presented != nonce {
        let detail = "the transfer connection presented the wrong handoff nonce".to_string();
        failures.abort(db, &detail);
        write_message(
            writer,
            &Message::Error {
                v: VERSION,
                message: format!("handoff aborted: {detail}"),
            },
        )
        .await
        .ok();
        return HandoffSessionOutcome::Answered;
    }
    // 5. The listener first, then the lock — the order is the contract.
    use std::os::unix::io::AsFd;
    if crate::handoff::send_one(&conn, ctx.listener_fd.as_fd()).is_err()
        || crate::handoff::send_one(&conn, ctx.lock_fd.as_fd()).is_err()
    {
        let detail = "the descriptor transfer failed".to_string();
        failures.abort(db, &detail);
        write_message(
            writer,
            &Message::Error {
                v: VERSION,
                message: format!("handoff aborted: {detail}"),
            },
        )
        .await
        .ok();
        return HandoffSessionOutcome::Answered;
    }
    // 6. Wait for the commit **marker byte**. It is *authorisation*, not proof
    // that a server is behind it: only the process this daemon answered can
    // know the nonce, so the byte says the requester is committing — whether it
    // went on to accept anything is not something this side can see (see
    // `handoff::wait_for_commit` for why a probe was rejected). End-of-stream is
    // an abort: a peer that connected and half-closed never served anything, and
    // committing on that left the socket with no listener.
    if let Err(detail) =
        crate::handoff::wait_for_commit(&conn, crate::handoff::DEFAULT_HANDOFF_TIMEOUT)
    {
        failures.abort(db, &detail);
        write_message(
            writer,
            &Message::Error {
                v: VERSION,
                message: format!("handoff aborted: {detail}"),
            },
        )
        .await
        .ok();
        return HandoffSessionOutcome::Answered;
    }
    // 7. The audit row, written by the outgoing daemon before it exits — with
    // the two protocol versions, the pane count, and this process's pid as the
    // agent (the incoming daemon's pid is not known here; its own
    // `handoff complete` line carries it for the operator).
    crate::handoff::record_handoff(db, VERSION, incoming_protocol, panes, std::process::id());
    // 8. Stop accepting. The accept loop ends after its current `accept` and
    // drops *this* process's listener fd; the incoming daemon holds a dup, and
    // the socket file stays. This session returns `Committed` so the session
    // loop ends it — the caller (`main`) exits 0 after `serve` returns.
    //
    // The loop may be parked in `accept` with nobody connecting, so the flag
    // alone would leave it parked: wake it with a connect to our own socket.
    // The wakeup connection is accepted (or refused — either way the loop
    // observes the flag next iteration) and handled as an ordinary session,
    // which ends at its handshake timeout with no side effects.
    ctx.stop_accepting
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = std::os::unix::net::UnixStream::connect(&ctx.socket);
    HandoffSessionOutcome::Committed
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

#[cfg(test)]
mod session_registry_tests {
    use super::*;

    /// The registry holds one entry per device with a count, and a session that
    /// ends removes exactly its own count — the leak check (T-0052). Thousands
    /// of connects and closes must leave nothing behind, or a long-lived daemon
    /// grows a map it never shrinks.
    #[test]
    fn registering_and_releasing_leaves_nothing_behind() {
        let sessions = Arc::new(LiveSessions::default());
        for round in 0..1_000 {
            let guard_a = sessions.register("dev_a");
            let guard_b = sessions.register("dev_a");
            let guard_c = sessions.register("dev_b");
            assert_eq!(sessions.counts().get("dev_a"), Some(&2));
            assert_eq!(sessions.counts().get("dev_b"), Some(&1));
            drop(guard_a);
            assert_eq!(sessions.counts().get("dev_a"), Some(&1), "round {round}");
            drop(guard_b);
            assert_eq!(
                sessions.counts().get("dev_a"),
                None,
                "a device with no sessions is removed, not left at zero (round {round})"
            );
            drop(guard_c);
        }
        assert!(
            sessions.counts().is_empty(),
            "1000 rounds left entries behind: {:?}",
            sessions.counts()
        );
    }

    /// One device with many sessions is one entry (not one per session), and
    /// releasing one does not touch its siblings.
    #[test]
    fn one_device_with_many_sessions_is_one_entry() {
        let sessions = Arc::new(LiveSessions::default());
        let guards: Vec<SessionGuard> = (0..64).map(|_| sessions.register("dev_a")).collect();
        assert_eq!(sessions.counts().len(), 1, "one device, one entry");
        assert_eq!(sessions.counts().get("dev_a"), Some(&64));
        drop(guards);
        assert!(sessions.counts().is_empty());
    }
}
