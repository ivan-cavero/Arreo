//! T-0032 relay slice: the real `arreo-tui` binary drives a pane on another
//! machine, through a real relay, on a real pty.
//!
//! No mocks: a real `arreo-relay` process serving a real QUIC listener, a real
//! `arreo-server` on the "peer" machine connected to it, a real second identity
//! for the client, and the real TUI — the same binary a user runs — given
//! `--remote`, on a pty, driven with real key events. Assertions are on the
//! reconstructed screen (what a human would see) plus the peer's own audit trail
//! (who the daemon thinks acted).
//!
//! `--interactive-evidence` writes the per-step screens to
//! `.loop/evidence/T-0032/` — the frames a reviewer reads.

use crate::harness::{render_screen, wait_bound, TuiSession};
use arreo_core::identity::{DeviceCert, DeviceKey, Role, RootKey};
use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

/// The name the peer machine claims in the account's directory. Reaching it by
/// this name — rather than by address — is the T-0045 path, and the name is what
/// T-0061's sidebar must report.
const PEER_NAME: &str = "peer-machine";

/// The pty size the harness uses; the same numbers go to the screen renderer.
const ROWS: usize = 30;
const COLS: usize = 120;

/// A process whose stderr is drained for its whole life.
///
/// The drain is not tidiness: a closed pipe makes the child die on its next log
/// line (EPIPE), which reads exactly like an authentication failure.
struct Proc {
    child: Child,
    log: Arc<Mutex<String>>,
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Proc {
    fn spawn(command: &mut Command, watch: Option<&str>) -> (Self, Option<SocketAddr>) {
        command.stdout(Stdio::null()).stderr(Stdio::piped());
        let mut child = command.spawn().expect("the process starts");
        let stderr = child.stderr.take().expect("stderr");
        let log = Arc::new(Mutex::new(String::new()));
        let (ready_tx, ready_rx) = mpsc::channel();
        {
            let log = Arc::clone(&log);
            let watch = watch.map(str::to_string);
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    if let Some(watch) = &watch {
                        if let Some(rest) = line.split(watch.as_str()).nth(1) {
                            if let Some(addr) = rest.split_whitespace().next() {
                                if let Ok(addr) = addr.parse::<SocketAddr>() {
                                    let _ = ready_tx.send(addr);
                                }
                            }
                        }
                    }
                    if let Ok(mut held) = log.lock() {
                        held.push_str(&line);
                        held.push('\n');
                    }
                }
            });
        }
        let addr = watch.map(|_| {
            ready_rx
                .recv_timeout(Duration::from_secs(30))
                .expect("the process announces its address")
        });
        (Self { child, log }, addr)
    }

    fn log_text(&self) -> String {
        self.log.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn await_log(&self, needle: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(30);
        while Instant::now() < deadline {
            if self.log_text().contains(needle) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }
}

fn hex(bytes: [u8; 32]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn debug_bin(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join("target")
        .join("debug")
        .join(name)
}

/// Wait for `needle` on the TUI's screen, sampling the painted output.
fn wait_screen(session: &TuiSession, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if session.screen().contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Is `needle` on an indented sidebar line — near the left edge of the frame?
///
/// The question text also appears in the focused pane's view on the right, so a
/// bare `contains` could not tell "the sidebar shows the question" from "the
/// terminal shows the question". The sidebar is the leftmost column, so the line
/// that holds it starts within a few characters of the frame's left edge.
fn sidebar_holds(screen: &str, needle: &str) -> bool {
    screen.split('\n').any(|line| {
        line.find(needle)
            .is_some_and(|at| line[..at].chars().take_while(|c| *c == ' ').count() <= 8)
    })
}

/// Is `needle` in the pane view — right of the sidebar's edge, so not the sidebar's
/// own copy of the same text?
///
/// The edge is read off the frame (the sidebar's top-right corner) rather than
/// assumed: the sidebar is resizable, and a hardcoded column would quietly stop
/// meaning anything the day the default width changed.
///
/// Compared in **columns, not bytes**: the frame is box-drawing characters, three
/// bytes each, so byte offsets from two different lines are not comparable — the
/// first version of this compared them and reported a pane-view hit as a sidebar
/// one.
fn right_region_holds(screen: &str, needle: &str) -> bool {
    let Some(edge) = screen
        .lines()
        .find_map(|line| line.find('┐').map(|at| line[..at].chars().count()))
    else {
        return false;
    };
    screen.lines().any(|line| {
        line.find(needle)
            .is_some_and(|at| line[..at].chars().count() > edge)
    })
}

/// Write the painted screen, for a reviewer.
fn frame(dir: &Path, name: &str, session: &TuiSession) {
    let raw = session.transcript();
    let screen = render_screen(&String::from_utf8_lossy(&raw), ROWS, COLS);
    let _ = std::fs::write(dir.join(format!("{name}.txt")), screen);
}

pub fn run(rest: &[String]) -> ExitCode {
    let evidence = rest.iter().any(|a| a == "--interactive-evidence");
    let evidence_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .join(".loop")
        .join("evidence")
        .join("T-0032");
    if evidence {
        let _ = std::fs::create_dir_all(&evidence_dir);
    }

    let (server_bin, cli_bin, tui_bin) = crate::harness::bins();
    let relay_bin = debug_bin("arreo-relay");
    for bin in [&server_bin, &cli_bin, &tui_bin, &relay_bin] {
        if !bin.exists() {
            println!(
                "[FAIL] relay: missing binary {} (build first: `cargo build --workspace`)",
                bin.display()
            );
            return ExitCode::FAILURE;
        }
    }

    let mut failures = 0usize;
    let mut passes = 0usize;
    let mut check = |name: &str, ok: bool, detail: &str| {
        if ok {
            println!("[PASS] relay: {name}");
            passes += 1;
        } else {
            println!("[FAIL] relay: {name}: {detail}");
            failures += 1;
        }
    };

    let base = std::env::temp_dir().join(format!("arreo-e2e-relay-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    if let Err(e) = std::fs::create_dir_all(&base) {
        println!("[FAIL] relay: scratch dir: {e}");
        return ExitCode::FAILURE;
    }

    // ---- the relay, and the account both machines live in ----
    let state_dir = base.join("relay-state");
    let _ = std::fs::create_dir_all(&state_dir);
    let mut relay_cmd = Command::new(&relay_bin);
    relay_cmd.args([
        "serve",
        "--listen",
        "127.0.0.1:0",
        "--state-dir",
        &state_dir.display().to_string(),
    ]);
    let (relay, relay_addr) = Proc::spawn(&mut relay_cmd, Some("router on "));
    let Some(relay_addr) = relay_addr else {
        println!("[FAIL] relay: the relay never announced its address");
        return ExitCode::FAILURE;
    };

    // ---- two machines under one account root (what a real deployment pairs) ----
    let root = RootKey::generate().expect("entropy");
    let account = "acct-e2e";
    let registered = Command::new(&relay_bin)
        .args([
            "account",
            "add",
            "--state-dir",
            &state_dir.display().to_string(),
            "--account",
            account,
            "--root-key",
            &hex(root.public().to_bytes()),
        ])
        .output()
        .expect("the account command runs");
    if !registered.status.success() {
        println!(
            "[FAIL] relay: registering the account: {}",
            String::from_utf8_lossy(&registered.stderr)
        );
        return ExitCode::FAILURE;
    }

    // The peer machine: identity directory, key, certificate.
    let peer_dir = base.join("peer");
    let peer_identity = peer_dir.join("identity");
    let _ = std::fs::create_dir_all(peer_identity.join("devices"));
    let peer_key = DeviceKey::generate().expect("entropy");
    let peer_cert = DeviceCert::issue(&root, &peer_key.public(), "peer", Role::Owner, 1_000, 1);
    root.save(&peer_identity.join("root.key"))
        .expect("root key");
    peer_key
        .save(&peer_identity.join("device.key"))
        .expect("peer key");
    peer_cert
        .save(&peer_identity.join("devices"))
        .expect("peer certificate");

    // The client machine: its own key, its certificate, and the peer's key
    // pinned — exactly what `arreo pair --join` writes on the joining side.
    let client_dir = base.join("client");
    let client_identity = client_dir.join("identity");
    let _ = std::fs::create_dir_all(client_identity.join("devices"));
    let client_key = DeviceKey::generate().expect("entropy");
    let client_cert = DeviceCert::issue(&root, &client_key.public(), "tui", Role::Owner, 1_000, 2);
    client_key
        .save(&client_identity.join("device.key"))
        .expect("client key");
    client_cert
        .save(&client_identity.join("devices"))
        .expect("client certificate");
    std::fs::write(
        client_identity.join("server.key"),
        format!("{}\n", hex(peer_key.public().to_bytes())),
    )
    .expect("pinned server key");

    // The peer must have the client pinned *before* its daemon starts: the
    // authority's index is what the handshake resolves against.
    let peer_socket = peer_dir.join("peer.sock");
    {
        let mut cmd = Command::new(&server_bin);
        cmd.arg("--socket").arg(&peer_socket);
        cmd.env("ARREO_IDENTITY_DIR", &peer_dir);
        let (daemon, _) = Proc::spawn(&mut cmd, None);
        if !daemon.await_log("serving on") {
            println!("[FAIL] relay: the peer daemon never bound its socket");
            return ExitCode::FAILURE;
        }
        let pinned = Command::new(&cli_bin)
            .args([
                "devices",
                "issue",
                "--socket",
                &peer_socket.display().to_string(),
                "--name",
                "tui",
                "--role",
                "owner",
                "--key",
                &hex(client_key.public().to_bytes()),
            ])
            .env("ARREO_IDENTITY_DIR", &peer_dir)
            .output()
            .expect("the pin command runs");
        check(
            "the client device is pinned on the peer machine",
            pinned.status.success(),
            &String::from_utf8_lossy(&pinned.stderr),
        );
    }

    // The peer's daemon, now connected to the relay (the route the client needs).
    let config = peer_dir.join("peer.toml");
    // Named, so this machine can be reached *by name* as well as by address
    // (T-0061): the name is what a person says, and the sidebar has to say it back.
    std::fs::write(
        &config,
        format!(
            "[relay]\nenabled = true\naddr = \"{relay_addr}\"\naccount = \"{account}\"\n\
             name = \"{PEER_NAME}\"\n"
        ),
    )
    .expect("config");
    let mut peer_cmd = Command::new(&server_bin);
    peer_cmd
        .arg("--socket")
        .arg(&peer_socket)
        .arg("--config")
        .arg(&config)
        .env("ARREO_IDENTITY_DIR", &peer_dir);
    let (peer, _) = Proc::spawn(&mut peer_cmd, None);
    let served = peer.await_log("serving on");
    let on_relay = peer.await_log("relay session up");
    check(
        "the peer daemon is serving and connected to the relay",
        served && on_relay,
        &peer.log_text(),
    );
    wait_bound(&peer_socket);

    // A pane on the peer machine, whose output is the marker the TUI must show.
    let marker = "REMOTE-MACHINE-MARKER-7c31";
    let spawned = Command::new(&cli_bin)
        .args([
            "spawn",
            "remote-pane",
            "/bin/sh",
            "-c",
            &format!("echo {marker}; echo second-line; sleep 300"),
            "--socket",
            &peer_socket.display().to_string(),
        ])
        .env("ARREO_IDENTITY_DIR", &peer_dir)
        .output()
        .expect("the spawn command runs");
    check(
        "the peer machine owns a pane",
        spawned.status.success(),
        &String::from_utf8_lossy(&spawned.stderr),
    );

    // ---- the TUI, on a pty, pointed at the other machine ----
    let remote_args = [
        "--remote",
        &relay_addr.to_string(),
        "--peer",
        &peer_cert.device().display_id(),
        "--account",
        account,
        "--identity",
        &client_identity.display().to_string(),
    ];
    let args: Vec<&str> = remote_args.to_vec();
    let Some(mut tui) = TuiSession::start_with(&tui_bin, &peer_socket, &args, &[]) else {
        println!("[FAIL] relay: the TUI did not start on a pty");
        return ExitCode::FAILURE;
    };
    if evidence {
        frame(&evidence_dir, "01-connected", &tui);
    }

    // The sidebar lists the *remote* machine's pane, and its scrollback carries
    // the marker — which crossed the relay as ciphertext and was decrypted by
    // the peer's own daemon.
    let listed = wait_screen(&tui, "remote-pane", Duration::from_secs(30));
    check(
        "the TUI lists the remote machine's pane",
        listed,
        &tui.screen(),
    );
    if evidence {
        frame(&evidence_dir, "02-remote-pane-listed", &tui);
    }

    // Focus it and attach: real key events through the real binary. The clock
    // starts at the keypress, so the measurement is "how long until the user
    // sees the other machine's output" and not "how long the process took to
    // start".
    tui.send("j");
    std::thread::sleep(Duration::from_millis(300));
    let attached_at = Instant::now();
    tui.send("\r");
    let attached = wait_screen(&tui, marker, Duration::from_secs(30));
    let attach_ms = attached_at.elapsed().as_millis() as u64;
    check(
        "attaching streams the remote pane's output",
        attached,
        &tui.screen(),
    );

    // §5's `cross_machine_attach_s`, enforced against the budget file rather
    // than a constant copied here: `bench` cannot measure this row (it needs a
    // relay and two daemons, which is not a load measurement), so this slice is
    // where the law is applied.
    match crate::bench::budget_target("cross_machine_attach_s") {
        Ok(target_s) => {
            let budget_ms = target_s * 1000;
            check(
                "the cross-machine attach meets its §5 budget",
                attach_ms <= budget_ms,
                &format!("took {attach_ms} ms, budget {budget_ms} ms"),
            );
            println!("relay: cross-machine attach {attach_ms} ms (budget {budget_ms} ms)");
        }
        Err(e) => check("the cross-machine attach meets its §5 budget", false, &e),
    }
    if evidence {
        frame(&evidence_dir, "03-attached-remote-output", &tui);
    }

    // The whole scrollback crossed the relay, not just the first line: the pane
    // wrote two lines and the attached view must hold both.
    check(
        "the remote pane's whole scrollback arrived",
        tui.screen().contains("second-line"),
        &tui.screen(),
    );

    // Attribution: the peer's own audit log names the client device, and the
    // attach it served is on record.
    let audit = Command::new(&cli_bin)
        .args(["audit", "--limit", "50", "--socket"])
        .arg(&peer_socket)
        .env("ARREO_IDENTITY_DIR", &peer_dir)
        .output()
        .expect("audit runs");
    let audit = String::from_utf8_lossy(&audit.stdout).to_string();
    check(
        "the peer's audit trail names the client device",
        audit.contains(&client_cert.device().display_id()),
        &audit,
    );
    // The session is on record. The TUI's "attach" is its focused-pane
    // streaming, which it does with `Read{from_line}` — a read, and reads are
    // deliberately not audited (a trail that logs every poll is a trail nobody
    // reads, T-0033). So the attach appears here as the *absence* of a row plus
    // the session that carried it, which is the honest statement.
    check(
        "the peer recorded the session that carried the attach",
        audit.contains("session.connect"),
        &audit,
    );
    check(
        "no keystroke was audited as input",
        !audit.contains(" send "),
        &audit,
    );
    if evidence {
        let _ = std::fs::write(evidence_dir.join("04-peer-audit.txt"), &audit);
    }

    // The relay carried ciphertext: the marker is in neither its state nor its
    // log, while the TUI is displaying it.
    let mut leaked = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&state_dir) {
        for entry in entries.flatten() {
            if let Ok(bytes) = std::fs::read(entry.path()) {
                if bytes.windows(marker.len()).any(|w| w == marker.as_bytes()) {
                    leaked.push(entry.path());
                }
            }
        }
    }
    check(
        "the relay holds no pane content",
        leaked.is_empty(),
        &format!("{leaked:?}"),
    );
    check(
        "the relay's log holds no pane content",
        !relay.log_text().contains(marker),
        &relay.log_text(),
    );

    // An unreachable relay is a typed error, not a hang: point the TUI at a
    // port nothing serves and read what it says.
    let dead: SocketAddr = "127.0.0.1:1".parse().expect("a valid address");
    let dead_args = [
        "--remote",
        &dead.to_string(),
        "--peer",
        &peer_cert.device().display_id(),
        "--account",
        account,
        "--identity",
        &client_identity.display().to_string(),
    ];
    let dead_refs: Vec<&str> = dead_args.to_vec();
    if let Some(mut dead_tui) = TuiSession::start_with(&tui_bin, &peer_socket, &dead_refs, &[]) {
        let reported = wait_screen(&dead_tui, "reconnecting", Duration::from_secs(30))
            || wait_screen(&dead_tui, "unreachable", Duration::from_secs(5));
        check(
            "an unreachable relay is a visible state, not a hang",
            reported,
            &dead_tui.screen(),
        );
        if evidence {
            frame(&evidence_dir, "05-unreachable-relay", &dead_tui);
        }
        dead_tui.send("q");
    } else {
        check(
            "an unreachable relay is a visible state, not a hang",
            false,
            "the TUI did not start",
        );
    }

    // Clean quit, and the local path is untouched by any of this.
    tui.send("q");
    let quit = {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if tui.exited() {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        tui.exited()
    };
    check(
        "q quits the remote TUI cleanly",
        quit,
        "the TUI did not exit",
    );

    // ---- T-0061: a remote question, in the local sidebar ----
    //
    // **After the quit above, deliberately.** This TUI uses the same client identity
    // as the one just closed, and two live sessions for one device id displace each
    // other at the relay (T-0060) — running them together made this section flaky
    // ("the TUI reaches a machine by name" lost its route mid-poll), which is the
    // product behaving correctly and the test asking for something impossible.
    //
    // A pane on the *other machine* blocks on a prompt. The operator's sidebar must
    // say which machine these panes belong to, and what the blocked agent is asking
    // — the flagship moment of §3.11, and the one that is worst to miss remotely,
    // because nobody can walk over and look at that terminal.
    let prompt = "Proceed with the deploy? [y/n]";
    let spawned = Command::new(&cli_bin)
        .args([
            "spawn",
            "asking-pane",
            "/bin/sh",
            "-c",
            &format!("printf '{prompt} '; sleep 300"),
            "--socket",
            &peer_socket.display().to_string(),
        ])
        .env("ARREO_IDENTITY_DIR", &peer_dir)
        .output()
        .expect("the spawn command runs");
    check(
        "the peer machine owns a pane that asks a question",
        spawned.status.success(),
        &String::from_utf8_lossy(&spawned.stderr),
    );

    // The TUI, on a pty, reaching that machine **by name** — nothing dialed from
    // argv, the name resolved through the account's directory.
    let config_arg = config.display().to_string();
    let name_args = ["--machine", PEER_NAME, "--config", &config_arg];
    // `ARREO_IDENTITY_DIR` names the *directory that holds* `identity/` — the
    // resolver appends that last segment itself (it is the same variable the
    // daemon and the CLI read).
    let identity_arg = client_dir.display().to_string();
    let name_env = [("ARREO_IDENTITY_DIR", identity_arg.as_str())];
    let Some(mut by_name) = TuiSession::start_by_name(&tui_bin, &name_args, &name_env) else {
        println!("[FAIL] relay: the TUI did not start with --machine");
        return ExitCode::FAILURE;
    };
    let named = wait_screen(&by_name, "asking-pane", Duration::from_secs(30));
    check(
        "the TUI reaches a machine by name",
        named,
        &by_name.screen(),
    );
    check(
        "the sidebar names the machine it is showing, and the link",
        by_name.screen().contains(&format!("{PEER_NAME} · relay")),
        &by_name.screen(),
    );
    // The question must be *in the sidebar*, not only in the focused pane's view:
    // an indented line starting near the sidebar's left edge. A check that only
    // searched the whole frame would pass on a frame with no sidebar at all.
    let asking = wait_screen(&by_name, "Proceed", Duration::from_secs(30));
    let in_sidebar = sidebar_holds(&by_name.screen(), "Proceed");
    // A long question is cut to the sidebar's width — marked with an ellipsis
    // rather than wrapped, because a wrapped line would push every pane below it
    // off the screen. The mark is the promise that this is the same text, shorter.
    let cut_marked = sidebar_holds(&by_name.screen(), "Proceed with the dep…");
    check(
        "the hung remote agent's question is shown in the sidebar",
        asking && in_sidebar,
        &by_name.screen(),
    );
    check(
        "and a question too long for the sidebar is cut, with the cut marked",
        cut_marked,
        &by_name.screen(),
    );
    if evidence {
        frame(&evidence_dir, "04-remote-question-in-sidebar", &by_name);
    }

    // The other half of the criterion: the *whole* question is readable, in the
    // pane view — the sidebar is the index, the pane is the text. Real key events:
    // the asking pane sorts first (question group leads), so Enter attaches it.
    by_name.send("\r");
    let whole = wait_screen(&by_name, prompt, Duration::from_secs(30));
    let on_the_right = right_region_holds(&by_name.screen(), prompt);
    check(
        "attaching shows the whole question, not the sidebar's summary of it",
        whole && on_the_right,
        &by_name.screen(),
    );
    if evidence {
        frame(&evidence_dir, "05-remote-question-in-pane", &by_name);
    }

    let _ = std::fs::remove_dir_all(&base);
    println!("relay: {passes} passed, {failures} failed");
    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
