//! `arreo-tui` binary (T-0015): sidebar + pane wall over the daemon socket.
//!
//! Usage: `arreo-tui [--socket PATH]`. Polls panes+metrics every second,
//! streams the focused pane, quits on `q`/Esc/Ctrl-C. Mouse clicks focus.

use arreo_core::proto::{AgentState, Message, VERSION};
use arreo_tui::client::{default_socket, Client, PaneSummary};
use arreo_tui::model::PaneView;
use arreo_tui::ui::{App, ViewMode};
use crossterm::event::{self, Event, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::Stdout;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut socket: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1).peekable();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--socket" => socket = args.next().map(PathBuf::from),
            "--help" | "-h" => {
                println!("usage: arreo-tui [--socket PATH]");
                return Ok(());
            }
            other => {
                eprintln!("arreo-tui: unknown flag {other}");
                std::process::exit(2);
            }
        }
    }
    let socket = socket.unwrap_or_else(default_socket);

    enable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        event::EnableMouseCapture
    )?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;
    let result = run(&socket, &mut terminal).await;
    disable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    )?;
    result
}

/// One daemon snapshot delivered to the UI loop.
enum Poll {
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

async fn run(
    socket: &std::path::Path,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> anyhow::Result<()> {
    let mut app = App::new();
    // Daemon traffic lives in its own task: a slow socket must never delay
    // input. The UI loop only drains events and applies finished snapshots.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Poll>(8);
    let subscription = Arc::new(Mutex::new(Subscription::default()));
    let poller = tokio::spawn(poll_daemon(
        socket.to_path_buf(),
        tx,
        Arc::clone(&subscription),
    ));

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
                        })
                        .collect();
                    // Preserve scrollback lines across polls (merge by id).
                    merge_views(&mut app, views);
                    app.status = format!(
                        "{} panes · j/k move · Enter attach · w wall · / search · q quit",
                        summaries.len()
                    );
                }
                Poll::Panes(Err(e)) => {
                    app.status = format!("daemon unreachable: {e} (is arreo-server running?)");
                }
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
async fn poll_daemon(
    socket: std::path::PathBuf,
    tx: tokio::sync::mpsc::Sender<Poll>,
    subscription: Arc<Mutex<Subscription>>,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tick.tick().await;
        let summaries = poll_summaries(&socket).await.map_err(|e| e.to_string());
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
        for id in targets {
            let from_line = subscription
                .lock()
                .ok()
                .and_then(|slot| slot.cursors.get(&id).copied())
                .unwrap_or(0);
            if let Ok(Message::Delta {
                lines, from_line, ..
            }) = Client::request(
                &socket,
                &Message::Read {
                    v: VERSION,
                    id: id.clone(),
                    from_line,
                },
            )
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

/// Merge fresh summaries into the model, preserving scrollback lines.
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

async fn poll_summaries(socket: &std::path::Path) -> anyhow::Result<Vec<PaneSummary>> {
    let panes = match Client::request(
        socket,
        &Message::Panes {
            v: VERSION,
            panes: vec![],
        },
    )
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
        let ram_kb = match Client::request(
            socket,
            &Message::MetricsReq {
                v: VERSION,
                id: pane.id.clone(),
            },
        )
        .await
        {
            Ok(Message::Metrics { rss_bytes, .. }) => rss_bytes / 1024,
            _ => 0,
        };
        // State via non-blocking wait (timeout 0 would spin; use short wait
        // for question, else derive from liveness below).
        let state = state_for(socket, &pane.id, pane.alive).await;
        out.push(PaneSummary {
            id: pane.id,
            alive: pane.alive,
            state,
            ram_kb,
        });
    }
    // Attention order is the model's job; keep daemon order here.
    Ok(out)
}

/// Resolve one pane's state: ask the daemon to wait ~0 for each actionable
/// state in priority order (question → blocked → done), else liveness.
async fn state_for(socket: &std::path::Path, id: &str, alive: bool) -> AgentState {
    for want in [AgentState::Question, AgentState::Blocked] {
        if let Ok(Message::StateEvent { .. }) = Client::request(
            socket,
            &Message::Wait {
                v: VERSION,
                id: id.to_string(),
                state: want,
                timeout_ms: 150,
            },
        )
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
