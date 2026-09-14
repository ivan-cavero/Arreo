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

use arreo_core::identity::{DeviceId, Role};
use arreo_core::proto::{AgentState, Message, VERSION};
use arreo_core::theme::{Depth, Variant};
use arreo_tui::client::{default_socket, Client, PaneSummary, Target};
use arreo_tui::diff_view::{self, State};
use arreo_tui::exit;
use arreo_tui::fleet::{Code, Fleet, GrantPreview, Outcome};
use arreo_tui::model::PaneView;
use arreo_tui::settings;
use arreo_tui::theme::ThemeState;
use arreo_tui::ui::{Action, App, ViewMode};
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
    let mut attached_to: Option<String> = None;
    let mut config: Option<PathBuf> = None;
    let mut theme: Option<String> = None;
    let mut variant: Option<String> = None;
    let mut depth: Option<String> = None;
    // T-0073: the opt-in exit. `None` = nobody said, so the config file (if
    // any) is the answer; `Some(true)` = `--shutdown-on-exit`, `Some(false)` =
    // `--no-shutdown-on-exit`, which overrules a config that says true.
    let mut shutdown_flag: Option<bool> = None;
    let mut assume_yes = false;
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
            "--shutdown-on-exit" => shutdown_flag = Some(true),
            "--no-shutdown-on-exit" => shutdown_flag = Some(false),
            "--yes" => assume_yes = true,
            "--help" | "-h" => {
                println!(
                    "usage: arreo-tui [--socket PATH] [--theme NAME] \
                     [--variant dark|light] [--depth truecolor|256|16|none]"
                );
                println!("                 [--shutdown-on-exit | --no-shutdown-on-exit] [--yes]");
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
                println!();
                println!("the opt-in exit (default off: quitting leaves the daemon alone)");
                println!(
                    "  --shutdown-on-exit  quitting also drain-stops *this machine's* daemon:"
                );
                println!("             it flushes every pane's ring and checkpoints its topology,");
                println!(
                    "             then exits 0 — never a kill, never an unlink while serving."
                );
                println!(
                    "             Refused on a --machine/--remote target (the flag is about a"
                );
                println!("             local daemon). The same opt-in is tui.exit_kills_daemon in");
                println!("             the config file; the flag wins over the config.");
                println!(
                    "  --no-shutdown-on-exit  quit without stopping the daemon, even when the"
                );
                println!("             config says tui.exit_kills_daemon = true (this run only)");
                println!("  --yes      do not ask before stopping (for scripts). Otherwise live");
                println!("             panes, remote sessions and other attached clients open a");
                println!("             confirmation naming what would die.");
                println!();
                println!("keys (mouse works too; ? lists them on screen):");
                println!("  j/k ↑/↓     move the cursor          Enter  attach the pane");
                println!("  Tab         next pane                w      wall ↔ focus");
                println!(
                    "  d           diff the attached pane's worktree changes (d again closes)"
                );
                println!("  n/p · h/l   in the diff: next/previous file · scroll sideways");
                println!("  /           search the transcript    t      theme picker");
                println!("  [ ]         sidebar narrower/wider   PgUp/PgDn, Home/End  scroll");
                println!("  s           spawn an agent: <id> <program> [args…]");
                println!("  i           send text to the attached pane (audited as this device)");
                println!("  x           kill the attached pane (asks first, naming it)");
                println!("  m           machines: list · a add (pairing code + invite) ·");
                println!("              r rename · x remove (a machine that is online needs");
                println!("              the explicit force key) · g this machine's trust grants");
                println!("  g           trust: a grant a device (fingerprint confirmed) ·");
                println!("              x revoke (confirmed)");
                println!("  ?           the key list             q / Esc  quit");
                println!();
                println!("A viewer certificate renders send/spawn/kill disabled, with the reason,");
                println!("instead of failing after the keypress.");
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
    // The opt-in exit (T-0073), resolved **before** the target: a request that
    // cannot be honored must be refused by name rather than dialed past. The
    // flag wins over the config file, which is the same file the daemon reads
    // for its `[relay]` section (`tui.exit_kills_daemon`).
    let settings_depth = depth
        .as_deref()
        .and_then(parse_depth)
        .unwrap_or_else(Depth::detect);
    let (settings, settings_problem) = settings::resolve(config.as_deref(), settings_depth);
    let opt_in = match (shutdown_flag, settings.exit_kills_daemon) {
        (Some(true), _) => exit::OptIn::Flag,
        (Some(false), _) => exit::OptIn::Off,
        (None, true) => exit::OptIn::Config,
        (None, false) => exit::OptIn::Off,
    };
    // The flag and the config key both name *this machine's* daemon, and a TUI
    // attached to another machine must not try to stop it — but it must not
    // pretend nothing was asked either. Refused here, on the way in, because
    // the target's kind is already known from argv.
    if let Some(target) = remote_target(&machine, remote, peer.as_deref()) {
        if opt_in != exit::OptIn::Off {
            eprintln!("{}", exit::remote_refusal(opt_in, &target));
            std::process::exit(2);
        }
    }
    // `--yes` exists to skip the quit confirmation. With no opt-in there is no
    // stop and no question, and a script must not believe it said something.
    if assume_yes && opt_in == exit::OptIn::Off {
        eprintln!("{}", exit::yes_without_optin());
        std::process::exit(2);
    }
    // The trust ledger lives beside the *local* socket (trust is local, T-0059):
    // for a local target that is the socket this TUI reads, and for a remote one
    // it is the default socket — the ledger's home is this machine either way.
    let local_socket = socket.clone().unwrap_or_else(default_socket);
    let target = if let Some(name) = machine {
        match arreo_core::mesh::resolve::by_name(&name, config.as_deref()).await {
            Ok(resolved) => {
                session = format!("{} · {}", resolved.name, resolved.target.link());
                attached_to = Some(resolved.name.clone());
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
    // T-0074: this device's role, and the machine it is looking at when that is
    // not the local one. The role comes from the same certificate the daemon's
    // gate reads — `owner` on the local socket, where the daemon applies no gate
    // at all, and the certificate's own role on a remote target.
    let role = match &target {
        Target::Local(_) => Role::Owner,
        Target::Remote(remote) => remote.cert.role(),
    };
    let device = match &target {
        Target::Local(_) => None,
        Target::Remote(remote) => Some(remote.cert.device().clone()),
    };
    let fleet = Fleet {
        socket: local_socket.clone(),
        // One resolution of "which identity directory is this", the same value
        // `Layout::for_socket` and the pairing flow would read from the
        // environment — carried explicitly so a test can point a ledger at a
        // scratch directory without touching the process environment.
        identity_root: arreo_core::identity::identity_root(),
        config: config.clone(),
        attached_to,
    };
    let request = ThemeRequest {
        theme,
        variant: variant.as_deref().and_then(parse_variant),
        depth: depth.as_deref().and_then(parse_depth),
    };
    let exit_policy = ExitPolicy {
        shutdown: opt_in != exit::OptIn::Off,
        assume_yes,
    };
    let startup = Startup {
        settings,
        settings_problem,
        exit: exit_policy,
    };
    let identity = Identity { role, device };

    enable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        crossterm::terminal::EnterAlternateScreen,
        event::EnableMouseCapture
    )?;
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;
    let result = run(
        target,
        session,
        request,
        startup,
        fleet,
        identity,
        &mut terminal,
    )
    .await;
    disable_raw_mode()?;
    crossterm::execute!(
        std::io::stdout(),
        event::DisableMouseCapture,
        crossterm::terminal::LeaveAlternateScreen
    )?;
    let exit = result?;
    if exit == Exit::StopDaemon {
        // T-0073: the operator's one "I am done" gesture. Now that the TUI has
        // left the terminal and dropped its connection, hand the daemon the
        // graceful drain-stop (T-0012 semantics, through the CLI that owns that
        // path) and let its exit code be this process's.
        std::process::exit(exit::stop_local_daemon(&local_socket));
    }
    Ok(())
}

/// Enough of a device id to name it on screen without filling the sidebar.
fn short_id(id: &str) -> String {
    id.chars().take(8).collect()
}

/// One daemon snapshot delivered to the UI loop.
enum Poll {
    /// A connection state worth showing (reconnecting, and where it is trying).
    Status(String),
    /// The answer to a verb the UI asked to run (T-0074): the CLI's own line,
    /// which the status bar holds until the next keypress instead of letting
    /// the 1 Hz pane poll overwrite it before it is drawn.
    Result(String),
    /// Sidebar truth for one cycle (pane summaries, or why the cycle failed).
    Panes(Result<Vec<PaneSummary>, String>),
    /// Incremental scrollback for the focused pane (`from_line` = where the
    /// delta starts; 0 means the pane's history begins here).
    Delta {
        id: String,
        from_line: usize,
        lines: Vec<String>,
    },
    /// The answer to a diff read (T-0092): the pane it was for — so an answer
    /// that arrives after the operator moved on is dropped rather than painted —
    /// and what was read.
    Diff { pane: String, state: State },
    /// The answer to a fleet verb the UI asked for (T-0074): the CLI's code,
    /// the CLI's sentence, and the rows a panel renders. The action travels with
    /// it, so the UI knows which surface the answer belongs to without guessing
    /// from the shape of the rows.
    Fleet { action: Action, outcome: Outcome },
    /// A grant that cleared every check and now needs the human's word
    /// (T-0074's fingerprint confirmation). Boxed: a `GrantPreview` carries a
    /// device key and a rendered line, and the channel carries many polls.
    ConfirmGrant(Box<GrantPreview>),
}

/// A verb the UI wants run on the daemon connection the poller already holds
/// (T-0074).
///
/// **The held connection, deliberately.** A spawn/send/kill sent on a fresh
/// connection would be a second session for the same device on the remote
/// transport (the far end hands the new handshake to the old one, T-0054) and,
/// on any transport, a different audit attribution: the daemon records the
/// device that sent the verb, and the criterion says a send is audited *as the
/// device*. So the verbs ride the connection the sidebar is already reading.
enum Command {
    Spawn {
        id: String,
        program: String,
        args: Vec<String>,
    },
    Kill {
        id: String,
    },
    Send {
        id: String,
        text: String,
    },
}

/// Options from the command line that shape how this process renders (they
/// override detection).
#[derive(Debug, Default, Clone)]
struct ThemeRequest {
    theme: Option<String>,
    variant: Option<Variant>,
    depth: Option<Depth>,
}

/// The opt-in exit as this run resolved it (T-0073): the flag's answer over the
/// config file's, and whether the operator asked not to be prompted.
#[derive(Debug, Clone, Copy)]
struct ExitPolicy {
    /// Quitting stops the local daemon (never true on a remote target: that is
    /// refused on the way in).
    shutdown: bool,
    /// `--yes`: skip the quit confirmation.
    assume_yes: bool,
}

/// What the UI loop decided on its way out (T-0073).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Exit {
    /// Quit only: the daemon keeps serving (the default).
    Quit,
    /// Quit, then drain-stop the local daemon.
    StopDaemon,
}

/// Everything `main` resolved before the terminal was taken: what the config
/// file asked for, whether it could be read at all, and how this run exits.
///
/// One struct rather than four parameters because they are one fact — "how this
/// process was told to behave" — and they travel together from the argv loop
/// (which has to resolve the opt-in before the target is dialed) into `run`.
struct Startup {
    settings: settings::Settings,
    /// The status line for a config file that could not be used.
    settings_problem: Option<String>,
    exit: ExitPolicy,
}

/// What this device may do (T-0074): its role, and its id when it has one. Both
/// come from the same match on the target, so they travel together.
struct Identity {
    role: Role,
    device: Option<DeviceId>,
}

/// The target's human label when it is on another machine, for the opt-in's
/// refusal (T-0073). `None` for a local target — the only case the opt-in can
/// be honored in.
fn remote_target(
    machine: &Option<String>,
    remote: Option<SocketAddr>,
    peer: Option<&str>,
) -> Option<String> {
    if let Some(name) = machine.as_deref() {
        return Some(format!("another machine (--machine {name})"));
    }
    remote.map(|addr| match peer {
        Some(peer) => format!("another machine (--remote {addr}, peer {peer})"),
        None => format!("another machine (--remote {addr})"),
    })
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
    startup: Startup,
    fleet: Fleet,
    identity: Identity,
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
) -> anyhow::Result<Exit> {
    let mut app = App::new();
    // Where a diff comes from, resolved once (T-0092): this process's own
    // working directory — the CLI's `--repo`, which the TUI has no flag for, is
    // the directory the operator started it in — and the config file this run
    // reads, through the same precedence the settings below use.
    let dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let diff_config = settings::config_path(fleet.config.as_deref());
    // A pane on another machine is a worktree on another machine, and the diff
    // view says so instead of reading this machine's disk. `Target` carries no
    // name (it is a socket or an address), so the name is the one `main`
    // resolved for the sidebar: the `--machine` name, or that device's id when
    // the target was an address.
    let remote = match &target {
        Target::Local(_) => None,
        Target::Remote(_) => Some(session.clone()),
    };
    // Which machine, and over what (T-0061): shown in the sidebar once, not
    // repeated on every row — one sidebar is one machine's panes today.
    app.session = session;
    // What this device may do (T-0074). A viewer's control keys render disabled
    // with the daemon's own denial sentence rather than failing after the
    // keypress, and the local socket is always `owner` (no gate applies there).
    app.role = identity.role;
    app.device = identity.device;
    let depth = request.depth.unwrap_or_else(Depth::detect);
    app.theme = ThemeState::with_depth(depth, request.variant.unwrap_or_default());
    // `[tui]` settings (T-0076, T-0073): from the same config file the daemon
    // reads, plus what the terminal itself decides (NO_COLOR implies still).
    // Resolved by `main`, because the opt-in exit has to be known before the
    // target is dialed (a remote target refuses it).
    let Startup {
        settings,
        settings_problem,
        exit: exit_policy,
    } = startup;
    app.settings = settings;
    // The socket the daemon stop would be sent to (T-0073): the local one, and
    // only ever used when the opt-in was honored — which a remote target never
    // reaches.
    let local_socket = fleet.socket.clone();
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
    // Verbs the UI asks for go the other way, to the task that holds the
    // connection (T-0074): one connection, one audit attribution.
    let (commands, command_rx) = tokio::sync::mpsc::channel::<Command>(8);
    let subscription = Arc::new(Mutex::new(Subscription::default()));
    let poller = tokio::spawn(poll_daemon(
        target.clone(),
        tx.clone(),
        command_rx,
        Arc::clone(&subscription),
    ));

    let mut needs_draw = true;
    // The loop's value: whether it is leaving to stop the daemon (T-0073).
    // Every exit from it assigns, which is why this is a loop expression rather
    // than a flag initialized to a value nothing reads.
    let stop_daemon = 'ui: loop {
        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    // The previous answer's moment is over: the operator acted.
                    // Cleared *before* the key is dispatched, so a key whose own
                    // answer is a line — a declined confirmation (T-0073) — has
                    // something to keep.
                    app.result = None;
                    if !app.on_key(key.code) {
                        // `q`/Esc asked to quit. With the opt-in on, quitting
                        // also drain-stops the local daemon — behind the
                        // guards: nothing live means nothing to ask about, and
                        // anything live opens a confirmation that names it
                        // first (T-0073). `--yes` is the scripted "do not ask".
                        if exit_policy.shutdown && !exit_policy.assume_yes {
                            let doomed = exit::doomed(app.model.panes(), &local_socket);
                            if doomed.is_empty() {
                                break 'ui true;
                            }
                            app.confirm = Some(exit::shutdown_confirm(&doomed, &local_socket));
                        } else {
                            break 'ui exit_policy.shutdown;
                        }
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
                            alive: s.alive,
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
                Poll::Result(line) => {
                    // Held until the next keypress: a sentence the frame never
                    // painted was never a sentence at all.
                    app.result = Some(line);
                }
                Poll::Fleet { action, outcome } => app.apply_fleet(&action, outcome),
                Poll::ConfirmGrant(preview) => app.show_grant_confirm(*preview),
                // The answer belongs to a pane (T-0092): the view keeps it only
                // if that is still the pane it is showing.
                Poll::Diff { pane, state } => app.diff.apply(&pane, state),
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
        // Verbs the UI queued (T-0074). A daemon verb rides the connection the
        // poller holds; a fleet verb runs in its own task (the relay is a
        // network away, and `machines add` waits on a pairing mailbox), and its
        // answer comes back through the same channel as everything else.
        while let Some(action) = app.take_action() {
            match &action {
                Action::Spawn { id, program, args } => {
                    let _ = commands
                        .send(Command::Spawn {
                            id: id.clone(),
                            program: program.clone(),
                            args: args.clone(),
                        })
                        .await;
                }
                Action::Kill { id } => {
                    let _ = commands.send(Command::Kill { id: id.clone() }).await;
                }
                Action::Send { id, text } => {
                    let _ = commands
                        .send(Command::Send {
                            id: id.clone(),
                            text: text.clone(),
                        })
                        .await;
                }
                // The quit confirmation's `yes` (T-0073): leave the loop with
                // the stop armed. Nothing is sent to the daemon from here —
                // the drain-stop runs after the terminal is handed back.
                Action::QuitDaemon => break 'ui true,
                // The diff is read here and off the event loop (T-0092): `git`
                // spawns processes (the worktree's diff, its `ls-files`, one
                // more per untracked file) and the disk they read is *this*
                // machine's, which is why no daemon verb is involved. The
                // answer travels back on the same channel as everything else.
                Action::Diff { pane } => {
                    let request = diff_view::Request {
                        pane: pane.clone(),
                        dir: dir.clone(),
                        config: diff_config.clone(),
                        remote: remote.clone(),
                    };
                    let tx = tx.clone();
                    tokio::task::spawn_blocking(move || {
                        let state = diff_view::read(&request);
                        let _ = tx.blocking_send(Poll::Diff {
                            pane: request.pane,
                            state,
                        });
                    });
                }
                _ => {
                    let fleet = fleet.clone();
                    let tx = tx.clone();
                    let action = action.clone();
                    tokio::spawn(async move {
                        run_fleet_action(fleet, action, tx).await;
                    });
                }
            }
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
    };
    poller.abort();
    Ok(match stop_daemon {
        true => Exit::StopDaemon,
        false => Exit::Quit,
    })
}

/// One verb from the UI, run on the held connection (T-0074).
///
/// The lines are the CLI's own (`spawned <id>` on success; `spawn: <why>` when
/// the daemon refuses), because the criterion is that the honesty the CLI
/// already has is what the TUI shows: a refused spawn names why, and a kill of
/// a pane that is already gone says so.
async fn run_command(conn: &mut Client, command: Command) -> String {
    match command {
        Command::Spawn { id, program, args } => {
            let request = Message::Spawn {
                v: VERSION,
                id: id.clone(),
                program,
                args,
                cols: 80,
                rows: 24,
                memory_max: None,
                pids_max: None,
                kill_on_breach: false,
            };
            match conn.call(&request).await {
                Ok(Message::Ok { .. }) => format!("spawned {id}"),
                Ok(Message::Error { message, .. }) => format!("spawn: {message}"),
                Ok(other) => format!("spawn: unexpected {other:?}"),
                Err(e) => format!("spawn: {e}"),
            }
        }
        Command::Kill { id } => {
            match conn
                .call(&Message::Kill {
                    v: VERSION,
                    id: id.clone(),
                })
                .await
            {
                Ok(Message::Ok { .. }) => format!("killed {id}"),
                Ok(Message::Error { message, .. }) => format!("kill: {message}"),
                Ok(other) => format!("kill: unexpected {other:?}"),
                Err(e) => format!("kill: {e}"),
            }
        }
        Command::Send { id, text } => {
            match conn
                .call(&Message::Send {
                    v: VERSION,
                    id: id.clone(),
                    data: text,
                })
                .await
            {
                Ok(Message::Ok { .. }) => format!("sent to {id}"),
                Ok(Message::Error { message, .. }) => format!("send: {message}"),
                Ok(other) => format!("send: unexpected {other:?}"),
                Err(e) => format!("send: {e}"),
            }
        }
    }
}

/// Run one fleet verb and push its answer back to the UI (T-0074).
///
/// The trust verbs are synchronous (a local SQLite ledger, plus the authority
/// index for the pin check), so they run on the blocking pool; the directory
/// verbs are async because the relay is a network away. `machines add` makes its
/// own blocking hop for the pairing exchange, which waits on a mailbox.
async fn run_fleet_action(fleet: Fleet, action: Action, tx: tokio::sync::mpsc::Sender<Poll>) {
    let outcome = match &action {
        Action::MachinesList => Some(fleet.machines_list().await),
        Action::MachinesRename { from, to } => Some(fleet.machines_rename(from, to).await),
        Action::MachinesRemove { name, force } => Some(fleet.machines_remove(name, *force).await),
        Action::MachinesAdd { code, uri } => Some(fleet.machines_add(code, uri).await),
        Action::TrustList => {
            let fleet = fleet.clone();
            match tokio::task::spawn_blocking(move || fleet.trust_list()).await {
                Ok(outcome) => Some(outcome),
                Err(e) => Some(blocking_failed("machines trust", &e)),
            }
        }
        Action::TrustRevoke { device } => {
            let fleet = fleet.clone();
            let device = device.clone();
            match tokio::task::spawn_blocking(move || fleet.trust_revoke(&device)).await {
                Ok(outcome) => Some(outcome),
                Err(e) => Some(blocking_failed("devices revoke", &e)),
            }
        }
        Action::TrustPreview { device, role } => {
            let fleet = fleet.clone();
            let device = device.clone();
            let role = role.clone();
            match tokio::task::spawn_blocking(move || fleet.trust_preview(&device, &role)).await {
                Ok(Ok(preview)) => {
                    // Only a grant that cleared every check reaches the human,
                    // so the confirmation never asks about something that
                    // cannot work — the CLI's own order.
                    let _ = tx.send(Poll::ConfirmGrant(Box::new(preview))).await;
                    return;
                }
                Ok(Err(outcome)) => Some(outcome),
                Err(e) => Some(blocking_failed("machines trust", &e)),
            }
        }
        Action::TrustGrant { device, role } => {
            // The checks run again — they are cheap, and refusing a grant that
            // no longer clears them is the honest answer rather than writing it
            // blind.
            let fleet = fleet.clone();
            let device = device.clone();
            let role = role.clone();
            match tokio::task::spawn_blocking(move || {
                fleet
                    .trust_preview(&device, &role)
                    .map(|preview| fleet.trust_grant(&preview))
            })
            .await
            {
                Ok(Ok(outcome) | Err(outcome)) => Some(outcome),
                Err(e) => Some(blocking_failed("machines trust", &e)),
            }
        }
        // Verbs with no fleet answer: a spawn/kill/send rides the daemon
        // connection the poller holds, and a diff (T-0092) is read from this
        // machine's disk with its answer delivered as `Poll::Diff`.
        Action::Spawn { .. } | Action::Kill { .. } | Action::Send { .. } | Action::Diff { .. } => {
            None
        }
        // Answered by the event loop before it ever gets here (T-0073): a quit
        // is not a fleet verb.
        Action::QuitDaemon => None,
    };
    if let Some(outcome) = outcome {
        let _ = tx.send(Poll::Fleet { action, outcome }).await;
    }
}

/// A blocking-pool task that did not finish: the verb did not run, and saying so
/// is better than a silent no-op.
fn blocking_failed(verb: &str, e: &tokio::task::JoinError) -> Outcome {
    Outcome {
        code: Code::Failure,
        message: format!("{verb}: the task failed: {e}"),
        machines: Vec::new(),
        grants: Vec::new(),
    }
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
/// replays from exactly where the transcript stopped. **A pass on a fresh
/// connection (the first one, or a reconnect) runs immediately**, without
/// waiting for the next tick: a daemon handoff drops the held connection, the
/// next poll fails, and the re-subscribed `Read`s must reach the screen now —
/// waiting out the cadence would add another full second to the resume (measured
/// by the `reattach` slice; T-0038).
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
    mut commands: tokio::sync::mpsc::Receiver<Command>,
    subscription: Arc<Mutex<Subscription>>,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let mut attempt: u32 = 0;
    let mut conn: Option<Client> = None;
    loop {
        let mut just_connected = false;
        if conn.is_none() {
            match Client::connect_to(&target).await {
                Ok(opened) => {
                    conn = Some(opened);
                    just_connected = true;
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
        // A pass on a *fresh* connection runs immediately instead of waiting for
        // the next tick. The tick cadence is the sidebar's refresh rate, not a
        // pre-connect delay — and for a reconnect it is the whole difference
        // between a ~1 s resume and a ~2 s one: after a daemon handoff the old
        // connection dies and the next poll fails, the loop reconnects, and
        // without this the re-subscribed `Read`s would sit until the following
        // tick (up to another second) before the transcript it came back for
        // reaches the screen (measured by the `reattach` slice). The one-second
        // wait for the *next* pass then resumes as normal.
        if !just_connected {
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
                // A verb the UI asked for (T-0074): run it now, on the
                // connection this task holds, and answer with the CLI's line.
                // The next pass then runs immediately after it, so what the verb
                // changed is on the sidebar without waiting out the cadence.
                Some(command) = commands.recv() => {
                    let line = match conn.as_mut() {
                        Some(active) => run_command(active, command).await,
                        // No connection: the pass below will reconnect and say
                        // so; the verb is reported as unreachable rather than
                        // silently dropped.
                        None => "the daemon is not reachable yet; try again".to_string(),
                    };
                    if tx.send(Poll::Result(line)).await.is_err() {
                        return; // UI is gone.
                    }
                    continue;
                }
            }
        }
        let Some(active) = conn.as_mut() else {
            continue;
        };
        // One request for the whole wall (T-0079): the daemon tells this client
        // every pane's state, RAM and series, and falls back to asking pane by
        // pane only for a peer that sent no detail. See
        // `arreo_tui::client::poll_summaries` for why the sidebar must not ask.
        let summaries = arreo_tui::client::poll_summaries(active).await;
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
