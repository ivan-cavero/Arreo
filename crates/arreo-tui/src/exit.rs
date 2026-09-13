//! The TUI's way out (T-0073): quit, or quit *and* stop the local daemon.
//!
//! One sentence: the TUI is a client, not an owner (T-0015), so quitting it
//! leaves the daemon alone — and this module is the whole of the exception.
//! Three rules, all here because they are about the *exit* rather than about
//! the screen:
//!
//! - **Opt-in, default off.** `--shutdown-on-exit` (or `tui.exit_kills_daemon`
//!   in the config the daemon already reads) is the only way quitting stops
//!   anything; the flag wins over the file. The composition lives in
//!   [`crate::settings::shutdown_on_exit`].
//! - **Local only, and loudly.** The opt-in is about *this machine's* daemon.
//!   A TUI attached to another machine (`--machine`, `--remote`) refuses the
//!   opt-in by name ([`remote_refusal`]) instead of quietly not doing it: a
//!   request that cannot be honored is not the same as no request, and an
//!   operator who asked for a stop must not be told nothing.
//! - **Graceful, never a kill.** The stop is the CLI's own `server stop`
//!   ([`stop_local_daemon`]): SIGTERM, the daemon drains every pane's
//!   committed ring output, checkpoints its topology, removes the socket and
//!   exits 0 (T-0012). The TUI shells out rather than reimplementing it
//!   because the CLI is the single home of that path — the pid resolution, the
//!   drain wait and the socket-removal proof — and there is no daemon protocol
//!   verb for a shutdown. Never `SIGKILL`, and never an unlink while a daemon
//!   is still serving.
//!
//! **What a confirmation is for.** With live panes, remote sessions, or other
//! clients, quitting is destructive enough to ask: [`doomed`] gathers what this
//! TUI can see would be left behind, and [`shutdown_confirm`] names it. Two of
//! the three are observable from here and one is not, which is said out loud
//! rather than papered over: the daemon records a *remote* session (its device
//! id, in the audit ledger, `session.connect`/`session.disconnect`), and the
//! sidebar knows which panes are alive — but a *local* session is recorded
//! nowhere (the daemon keys sessions by device, and the local socket has no
//! device), so no client can count the other local clients attached to this
//! daemon. The confirmation says what stopping does to them; it does not invent
//! a number it cannot have.

use crate::model::PaneView;
use crate::ui::{Action, Confirm};
use arreo_core::identity::authority::sidecar_db;
use arreo_core::store::{actions, SessionStore};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Which source asked for the daemon stop, when one did (T-0073).
///
/// Carried rather than collapsed to a `bool` because the two sources are
/// refused and overruled by different words: a refusal for `--shutdown-on-exit`
/// tells the operator to drop the flag, and one for the config key tells them
/// about `--no-shutdown-on-exit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptIn {
    /// Nobody asked: quitting leaves the daemon alone. The default.
    Off,
    /// `--shutdown-on-exit`.
    Flag,
    /// `tui.exit_kills_daemon = true` in the config file.
    Config,
}

/// The loud refusal for an opt-in that meets a target on another machine.
///
/// A refusal, not a silent skip: the flag names *this machine's* daemon, and a
/// TUI looking at another machine's must not stop it — but neither may it
/// pretend the operator did not ask. The line names the source of the request
/// (so the fix is obvious), the target, and the way out.
#[must_use]
pub fn remote_refusal(opt_in: OptIn, target: &str) -> String {
    match opt_in {
        OptIn::Flag => format!(
            "arreo-tui: --shutdown-on-exit stops this machine's daemon, and this TUI is \
             attached to {target}. Quitting here must not stop that machine's daemon: drop \
             the flag (or pass --no-shutdown-on-exit) to quit this TUI and leave it running."
        ),
        OptIn::Config => format!(
            "arreo-tui: tui.exit_kills_daemon = true asks quitting to stop this machine's \
             daemon, and this TUI is attached to {target}. Quitting here must not stop that \
             machine's daemon: pass --no-shutdown-on-exit to override the config for this run."
        ),
        OptIn::Off => String::new(),
    }
}

/// The refusal for `--yes` with nothing for it to skip.
///
/// `--yes` means "do not ask me about the daemon stop". Without the opt-in
/// there is no daemon stop and no question, so the flag is inert — and an inert
/// flag is a script that believes it said something. Loud, at startup.
#[must_use]
pub fn yes_without_optin() -> String {
    "arreo-tui: --yes skips the quit confirmation, which only exists when quitting also stops \
     the daemon: pass --shutdown-on-exit (or set tui.exit_kills_daemon = true), or drop --yes."
        .to_string()
}

/// What a drain-stop would leave behind, as far as this TUI can see it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Doomed {
    /// Live panes, by id (a pane whose child already exited is not one).
    pub panes: Vec<String>,
    /// Remote sessions the daemon's own ledger records as still open, by
    /// device id.
    pub remote_sessions: Vec<String>,
}

impl Doomed {
    /// Nothing worth a confirmation: no pane, no remote session.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.panes.is_empty() && self.remote_sessions.is_empty()
    }
}

/// Gather what this TUI can see would be left behind (T-0073).
///
/// `panes` is the sidebar's own truth (the daemon told it which panes are
/// alive), and the remote sessions come from the daemon's audit ledger beside
/// the socket. Anything the ledger cannot answer is empty, never invented: the
/// confirmation is allowed to under-name what dies — it is not allowed to
/// name something that is not there.
#[must_use]
pub fn doomed(panes: &[PaneView], socket: &Path) -> Doomed {
    Doomed {
        panes: panes
            .iter()
            .filter(|pane| pane.alive)
            .map(|pane| pane.id.clone())
            .collect(),
        remote_sessions: remote_sessions(&sidecar_db(socket)),
    }
}

/// The devices with a live *remote* session, from the daemon's audit ledger.
///
/// A remote session writes `session.connect` when it opens and
/// `session.disconnect` when it ends (the daemon records both only for
/// authenticated sessions — see `serve_session_inner`), so the live set is the
/// per-device balance of the two. Ordered by device id so two runs render the
/// same sentence.
///
/// Bounded on purpose: the queries read at most [`AUDIT_SCAN`] rows each, which
/// is far past any real machine's retention (the daemon prunes the log). A
/// store that is absent, corrupt, or unreadable answers "none" — the stop is
/// still guarded by the panes, and a ledger read is not worth failing a quit
/// over.
fn remote_sessions(db: &Path) -> Vec<String> {
    if !db.exists() {
        return Vec::new();
    }
    let Ok(store) = SessionStore::open(db) else {
        return Vec::new();
    };
    let Ok(opened) = store.audit_by_action(actions::SESSION_CONNECT, AUDIT_SCAN) else {
        return Vec::new();
    };
    let Ok(closed) = store.audit_by_action(actions::SESSION_DISCONNECT, AUDIT_SCAN) else {
        return Vec::new();
    };
    let mut balance: HashMap<String, i64> = HashMap::new();
    for row in opened {
        *balance.entry(row.device).or_default() += 1;
    }
    for row in closed {
        *balance.entry(row.device).or_default() -= 1;
    }
    let mut live: Vec<String> = balance
        .into_iter()
        .filter(|(_, open)| *open > 0)
        .map(|(device, _)| device)
        .collect();
    live.sort();
    live
}

/// How many audit rows each side of the balance reads. Generous against a real
/// machine's retained log (the daemon prunes it), and finite so a huge ledger
/// cannot turn a quit into a scan.
const AUDIT_SCAN: usize = 10_000;

/// The confirmation for "quit and stop the daemon" (T-0073).
///
/// The lines are the truth about what the stop is and what it does: the drain
/// (rings flushed, topology checkpointed), the panes (they keep running with
/// nothing watching them — the daemon says so itself when it exits), and the
/// remote sessions that lose their daemon. It names what would die rather than
/// asking a bare "are you sure", because a confirmation that does not say what
/// it is about is one nobody can answer.
#[must_use]
pub fn shutdown_confirm(doomed: &Doomed, socket: &Path) -> Confirm {
    let mut lines = vec![
        format!(
            "quitting will drain-stop the daemon at {} (flush every pane's ring, checkpoint \
             its topology, exit 0).",
            socket.display()
        ),
        "the panes keep running, but nothing will be watching them; restart the daemon to \
         restore them from the checkpoint."
            .to_string(),
    ];
    if !doomed.panes.is_empty() {
        lines.push(format!(
            "live panes ({}): {}",
            doomed.panes.len(),
            doomed.panes.join(", ")
        ));
    }
    if !doomed.remote_sessions.is_empty() {
        lines.push(format!(
            "remote sessions ({}): {}",
            doomed.remote_sessions.len(),
            doomed.remote_sessions.join(", ")
        ));
        lines.push("those machines' clients lose this daemon.".to_string());
    }
    // The local clients this daemon cannot count (no device to key on): named
    // as a fact about the stop rather than as a number nobody has.
    lines.push("every other attached client loses its daemon connection.".to_string());
    Confirm {
        title: "stop this machine's daemon on exit?".to_string(),
        lines,
        yes: Action::QuitDaemon,
        force: None,
        cancel: Some("shutdown-on-exit: not confirmed; nothing changed".to_string()),
    }
}

/// Run the daemon's own graceful stop, on the operator's terminal.
///
/// The stop is the CLI's `server stop` (T-0012): SIGTERM to the daemon serving
/// `socket`, then a bounded wait for the socket file to disappear — the
/// daemon's last act, which is what proves the drain and the checkpoint ran
/// before the process left. Inherited stdio, so the CLI's own line
/// (`server stopped (pid N)`) is what the operator reads.
///
/// The binary is the sibling of this one (how `cargo` and every install layout
/// place `arreo` beside `arreo-tui`), falling back to `arreo` on `PATH`. A
/// binary that cannot be found or a stop that fails is loud and returns
/// non-zero: a quit that was asked to stop the daemon and did not is a failure,
/// never a quiet success.
#[must_use]
pub fn stop_local_daemon(socket: &Path) -> i32 {
    let mut command = std::process::Command::new(cli_binary());
    command.args(["server", "stop", "--socket"]).arg(socket);
    match command.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!(
                "arreo-tui: cannot stop the daemon at {}: {e}",
                socket.display()
            );
            1
        }
    }
}

/// The CLI binary to hand the stop to: `arreo` beside this executable, else
/// `arreo` from `PATH`.
fn cli_binary() -> PathBuf {
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("arreo")))
        .filter(|candidate| candidate.exists());
    sibling.unwrap_or_else(|| PathBuf::from("arreo"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arreo_core::store::{AuditEvent, AuditKind, AuditOutcome};

    fn pane(id: &str, alive: bool) -> PaneView {
        PaneView {
            id: id.to_string(),
            state: "idle",
            alive,
            ram_kb: 0,
            lines: Vec::new(),
            ram_history: Vec::new(),
            asking: None,
        }
    }

    /// The refusal is a refusal: it names the source of the request, the target,
    /// and the way out — and the two sources read differently, because the two
    /// fixes are different.
    #[test]
    fn a_remote_target_refuses_the_opt_in_by_name() {
        let flag = remote_refusal(OptIn::Flag, "another machine (--remote 10.0.0.2:443)");
        assert!(flag.contains("--shutdown-on-exit"), "{flag}");
        assert!(
            flag.contains("must not stop that machine's daemon"),
            "the refusal must say what it refuses: {flag}"
        );
        assert!(flag.contains("--no-shutdown-on-exit"), "{flag}");

        let config = remote_refusal(OptIn::Config, "another machine (--machine build)");
        assert!(config.contains("tui.exit_kills_daemon = true"), "{config}");
        assert!(config.contains("--machine build"), "{config}");
        assert_ne!(flag, config, "the two sources must not read the same");

        // `Off` is not a refusal and must not produce a sentence at all.
        assert!(remote_refusal(OptIn::Off, "another machine").is_empty());
    }

    /// `--yes` with nothing to skip is loud, not inert.
    #[test]
    fn yes_without_the_opt_in_says_so() {
        let line = yes_without_optin();
        assert!(line.contains("--yes"), "{line}");
        assert!(line.contains("--shutdown-on-exit"), "{line}");
    }

    /// The confirmation names what would die — and the local clients nobody can
    /// count are said as a fact about the stop, never as a number.
    #[test]
    fn the_confirmation_names_what_would_die() {
        let doomed = Doomed {
            panes: vec!["alpha".into(), "beta".into()],
            remote_sessions: vec!["dev_1".into()],
        };
        let confirm = shutdown_confirm(&doomed, Path::new("/run/arreo.sock"));
        let body = confirm.lines.join("\n");
        assert!(body.contains("live panes (2): alpha, beta"), "{body}");
        assert!(body.contains("remote sessions (1): dev_1"), "{body}");
        assert!(body.contains("/run/arreo.sock"), "{body}");
        assert!(
            body.contains("every other attached client loses its daemon connection"),
            "{body}"
        );
        assert_eq!(confirm.yes, Action::QuitDaemon);
        assert_eq!(
            confirm.cancel.as_deref(),
            Some("shutdown-on-exit: not confirmed; nothing changed"),
            "the declined line is the CLI's own shape"
        );

        // Nothing doomed: the panes and sessions lines are absent, not zeroed.
        let bare = shutdown_confirm(&Doomed::default(), Path::new("/run/arreo.sock"));
        let body = bare.lines.join("\n");
        assert!(!body.contains("live panes (0)"), "{body}");
        assert!(!body.contains("remote sessions (0)"), "{body}");
    }

    /// A dead pane is not something a drain-stop takes: the guard is about the
    /// panes that would be left running, and a pane whose child exited is not
    /// one of them.
    #[test]
    fn only_live_panes_are_doomed() {
        let panes = [pane("alpha", true), pane("gone", false)];
        let doomed = Doomed {
            panes: panes
                .iter()
                .filter(|p| p.alive)
                .map(|p| p.id.clone())
                .collect(),
            remote_sessions: Vec::new(),
        };
        assert_eq!(doomed.panes, vec!["alpha".to_string()]);
        assert!(!doomed.is_empty());

        let all_dead = [pane("gone", false)];
        let doomed = Doomed {
            panes: all_dead
                .iter()
                .filter(|p| p.alive)
                .map(|p| p.id.clone())
                .collect(),
            remote_sessions: Vec::new(),
        };
        assert!(doomed.is_empty(), "nothing live means nothing to confirm");
    }

    /// The live remote sessions are the per-device balance of the daemon's own
    /// connect/disconnect rows — a device that connected twice and left once is
    /// still there, one that left as often as it came is not.
    #[test]
    fn remote_sessions_are_the_audit_balance() {
        let path = std::env::temp_dir().join(format!(
            "arreo-tui-exit-balance-{}-{:?}.db",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&path);
        {
            let store = SessionStore::open(&path).expect("store");
            let record = |device: &str, action: &str| {
                // The kind the daemon itself writes for a session row:
                // `Unknown` (sessions have no older kind; the `action` is what
                // readers use).
                let mut event = AuditEvent::new(action, AuditKind::Unknown, AuditOutcome::Ok, 0);
                event.device = device.to_string();
                store.record(&event).expect("record");
            };
            record("dev_open", actions::SESSION_CONNECT);
            record("dev_closed", actions::SESSION_CONNECT);
            record("dev_closed", actions::SESSION_DISCONNECT);
            record("dev_twice", actions::SESSION_CONNECT);
            record("dev_twice", actions::SESSION_CONNECT);
            record("dev_twice", actions::SESSION_DISCONNECT);
        }

        assert_eq!(
            remote_sessions(&path),
            vec!["dev_open".to_string(), "dev_twice".to_string()],
            "the live set is who has an open session, not who ever connected"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A store that is not there answers "no remote sessions" — and does not
    /// create one as a side effect of quitting.
    #[test]
    fn an_absent_ledger_is_not_a_session() {
        let missing = std::env::temp_dir().join(format!(
            "arreo-tui-exit-missing-{}-{:?}.db",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_file(&missing);
        assert!(remote_sessions(&missing).is_empty());
        assert!(!missing.exists(), "reading must not create the store");
    }
}
