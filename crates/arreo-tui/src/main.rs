//! `arreo-tui` binary (T-0015): sidebar + pane wall over the daemon socket, or
//! over the relay to a daemon on another machine (T-0032).
//!
//! Usage: `arreo-tui [--socket PATH]` or `arreo-tui --remote <addr> --peer
//! <device-id> --account <acct>`. Polls panes+metrics every second, streams the
//! focused pane, quits on `q`/Esc/Ctrl-C. Mouse clicks focus.
//!
//! **The remote case is the same client.** Every verb below is the same
//! `Message`, the same framing and the same resume cursor whether the bytes came
//! from a Unix socket or from the relay; [`Target`] is the only thing that
//! differs, and the reconnect loop is transport-blind. A drop is a reconnect
//! with backoff, never a lost session: the pane cursors live in the UI's
//! subscription, so a reconnect resumes exactly where the transcript stopped
//! (`Read { from_line }` replays from the cursor, and the transcript is
//! byte-identical to a run that never dropped).

use arreo_core::proto::{AgentState, Message, VERSION};
use arreo_core::theme::{Depth, Variant};
use arreo_tui::client::{default_socket, Client, PaneSummary, Target};
use arreo_tui::model::PaneView;
use arreo_tui::settings;
use arreo_tui::theme::ThemeState;
use arreo_tui::ui::{App, ViewMode};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::Stdout;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut socket: Option<PathBuf> = None;
    let mut remote: Option<SocketAddr> = None;
    let mut peer: Option<String> = None;
    let mut account: Option<String> = None;
    let mut identity: Option<PathBuf> = None;
    let mut machine: Option<String> = None;
    let mut config: Option<PathBuf> = None;
    let mut theme: Option<String> = None;
    let mut variant: Option<String> = None;
    let mut depth: Option<String> = None;
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = args.next().map(PathBuf::from),
            "--remote" => {
                remote = match args.next().map(|addr| addr.parse()) {
                    Some(Ok(addr)) => Some(addr),
                    Some(Err(e)) => {
                        eprintln!("arreo-tui: --remote wants a relay address: {e}");
                        std::process::exit(2);
                    }
                    None => {
                        eprintln!("arreo-tui: --remote wants an address (host:port)");
                        std::process::exit(2);
                    }
                }
            }
            "--peer" => peer = args.next(),
            "--account" => account = args.next(),
            "--identity" => identity = args.next().map(PathBuf::from),
            "--machine" => machine = args.next(),
            "--config" => config = args.next().map(PathBuf::from),
            "--theme" => theme = args.next(),
            "--variant" => variant = args.next(),
            "--depth" => depth = args.next(),
            "--help" | "-h" => {
                println!(
                    "usage: arreo-tui [--socket PATH] [--theme NAME] \
                     [--variant dark|light] [--depth truecolor|256|16|none]"
                );
                println!("       arreo-tui --machine NAME [--config PATH]   (a machine, by name)");
                println!(
                    "       arreo-tui --remote HOST:PORT --peer DEVICE_ID [--account A] \
                     [--identity DIR]"
                );
                println!("  --machine  another machine, by the name the account's directory holds");
                println!("             (the same name `arreo attach --machine` takes; nothing is");
                println!("             dialed from argv — the name resolves through the relay)");
                println!("  --config   the config whose [relay] section names the account");
                println!("  --remote  a daemon on another machine, at a known address");
                println!("  --peer    that machine's device id (as `arreo devices list` shows it)");
                println!("  --identity  this device's identity dir (default: the standard one)");
                return Ok(());
            }
            other => {
                eprintln!("arreo-tui: unknown flag {other}");
                std::process::exit(2);
            }
        }
    }
    if machine.is_some() && (remote.is_some() || peer.is_some() || socket.is_some()) {
        eprintln!(
            "arreo-tui: --machine names a machine; it cannot be combined with --remote/--peer/\
             --socket (those are addresses, and mixing them would leave it ambiguous which one \
             was obeyed)"
        );
        std::process::exit(2);
    }
    // What this TUI is looking at, said once and shown in the sidebar (T-0061). A
    // local session says so rather than naming a machine: `Target` does not know a
    // name, and only a resolution does — so a name here is one that was resolved,
    // never one that was assumed.
    let mut session = "this machine · socket".to_string();
    let target = if let Some(name) = machine {
        match arreo_core::mesh::resolve::by_name(&name, config.as_deref()).await {
            Ok(resolved) => {
                session = format!("{} · {}", resolved.name, resolved.target.link());
                resolved.target
            }
            Err(e) => {
                eprintln!("arreo-tui: --machine {name}: {}", e.message());
                std::process::exit(2);
            }
        }
    } else {
        match (remote, &peer) {
            (Some(relay), Some(peer)) => {
                let root = identity.unwrap_or_else(arreo_core::identity::identity_root);
                let Some(account) = account.or_else(|| std::env::var("ARREO_ACCOUNT").ok()) else {
                    eprintln!(
                    "arreo-tui: --account is required for a remote target (or set ARREO_ACCOUNT)"
                );
                    eprintln!(
                        "  the account is the relay-side tenant both machines are registered under"
                    );
                    std::process::exit(2);
                };
                match Target::remote(relay, &account, peer, &root) {
                    Ok(target) => {
                        // Addressed by device id, so the label says so: a name here
                        // would be one this process never resolved, and `Target` holds
                        // no name to give (see its `link`). Eight characters is enough
                        // to tell two machines apart on screen and to paste elsewhere.
                        session = format!("device {} · relay", short_id(peer));
                        target
                    }
                    Err(e) => {
                        eprintln!("arreo-tui: {e}");
                        eprintln!(
                            "  a remote target needs this device paired with that machine \
                         (`arreo pair --join`), which writes device.key, the certificate and \
                         the pinned server key into {}",
                            root.display()
                        );
                        std::process::exit(2);
                    }
                }
            }
            // A peer without a relay is a typo, not a default: silently attaching to
            // the local daemon would show the user the wrong machine's panes.
            (None, Some(_)) => {
                eprintln!("arreo-tui: --peer needs --remote HOST:PORT");
                std::process::exit(2);
            }
            (Some(_), None) => {
                eprintln!("arreo-tui: --remote needs --peer DEVICE_ID");
                std::process::exit(2);
            }
            (None, None) => Target::Local(socket.unwrap_or_else(default_socket)),
        }
    };
    let request = ThemeRequest {
        theme,
        variant: variant.as_deref().and_then(parse_variant),
        depth: depth.as_deref().and_then(parse_depth),
        config,
    };

    enable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        event::EnableMouseCapture
    )?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;
    let result = run(target, session, request, &mut terminal).await;
    disable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    )?;
    result
}

/// Enough of a device id to name it on screen without filling the sidebar.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// One daemon snapshot delivered to the UI loop.
enum Poll {
    /// A connection state worth showing (reconnecting, and where it is trying).
    Status(String),
    /// Sidebar truth for one cycle (pane summaries, or why the cycle failed).
    Panes(Result<Vec<PaneSummary>, String>),
    /// Incremental scrollback for the focused pane (`from_line` = where the
    /// delta starts; 0 means the pane's history begins here).
    Delta {
        id: String,
        from_line: usize,
        lines: Vec<String>,
    },
}

/// Options from the command line that shape how this process renders (they
/// override detection).
#[derive(Debug, Default, Clone)]
struct ThemeRequest {
    theme: Option<String>,
    variant: Option<Variant>,
    depth: Option<Depth>,
    /// `--config PATH` (T-0076): the same file the daemon reads, for its `[tui]`
    /// section. `$ARREO_CONFIG` is the fallback, resolved by `settings`.
    config: Option<PathBuf>,
}

fn parse_variant(raw: &str) -> Option<Variant> {
    match raw.to_ascii_lowercase().as_str() {
        "dark" => Some(Variant::Dark),
        "light" => Some(Variant::Light),
        other => {
            eprintln!("arreo-tui: unknown variant {other:?} (dark|light)");
            None
        }
    }
}

fn parse_depth(raw: &str) -> Option<Depth> {
    match raw.to_ascii_lowercase().as_str() {
        "truecolor" | "24bit" => Some(Depth::Truecolor),
        "256" | "ansi256" => Some(Depth::Ansi256),
        "16" | "ansi16" => Some(Depth::Ansi16),
        "none" | "nocolor" => Some(Depth::NoColor),
        other => {
            eprintln!("arreo-tui: unknown depth {other:?} (truecolor|256|16|none)");
            None
        }
    }
}

async fn run(
    target: Target,
    session: String,
    request: ThemeRequest,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> anyhow::Result<()> {
    let mut app = App::new();
    // Which machine, and over what (T-0061): shown in the sidebar once, not
    // repeated on every row — one sidebar is one machine's panes today.
    app.session = session;
    let depth = request.depth.unwrap_or_else(Depth::detect);
    app.theme = ThemeState::with_depth(depth, request.variant.unwrap_or_default());
    // `[tui]` settings (T-0076): from the same config file the daemon reads,
    // plus what the terminal itself decides (NO_COLOR implies still).
    let (settings, settings_problem) = settings::resolve(request.config.as_deref(), depth);
    app.settings = settings;
    if let Some(name) = request.theme.as_deref() {
        if let Err(e) = app.theme.select(name) {
            // A bad --theme is worth saying out loud, not silently ignoring.
            app.status = format!("theme {name:?}: {e}");
        }
    } else if let Some(problem) = app.theme.startup_error() {
        // Same rule for a broken user theme shadowing the default.
        app.status = problem.to_string();
    }
    // A config the user pointed at that we could not use is the same kind of
    // fact, and it outranks nothing else — it is appended, not overwritten.
    if let Some(problem) = settings_problem {
        app.status = match app.status.is_empty() {
            true => problem,
            false => format!("{} · {problem}", app.status),
        };
    }
    // Daemon traffic lives in its own task: a slow socket must never delay
    // input. The UI loop only drains events and applies finished snapshots.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Poll>(8);
    let subscription = Arc::new(Mutex::new(Subscription::default()));
    let poller = tokio::spawn(poll_daemon(target.clone(), tx, Arc::clone(&subscription)));

    let mut needs_draw = true;
    loop {
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if !app.on_key(key.code) {
                        break;
                    }
                    needs_draw = true;
                }
                Event::Mouse(mouse) => {
                    use crossterm::event::{MouseButton, MouseEventKind};
                    match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            app.on_click(mouse.column, mouse.row);
                            needs_draw = true;
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            app.on_drag(mouse.column);
                            needs_draw = true;
                        }
                        MouseEventKind::Up(MouseButton::Left) => app.on_drag_end(),
                        MouseEventKind::ScrollDown => {
                            app.on_scroll(1);
                            needs_draw = true;
                        }
                        MouseEventKind::ScrollUp => {
                            app.on_scroll(-1);
                            needs_draw = true;
                        }
                        _ => {}
                    }
                }
                Event::Resize(_, _) => needs_draw = true,
                _ => {}
            }
        }
        while let Ok(poll) = rx.try_recv() {
            match poll {
                Poll::Panes(Ok(summaries)) => {
                    let views: Vec<PaneView> = summaries
                        .iter()
                        .map(|s| PaneView {
                            id: s.id.clone(),
                            state: state_name(&s.state),
                            ram_kb: s.ram_kb,
                            lines: Vec::new(),
                            ram_history: s.ram_history.clone(),
                            asking: s.asking.clone(),
                        })
                        .collect();
                    // Preserve scrollback lines across polls (merge by id).
                    merge_views(&mut app, views);
                    // The key legend is the status renderer's job (it knows the
                    // width, and a legend that clips mid-word teaches nothing).
                    app.status = format!("{} panes", summaries.len());
                }
                Poll::Panes(Err(e)) => {
                    app.status = format!("daemon unreachable: {e}");
                }
                Poll::Status(line) => app.status = line,
                Poll::Delta {
                    id,
                    from_line,
                    lines,
                } => {
                    if let Some(pane) = app.model.panes_mut().iter_mut().find(|p| p.id == id) {
                        if from_line == 0 {
                            // Fresh subscription: replace, don't stack history.
                            pane.lines.clear();
                        }
                        pane.lines.extend(lines);
                        if pane.lines.len() > SCROLLBACK_CAP {
                            let drop = pane.lines.len() - SCROLLBACK_CAP;
                            pane.lines.drain(..drop);
                        }
                    }
                }
            }
            needs_draw = true;
        }
        // Tell the poller what to stream: the focused pane, or every pane
        // while the wall is up (the wall is only honest if its tiles are).
        if let Ok(mut slot) = subscription.lock() {
            let focus = app.model.focused_id().map(str::to_string);
            let wall = app.view == ViewMode::Wall;
            if slot.focus != focus || slot.wall != wall {
                slot.focus = focus;
                slot.wall = wall;
                if !slot.wall {
                    // Leaving the wall drops every cursor but the focused one.
                    let keep = slot.focus.clone();
                    slot.cursors.retain(|id, _| Some(id) == keep.as_ref());
                }
            }
        }
        if needs_draw {
            terminal.draw(|frame| app.render(frame))?;
            needs_draw = false;
        }
    }
    poller.abort();
    Ok(())
}

/// Pane scrollback kept in the view (mirrors the daemon's ring, which is the
/// source of truth — this only bounds the TUI's own memory).
const SCROLLBACK_CAP: usize = 512;

/// How many panes the wall streams at once. Past this the sidebar is the
/// overview; unbounded streaming would be a bandwidth bill, not a feature.
const WALL_STREAM_MAX: usize = 9;

/// What the UI wants streamed, plus the per-pane read cursors the poller
/// maintains (the UI never touches the cursors).
#[derive(Default)]
struct Subscription {
    focus: Option<String>,
    wall: bool,
    cursors: std::collections::HashMap<String, usize>,
}

/// Daemon-facing loop: sidebar truth once a second, then one incremental
/// `Read` per subscribed pane, pushed to the UI over a channel.
///
/// **One connection, held.** The verbs of a pass share it, and it outlives the
/// pass: a *remote* connection is a session with a peer device, and the far end
/// multiplexes one stream per peer — opening a second session for the same
/// device while the first is still held does not create a second stream, it
/// hands the new handshake to the old one. So the connection is long-lived and
/// the loop is a reconnect loop, not a connect-per-pass loop.
///
/// **A failure is a reconnect, never a lost transcript.** The backoff is
/// [`arreo_core::relay::session::backoff_delay`]'s (exponential, capped at 30 s,
/// jittered), the attempt counter resets on the first pass that answers, and the
/// UI is told what is happening instead of being shown a frozen screen. The pane
/// cursors live in the subscription and are *not* reset, so a resumed read
/// replays from exactly where the transcript stopped.
///
/// **The drop case is bounded by the far end, not by this loop.** A client that
/// vanishes is not noticed by the daemon until one of its reads or writes fails,
/// and the relay does not tell a device that its peer went away (T-0054), so a
/// reconnect may have to wait for the daemon to give up the old stream. The loop
/// keeps trying — it never gives up while the TUI is open — and says so in the
/// status bar.
async fn poll_daemon(
    target: Target,
    tx: tokio::sync::mpsc::Sender<Poll>,
    subscription: Arc<Mutex<Subscription>>,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut attempt: u32 = 0;
    let mut conn: Option<Client> = None;
    loop {
        if conn.is_none() {
            match Client::connect_to(&target).await {
                Ok(opened) => {
                    conn = Some(opened);
                    let _ = tx
                        .send(Poll::Status(format!("connected to {}", target.describe())))
                        .await;
                }
                Err(e) => {
                    let delay = report_disconnected(&tx, &target, attempt, &e).await;
                    tokio::time::sleep(delay).await;
                    attempt = attempt.saturating_add(1);
                    continue;
                }
            }
        }
        // The session can also die *between* verbs, which on a remote target is
        // the difference between a one-second stall and an honest status line —
        // so the wait for the next tick races the session's own closure signal.
        let closed = conn.as_ref().and_then(Client::closed);
        let next_tick = tick.tick();
        let drop_notice = async {
            match &closed {
                Some(closed) => closed.wait().await,
                // Local connections report a closure by failing the next read.
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            _ = next_tick => {}
            () = drop_notice => {
                let delay = report_closed(&tx, &target, attempt).await;
                conn = None;
                tokio::time::sleep(delay).await;
                attempt = attempt.saturating_add(1);
                continue;
            }
        }
        let Some(active) = conn.as_mut() else {
            continue;
        };
        let summaries = poll_summaries(active).await.map_err(|e| e.to_string());
        // A pass that answered is the thing the attempt counter measures: a
        // connection that opens and then refuses every verb (a role refusal, a
        // peer that is not serving) is not healthy, and backing off is the
        // honest response rather than a one-second retry loop.
        let answered = summaries.is_ok();
        if answered {
            attempt = 0;
        } else {
            attempt = attempt.saturating_add(1);
            conn = None;
        }
        let ids: Vec<String> = match &summaries {
            Ok(summaries) => summaries.iter().map(|s| s.id.clone()).collect(),
            Err(_) => Vec::new(),
        };
        if tx.send(Poll::Panes(summaries)).await.is_err() {
            return; // UI is gone.
        }
        let targets = match subscription.lock() {
            Ok(mut slot) => {
                let mut targets: Vec<String> = Vec::new();
                if slot.wall {
                    targets.extend(ids.iter().take(WALL_STREAM_MAX).cloned());
                } else if let Some(focus) = slot.focus.clone() {
                    targets.push(focus);
                }
                // A pane nobody watches loses its cursor; a new subscription
                // starts at the beginning of the ring.
                slot.cursors.retain(|id, _| targets.contains(id));
                for id in &targets {
                    slot.cursors.entry(id.clone()).or_insert(0);
                }
                targets
            }
            Err(_) => Vec::new(),
        };
        let Some(active) = conn.as_mut() else {
            continue;
        };
        for id in targets {
            let from_line = subscription
                .lock()
                .ok()
                .and_then(|slot| slot.cursors.get(&id).copied())
                .unwrap_or(0);
            if let Ok(Message::Delta {
                lines, from_line, ..
            }) = active
                .call(&Message::Read {
                    v: VERSION,
                    id: id.clone(),
                    from_line,
                })
                .await
            {
                if let Ok(mut slot) = subscription.lock() {
                    slot.cursors.insert(id.clone(), from_line + lines.len());
                }
                if tx
                    .send(Poll::Delta {
                        id,
                        from_line,
                        lines,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
    }
}

/// Tell the UI the connection is gone and how long until the next attempt, and
/// return that delay.
///
/// The backoff is the relay session's own policy — one schedule for a daemon
/// reconnecting to a relay and a client reconnecting to a daemon, because they
/// are the same event: "the carrier went away".
async fn report_disconnected(
    tx: &tokio::sync::mpsc::Sender<Poll>,
    target: &Target,
    attempt: u32,
    error: &arreo_tui::client::ClientError,
) -> Duration {
    let delay = arreo_core::relay::session::backoff_delay(attempt, jitter());
    let line = if matches!(target, Target::Local(_)) {
        format!("daemon unreachable: {error}")
    } else {
        format!(
            "reconnecting to {} (attempt {}, next in {:.1}s): {error}",
            target.describe(),
            attempt + 1,
            delay.as_secs_f64()
        )
    };
    let _ = tx.send(Poll::Status(line)).await;
    delay
}

/// The same, for a session that ended between two passes (the idle drop).
async fn report_closed(
    tx: &tokio::sync::mpsc::Sender<Poll>,
    target: &Target,
    attempt: u32,
) -> Duration {
    let delay = arreo_core::relay::session::backoff_delay(attempt, jitter());
    let _ = tx
        .send(Poll::Status(format!(
            "reconnecting to {} (attempt {}, next in {:.1}s): the session closed",
            target.describe(),
            attempt + 1,
            delay.as_secs_f64()
        )))
        .await;
    delay
}

/// A jitter fraction in `[0, 1)`, from the clock.
///
/// Same reasoning as the daemon's: jitter is a spread problem, not a secrecy
/// one, so distinct processes starting at distinct nanoseconds is the whole
/// requirement and needs no dependency.
fn jitter() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| f64::from(d.subsec_nanos()) / 1e9)
        .unwrap_or(0.0)
}

fn state_name(state: &AgentState) -> &'static str {
    match state {
        AgentState::Unknown => "unknown",
        AgentState::Working => "working",
        AgentState::Idle => "idle",
        AgentState::Question => "question",
        AgentState::Blocked => "blocked",
        AgentState::Done => "done",
    }
}

/// Merge fresh summaries into the model, preserving scrollback lines (and the
/// sparkline history, which arrives with the summary).
fn merge_views(app: &mut App, views: Vec<PaneView>) {
    let mut old_lines: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for pane in app.model.panes() {
        old_lines.insert(pane.id.clone(), pane.lines.clone());
    }
    let mut merged = views;
    for pane in &mut merged {
        if let Some(lines) = old_lines.remove(&pane.id) {
            pane.lines = lines;
        }
    }
    app.model.set_panes(merged);
}

async fn poll_summaries(conn: &mut Client) -> anyhow::Result<Vec<PaneSummary>> {
    let panes = match conn
        .call(&Message::Panes {
            v: VERSION,
            panes: vec![],
        })
        .await
    {
        Ok(Message::Panes { panes, .. }) => panes,
        Ok(Message::Error { message, .. }) => anyhow::bail!("panes: {message}"),
        Ok(other) => anyhow::bail!("panes: unexpected {other:?}"),
        Err(e) => anyhow::bail!("{e}"),
    };
    let mut out = Vec::new();
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
        // best-effort like ram_kb, empty when the daemon has no series yet.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let ram_history = match conn
            .call(&Message::MetricsHistory {
                v: VERSION,
                id: pane.id.clone(),
                since_ms: now_ms.saturating_sub(3_600_000),
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
        // State via non-blocking wait (timeout 0 would spin; use short wait
        // for question, else derive from liveness below).
        let state = state_for(conn, &pane.id, pane.alive).await;
        // **What it is asking, for a pane that is asking (T-0061).** Fetched only
        // in that state, so an ordinary cycle costs exactly what it cost before.
        // `Read` is a snapshot of the pane's hot ring (`HOT_LINES`), not a
        // consuming read — several readers see the same lines, which is why the
        // focused pane's attach and this cannot steal from each other.
        let asking = if state == AgentState::Question {
            arreo_tui::client::asking_line(conn, &pane.id).await
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

/// Resolve one pane's state: ask the daemon to wait ~0 for each actionable
/// state in priority order (question → blocked → done), else liveness.
async fn state_for(conn: &mut Client, id: &str, alive: bool) -> AgentState {
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
