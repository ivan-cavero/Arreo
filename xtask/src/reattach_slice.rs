//! T-0038 stage 3 slice: a real TUI and a real CLI tear through a real daemon
//! handoff and reattach — same pane, same pane id, under the recorded budget.
//!
//! One sentence: a daemon serves a pane that ticks (`tick-N pid=$$`) and echoes
//! input (`got:<line>`); a live `arreo attach` (a real process whose stdout is
//! parsed as it arrives) and a live `arreo-tui` on a real pty (the shared
//! harness's `TuiSession`) watch it; the daemon is handed over
//! (`--handoff-from <socket>`, the exact primitive `arreo update --server`
//! spawns); then both clients must be back on the same pane, same pane id, in
//! under `server_handoff_reattach_s` — read from `perf-budget.toml` via
//! [`crate::bench::budget_target`], the file being the law — with the tick
//! stream continuous (no loss, no duplicate, and no restart: every line carries
//! the pane's own pid, so the pid never changing across the cut *is* "the
//! session id is unchanged", observed rather than assumed). Input sent while
//! the cut is in flight is reflected exactly once per accepted send — or, when
//! the send was refused by the cut, once a retry accepts it: "lost and
//! unmentioned" is the failure the input half exists for.
//!
//! ## Why the reattach time is the product's clock, not a harness timer
//!
//! The pane emits a tick every ~50 ms, so the client's transcript is a
//! self-timing instrument: the "last pre-cut line" is the tick that stops
//! arriving, the "first post-cut line" is the tick that resumes, and the wall
//! time between them is measured at the moments the *client's output* showed
//! them — not at moments the harness chose. The number of ticks the client
//! missed (burst − last pre-cut − 1) times the measured tick period is the same
//! number from inside the product; the slice records both.
//!
//! ## The cut is `--handoff-from`, not `arreo update --server`
//!
//! `update --server`'s added steps — stage the candidate beside the installed
//! binary, prove it runs, install it — are T-0070 / stage-2 territory (covered
//! by `update_slice` and `handoff.rs`), and running it against `target/debug`
//! would write to the build tree (the `update_slice` trap). The piece this
//! slice measures is the *clients'* reattach, which depends only on the cut
//! itself; `update --server` performs exactly this primitive
//! (`<staged> --handoff-from <socket>`, update.rs `server()`), so spawning it
//! directly is the same mechanism with none of the install side effects.
//!
//! ## Hermetic
//!
//! Both daemons, every CLI invocation and the TUI get HOME and the XDG
//! directories pointed inside the slice's scratch directory, `TERM` pinned by
//! the TUI harness, socket + SQLite beside the socket in scratch, and every
//! process is killed and reaped before the slice returns (the final daemon via
//! `arreo server stop`, which drains its panes — a hard kill would orphan the
//! ticker). The `Sandbox`/`Daemon`/bounded-CLI helpers duplicate the ones in
//! `xtask/src/update_slice.rs` on purpose, with a pointer here instead of a
//! silent second edition; the TUI itself is driven through the shared harness.

use crate::bench::budget_target;
use crate::harness::{bins, TuiSession};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long one one-shot `arreo` invocation may take before the slice calls it
/// a failure. Every healthy one finishes in well under a second, so this turns
/// a hang into a named FAIL rather than a stalled slice.
const CLI_DEADLINE: Duration = Duration::from_secs(15);

const PANE_ID: &str = "tick";

/// The slice name, for `[PASS]/[FAIL]` lines and the transcript.
const MACHINE_SLICE: &str = "reattach";

/// How many ticks each client must show before the cut — enough of a run that
/// a restart (a second `tick-1`) is loud.
const MIN_PRE_TICKS: u64 = 15;

pub fn run(rest: &[String]) -> ExitCode {
    let _ = rest; // evidence is always written; today there are no flags
    let root = workspace_root();
    let evidence_dir = root.join(".loop").join("evidence").join("T-0038");
    let mut report = Report::default();

    let (server_bin, cli_bin, tui_bin) = bins();
    for bin in [&server_bin, &cli_bin, &tui_bin] {
        if !bin.exists() {
            report.fail(format!("missing binary {} (build first)", bin.display()));
            let _ = std::fs::create_dir_all(&evidence_dir);
            let _ = std::fs::write(
                evidence_dir.join("stage3-numbers.txt"),
                "T-0038 stage 3 — slice never ran (missing binaries)\n",
            );
            return finish(&report);
        }
    }

    // The budget row, read from the file — the file is the law, and a constant
    // here would be a second source for one fact.
    let budget_secs = match budget_target("server_handoff_reattach_s") {
        Ok(secs) => secs,
        Err(e) => {
            report.fail(format!("cannot read server_handoff_reattach_s: {e}"));
            return finish(&report);
        }
    };
    let budget = Duration::from_secs(budget_secs);
    report.note(format!(
        "budget: server_handoff_reattach_s = {budget_secs}s (read from perf-budget.toml)"
    ));

    let scratch = match Scratch::new() {
        Ok(scratch) => scratch,
        Err(e) => {
            report.fail(format!("scratch: {e}"));
            return finish(&report);
        }
    };
    let sandbox = match Sandbox::new(scratch.root()) {
        Ok(sandbox) => sandbox,
        Err(e) => {
            report.fail(format!("sandbox: {e}"));
            return finish(&report);
        }
    };
    let socket = scratch.root().join("a.sock");

    // ---- 1. The outgoing daemon and the pane -------------------------------
    let mut old = match Daemon::spawn(
        &sandbox,
        &server_bin,
        &cli_bin,
        &socket,
        "old",
        &["--socket", socket.to_str().expect("socket is utf-8")],
    ) {
        Ok(daemon) => daemon,
        Err(e) => {
            report.fail(format!("outgoing daemon: {e}"));
            return finish(&report);
        }
    };
    report.pass("outgoing daemon serves");
    let pane_script = format!(
        "{}\n{}\n{}",
        "stty -echo",
        // The ticker runs in a POSIX subshell so the foreground line reader can
        // echo input; `$$` in a `( … )` subshell is the parent's pid, so every
        // tick still carries the pane child's own pid.
        "( i=0; while true; do i=$((i+1)); echo \"tick-$i pid=$$\"; sleep 0.05; done ) &",
        "while IFS= read -r line; do echo \"got:$line\"; done"
    );
    let spawned = run_cli(
        &sandbox,
        &cli_bin,
        &[
            "spawn",
            PANE_ID,
            "/bin/sh",
            "-c",
            pane_script.as_str(),
            "--socket",
            socket.to_str().expect("socket is utf-8"),
        ],
        CLI_DEADLINE,
    );
    if !spawned.ok() {
        report.fail(format!("spawn pane: {spawned}"));
        return finish(&report);
    }
    // The pane is producing before any client attaches. One read races the pane's
    // very first tick (spawn → exec → `stty` → first echo is hundreds of ms under
    // load), so this polls until the read shows ticks, bounded.
    let ready = until(Duration::from_secs(15), || {
        let read = run_cli(
            &sandbox,
            &cli_bin,
            &["read", PANE_ID, "--socket", socket.to_str().expect("utf-8")],
            CLI_DEADLINE,
        );
        read.ok() && read.output.contains("tick-")
    });
    if !ready {
        report.fail("the pane never produced ticks before the cut");
        return finish(&report);
    }
    report.pass("pane ticks before the cut");

    // ---- 2. The clients -----------------------------------------------------
    let Some(attach) = AttachChild::spawn(&sandbox, &cli_bin, &socket) else {
        report.fail("cannot start `arreo attach`");
        return finish(&report);
    };
    let Some(mut tui) = ({
        let env: Vec<(&str, String)> = sandbox.env_pairs();
        let env_refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        TuiSession::start_with(&tui_bin, &socket, &[], &env_refs)
    }) else {
        report.fail("cannot start the TUI under a pty");
        return finish(&report);
    };

    // Sidebar shows the pane; Enter focuses it; the pane view streams ticks.
    if !until(Duration::from_secs(15), || tui.screen().contains(PANE_ID)) {
        report.fail(format!(
            "the TUI never listed the pane (screen: {})",
            screen_head(&tui)
        ));
        return finish(&report);
    }
    tui.send("\r");
    if !until(Duration::from_secs(15), || tui_max_tick(&tui).is_some()) {
        report.fail(format!(
            "the TUI never streamed the pane (screen: {})",
            screen_head(&tui)
        ));
        return finish(&report);
    }

    // Both clients have watched ≥ MIN_PRE_TICKS ticks before the cut.
    if !until(Duration::from_secs(15), || {
        attach.last_tick().is_some_and(|(n, _)| n >= MIN_PRE_TICKS)
    }) {
        report.fail("the CLI attach never streamed enough ticks before the cut");
        return finish(&report);
    }
    if !until(Duration::from_secs(15), || {
        tui_max_tick(&tui).is_some_and(|n| n >= MIN_PRE_TICKS)
    }) {
        report.fail("the TUI never displayed enough ticks before the cut");
        return finish(&report);
    }
    report.pass("CLI and TUI attached and streaming pre-cut ticks");

    // The pane id is on the daemon's side, unchanged across the cut — the
    // session the clients are attached to.
    let panes_pre = run_cli(
        &sandbox,
        &cli_bin,
        &["panes", "--socket", socket.to_str().expect("utf-8")],
        CLI_DEADLINE,
    );
    if !panes_pre.ok() || !panes_pre.output.contains(PANE_ID) {
        report.fail(format!(
            "the daemon does not list the pane pre-cut: {panes_pre}"
        ));
        return finish(&report);
    }

    // ---- 3. The cut, with input racing it -----------------------------------
    let cut_start = Instant::now();
    let sender_stop = Arc::new(AtomicBool::new(false));
    let send_log: Arc<Mutex<Vec<(String, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let sender = start_sender(&sandbox, &cli_bin, &socket, &sender_stop, &send_log);
    let mut incoming = match Daemon::spawn(
        &sandbox,
        &server_bin,
        &cli_bin,
        &socket,
        "new",
        &[
            "--handoff-from",
            socket.to_str().expect("socket is utf-8"),
            "--handoff-timeout-secs",
            "30",
        ],
    ) {
        Ok(daemon) => daemon,
        Err(e) => {
            sender_stop.store(true, Ordering::Relaxed);
            let _ = sender.join();
            report.fail(format!("incoming daemon: {e}"));
            return finish(&report);
        }
    };
    report.note("cut started; waiting for the outgoing daemon to exit");

    // While the cut happens, track the TUI's last display *advance* (the poll
    // updates the pane view about once a second). The outgoing daemon's exit
    // freezes the display — the connection is dead — so the last advance at or
    // before the exit is the last pre-cut line the TUI ever showed, which is
    // the transcript-clock anchor (see the module doc).
    let mut tui_last_max: u64 = tui_max_tick(&tui).unwrap_or(0);
    let mut tui_last_advance_ts: Instant = Instant::now();
    let exit_deadline = Instant::now() + Duration::from_secs(30);
    let mut old_gone: Option<Instant> = None;
    while old_gone.is_none() && Instant::now() < exit_deadline {
        if old.try_wait() {
            old_gone = Some(Instant::now());
            break;
        }
        if let Some(max) = tui_max_tick(&tui) {
            if max > tui_last_max {
                tui_last_max = max;
                tui_last_advance_ts = Instant::now();
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let Some(old_exit_ts) = old_gone else {
        sender_stop.store(true, Ordering::Relaxed);
        let _ = sender.join();
        report.fail("the outgoing daemon never exited — the cut did not complete");
        return finish(&report);
    };
    report.pass("the cut completed: the outgoing daemon exited");

    // Post-cut: something serves the socket and still knows the pane.
    let panes_post = run_cli(
        &sandbox,
        &cli_bin,
        &["panes", "--socket", socket.to_str().expect("utf-8")],
        CLI_DEADLINE,
    );
    if !panes_post.ok() || !panes_post.output.contains(PANE_ID) {
        report.fail(format!(
            "after the cut the daemon does not list the pane (pane id changed or lost): \
             {panes_post}"
        ));
        let _ = incoming.stop();
        sender_stop.store(true, Ordering::Relaxed);
        let _ = sender.join();
        return finish(&report);
    }
    report.pass(format!(
        "pane id unchanged across the cut: `panes` still lists {PANE_ID:?}"
    ));

    // ---- 4. The reattach measurement -----------------------------------------
    // TUI: the first display advance past the pre-cut max is the first
    // post-cut line the TUI showed; the CLI anchor is its transcript.
    let mut tui_resumed: Option<(u64, Instant)> = None;
    let tui_deadline = Instant::now() + Duration::from_secs(20);
    while tui_resumed.is_none() && Instant::now() < tui_deadline {
        if let Some(max) = tui_max_tick(&tui) {
            if max > tui_last_max {
                tui_resumed = Some((max, Instant::now()));
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let cli_post_idx = {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let ticks = attach.ticks();
            if let Some(idx) = ticks.iter().position(|(_, _, ts, _)| *ts > old_exit_ts) {
                break Some(idx);
            }
            if Instant::now() > deadline {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let Some(cli_post_idx) = cli_post_idx else {
        report.fail("the CLI attach never resumed after the cut");
        let _ = incoming.stop();
        sender_stop.store(true, Ordering::Relaxed);
        let _ = sender.join();
        return finish(&report);
    };
    let ticks = attach.ticks();
    let cli_pre_idx = cli_post_idx.saturating_sub(1);
    let (cli_pre_tick, _, cli_pre_ts, _) = ticks[cli_pre_idx];
    let (cli_post_tick, _, cli_post_ts, _) = ticks[cli_post_idx];
    let cli_reattach = cli_post_ts.duration_since(cli_pre_ts);
    let cli_cut_to_resume = cli_post_ts.duration_since(cut_start);

    let Some((tui_post_max, tui_post_ts)) = tui_resumed else {
        report.fail(format!(
            "the TUI never resumed after the cut (last pre-cut max {tui_last_max} shown \
             {:.3}s after cut start; TUI process exited: {}; screen now:\n{})",
            tui_last_advance_ts.duration_since(cut_start).as_secs_f64(),
            tui.exited(),
            screen_head(&tui),
        ));
        let _ = incoming.stop();
        sender_stop.store(true, Ordering::Relaxed);
        let _ = sender.join();
        return finish(&report);
    };
    let tui_pre_ts = tui_last_advance_ts;
    let tui_reattach = tui_post_ts.duration_since(tui_pre_ts);
    let tui_cut_to_resume = tui_post_ts.duration_since(cut_start);

    // The transcript-clock cross-check: missing ticks × the measured period. The
    // daemon replays everything emitted during the outage in one burst, so the
    // *newest* replay tick (not the burst's first line, which is the wall
    // anchor) marks how long the client was away.
    let period = attach.mean_tick_period(cli_pre_idx);
    let cli_burst_max = ticks
        .iter()
        .skip(cli_post_idx)
        .take_while(|(_, _, ts, _)| ts.duration_since(cli_post_ts) < Duration::from_millis(500))
        .map(|(n, _, _, _)| *n)
        .max()
        .unwrap_or(cli_post_tick);
    let cli_missed = cli_burst_max.saturating_sub(cli_pre_tick);
    let tui_missed = tui_post_max.saturating_sub(tui_last_max);
    let cli_clock = period.map(|p| Duration::from_secs_f64(cli_missed as f64 * p.as_secs_f64()));
    let tui_clock = period.map(|p| Duration::from_secs_f64(tui_missed as f64 * p.as_secs_f64()));

    let cli_ms = cli_reattach.as_millis();
    let tui_ms = tui_reattach.as_millis();
    report.check(
        "cli reattaches under the budget",
        cli_ms < budget.as_millis(),
        format!("{cli_ms} ms (budget {budget_secs}s)"),
    );
    report.check(
        "tui reattaches under the budget",
        tui_ms < budget.as_millis(),
        format!("{tui_ms} ms (budget {budget_secs}s)"),
    );
    report.note(format!(
        "cli: last pre-cut tick {cli_pre_tick}, first post-cut tick {cli_post_tick} \
         (reattach {:.3}s, cut→resume {:.3}s, transcript clock ≈ {:.3}s from {cli_missed} \
         missed ticks × {:.0} ms)",
        cli_reattach.as_secs_f64(),
        cli_cut_to_resume.as_secs_f64(),
        cli_clock.map(|d| d.as_secs_f64()).unwrap_or(0.0),
        period.map(|p| p.as_millis() as f64).unwrap_or(0.0),
    ));
    report.note(format!(
        "tui: last pre-cut tick {tui_last_max}, first post-cut tick {tui_post_max} \
         (reattach {:.3}s, cut→resume {:.3}s, transcript clock ≈ {:.3}s from {tui_missed} \
         missed ticks × {:.0} ms)",
        tui_reattach.as_secs_f64(),
        tui_cut_to_resume.as_secs_f64(),
        tui_clock.map(|d| d.as_secs_f64()).unwrap_or(0.0),
        period.map(|p| p.as_millis() as f64).unwrap_or(0.0),
    ));

    // ---- 5. The stream contracts ---------------------------------------------
    // The CLI's stdout is the pane's own transcript: one pid from first line to
    // last (a restarted child would print a new pid and start at tick-1 again)
    // and the tick sequence contiguous (each next, no gap, no duplicate).
    let first_pid = ticks.first().map(|(_, pid, _, _)| *pid);
    let all_same_pid = ticks.iter().all(|(_, pid, _, _)| Some(*pid) == first_pid);
    let contiguous = ticks
        .windows(2)
        .all(|w| w[1].0 == w[0].0 + 1 && w[1].1 == w[0].1);
    report.check(
        "cli transcript: one pane pid across the cut",
        all_same_pid,
        format!("pids seen: {first_pid:?}"),
    );
    report.check(
        "cli transcript: ticks contiguous (no loss, no duplicate, no restart)",
        contiguous,
        format!("{} lines", ticks.len()),
    );
    // The TUI is on the same pane after the cut: its title still names the pane
    // and the tick stream advanced — the view was not restarted at tick-1.
    report.check(
        "tui shows the same pane after the cut",
        tui.screen().contains(PANE_ID) && tui_post_max > tui_last_max,
        format!("shows {PANE_ID:?}; tick advanced {tui_last_max} → {tui_post_max}"),
    );

    // ---- 6. Input sent during the cut ----------------------------------------
    // Stop the sender, retry the refused sends against the still-serving daemon,
    // and only then read the pane's accumulated echo back in one final read —
    // a snapshot taken before the retries would miss their echoes entirely.
    // The daemon itself is kept up until the very end (its graceful drain kills
    // the pane child, which is what ends the ticker).
    sender_stop.store(true, Ordering::Relaxed);
    let _ = sender.join();
    let performed: Vec<(String, bool)> = send_log.lock().map(|l| l.clone()).unwrap_or_default();
    // The retryable half: a send refused by the cut is retried once; it must be
    // accepted and its echo must then appear.
    let mut acked_first: Vec<String> = Vec::new();
    let mut refused: Vec<String> = Vec::new();
    for (text, ok) in performed {
        if ok {
            acked_first.push(text);
        } else {
            refused.push(text);
        }
    }
    let mut retried_ok: Vec<String> = Vec::new();
    for text in &refused {
        let line = format!("{text}\n");
        let retry = run_cli(
            &sandbox,
            &cli_bin,
            &[
                "send",
                PANE_ID,
                &line,
                "--socket",
                socket.to_str().expect("utf-8"),
            ],
            CLI_DEADLINE,
        );
        if retry.ok() {
            retried_ok.push(text.clone());
        }
    }
    // The pane echoes within milliseconds; a short settle keeps the read from
    // racing the pane's own echo of the last retry.
    std::thread::sleep(Duration::from_millis(300));
    let final_read = run_cli(
        &sandbox,
        &cli_bin,
        &["read", PANE_ID, "--socket", socket.to_str().expect("utf-8")],
        CLI_DEADLINE,
    );
    let echoes: Vec<String> = final_read
        .output
        .lines()
        .filter_map(|line| line.strip_prefix("got:").map(str::to_string))
        .collect();
    let mut input_detail: Vec<String> = Vec::new();
    let mut input_failures = 0usize;
    for text in &acked_first {
        let count = echoes.iter().filter(|e| *e == text).count();
        input_detail.push(format!("{text}: accepted — {count} echo(s)"));
        if count != 1 {
            input_failures += 1;
            report.fail(format!(
                "input {text:?} was accepted but reflected {count} times (acked, never lost)"
            ));
        }
    }
    for text in &refused {
        let accepted = retried_ok.iter().any(|t| t == text);
        let count = echoes.iter().filter(|e| *e == text).count();
        if accepted {
            input_detail.push(format!(
                "{text}: refused by the cut, retried ok — {count} echo(s)"
            ));
        } else {
            input_detail.push(format!("{text}: refused by the cut, retry also failed"));
        }
        if accepted && count < 1 {
            input_failures += 1;
            report.fail(format!(
                "retried input {text:?} was accepted but never reflected (accepted, never lost)"
            ));
        }
    }
    report.check(
        "input during the cut is reflected exactly once per accepted send",
        input_failures == 0,
        format!(
            "{} accepted, {} refused-then-retried, {} echoes total",
            acked_first.len(),
            retried_ok.len(),
            echoes.len()
        ),
    );

    // The measurements are taken; take the daemon down (graceful drain, so the
    // pane child goes with it) before writing the evidence.
    let _ = incoming.stop();

    // ---- Evidence -------------------------------------------------------------
    let pre_secs = |t: Instant| t.duration_since(cut_start).as_secs_f64();
    let period_ms = period.map(|p| p.as_millis() as f64).unwrap_or(0.0);
    let numbers = [
        "T-0038 stage 3 — client reattach across a daemon handoff".to_string(),
        format!(
            "budget: server_handoff_reattach_s = {budget_secs}s (perf-budget.toml, phase0=true)"
        ),
        String::new(),
        format!(
            "cli: last pre-cut tick {cli_pre_tick} at {:.3}s; first post-cut tick \
             {cli_post_tick} at {:.3}s (replay burst through tick {cli_burst_max}; t=0 is cut start)",
            pre_secs(cli_pre_ts),
            pre_secs(cli_post_ts)
        ),
        format!(
            "cli: reattach {:.3}s | cut→resume {:.3}s | transcript clock ≈ {:.3}s \
             ({cli_missed} missed ticks × {period_ms:.0} ms)",
            cli_reattach.as_secs_f64(),
            cli_cut_to_resume.as_secs_f64(),
            cli_clock.map(|d| d.as_secs_f64()).unwrap_or(0.0),
        ),
        String::new(),
        format!(
            "tui: last pre-cut tick {tui_last_max} shown at {:.3}s; first post-cut tick \
             {tui_post_max} at {:.3}s",
            pre_secs(tui_pre_ts),
            pre_secs(tui_post_ts)
        ),
        format!(
            "tui: reattach {:.3}s | cut→resume {:.3}s | transcript clock ≈ {:.3}s \
             ({tui_missed} missed ticks × {period_ms:.0} ms)",
            tui_reattach.as_secs_f64(),
            tui_cut_to_resume.as_secs_f64(),
            tui_clock.map(|d| d.as_secs_f64()).unwrap_or(0.0),
        ),
        String::new(),
        format!(
            "pane id: {PANE_ID:?} unchanged — `panes` lists it before and after the cut; the \
             CLI transcript carries one pid ({first_pid:?}) throughout; CLI ticks contiguous: \
             {contiguous}"
        ),
        format!(
            "input during cut: {} accepted sends, {} refused (of which {retried_ok_len} retried \
             ok); every accepted send reflected exactly once: {input_clean}",
            acked_first.len(),
            refused.len(),
            retried_ok_len = retried_ok.len(),
            input_clean = input_failures == 0,
        ),
    ]
    .join("\n");
    let _ = std::fs::create_dir_all(&evidence_dir);
    for (name, text) in [
        ("stage3-numbers.txt", &numbers),
        ("stage3-cli-stream.txt", &attach.evidence()),
        (
            "stage3-tui-screens.txt",
            &format!("post-cut screen (pane region):\n{}\n", tui.screen()),
        ),
        ("stage3-sends.txt", &input_detail.join("\n")),
    ] {
        if let Err(e) = std::fs::write(evidence_dir.join(name), text) {
            report.fail(format!("cannot write evidence {name}: {e}"));
        }
    }
    report.note(format!("evidence written to {}", evidence_dir.display()));
    finish(&report)
}

/// Print the summary line and return the slice's exit code.
fn finish(report: &Report) -> ExitCode {
    if report.failures == 0 {
        println!(
            "[PASS] {MACHINE_SLICE}: reattach within budget ({} checks)",
            report.passes
        );
        ExitCode::SUCCESS
    } else {
        println!(
            "[FAIL] {MACHINE_SLICE}: {} failure(s), {} pass(es)",
            report.failures, report.passes
        );
        ExitCode::FAILURE
    }
}

/// The workspace root: xtask's manifest dir is one level below it.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

/// A verdict counter.
#[derive(Default)]
struct Report {
    passes: usize,
    failures: usize,
}

impl Report {
    fn say(&mut self, line: String) {
        println!("{MACHINE_SLICE}> {line}");
    }
    fn note(&mut self, line: impl Into<String>) {
        self.say(format!("note: {}", line.into()));
    }
    fn pass(&mut self, what: impl Into<String>) {
        println!("[PASS] {MACHINE_SLICE}: {}", what.into());
        self.passes += 1;
    }
    fn fail(&mut self, what: impl Into<String>) {
        println!("[FAIL] {MACHINE_SLICE}: {}", what.into());
        self.failures += 1;
    }
    fn check(&mut self, name: &str, ok: bool, detail: String) {
        if ok {
            self.pass(format!("{name} ({detail})"));
        } else {
            self.fail(format!("{name}: {detail}"));
        }
    }
}

/// The scratch directory, removed when it goes out of scope (its socket and
/// SQLite live inside, and a failed run must not leave state behind).
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Result<Self, String> {
        let root = std::env::var_os("ARREO_E2E_SCRATCH")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(format!("arreo-e2e-reattach-{}", std::process::id()));
        match std::fs::remove_dir_all(&root) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("clean {}: {e}", root.display())),
        }
        std::fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
        Ok(Self(root))
    }

    fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The environment every process in this slice gets: HOME and the XDG
/// directories pointed inside the scratch directory, and `ARREO_STATE_DIR` in
/// case any component consults it. The daemon keys its SQLite off the socket
/// path, but its device-authority bootstrap follows `XDG_DATA_HOME`, and the
/// slice must not touch — or depend on — the developer's own state.
#[derive(Clone)]
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(root: &Path) -> Result<Self, String> {
        for dir in ["home", "state", "data", "config", "run", "arreo-state"] {
            std::fs::create_dir_all(root.join(dir))
                .map_err(|e| format!("{}/{dir}: {e}", root.display()))?;
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    fn command(&self, program: &Path) -> Command {
        let mut command = Command::new(program);
        command
            .current_dir(&self.root)
            .env("HOME", self.root.join("home"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_RUNTIME_DIR", self.root.join("run"))
            .env("ARREO_STATE_DIR", self.root.join("arreo-state"));
        command
    }

    /// The same variables, as `&str` pairs for the TUI harness.
    fn env_pairs(&self) -> Vec<(&str, String)> {
        vec![
            ("HOME", self.root.join("home").display().to_string()),
            (
                "XDG_STATE_HOME",
                self.root.join("state").display().to_string(),
            ),
            (
                "XDG_DATA_HOME",
                self.root.join("data").display().to_string(),
            ),
            (
                "XDG_CONFIG_HOME",
                self.root.join("config").display().to_string(),
            ),
            (
                "XDG_RUNTIME_DIR",
                self.root.join("run").display().to_string(),
            ),
            (
                "ARREO_STATE_DIR",
                self.root.join("arreo-state").display().to_string(),
            ),
        ]
    }
}

/// A daemon the slice spawned, kill + reap on drop. The outgoing daemon exits
/// by itself at the commit; the incoming one keeps serving (that is its job),
/// so `stop()` drains it gracefully at the end — `arreo server stop` (SIGTERM)
/// takes the pane children with it, where a hard kill would orphan the ticker.
struct Daemon {
    child: Child,
    sandbox: Sandbox,
    cli: PathBuf,
    socket: PathBuf,
}

impl Daemon {
    fn spawn(
        sandbox: &Sandbox,
        server_bin: &Path,
        cli_bin: &Path,
        socket: &Path,
        tag: &str,
        args: &[&str],
    ) -> Result<Self, String> {
        let log = sandbox.root.join(format!("server-{tag}.log"));
        let out = std::fs::File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
        let err = out
            .try_clone()
            .map_err(|e| format!("{}: {e}", log.display()))?;
        let mut command = sandbox.command(server_bin);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err));
        let child = command
            .spawn()
            .map_err(|e| format!("{}: {e}", server_bin.display()))?;
        let daemon = Self {
            child,
            sandbox: sandbox.clone(),
            cli: cli_bin.to_path_buf(),
            socket: socket.to_path_buf(),
        };
        if !daemon.wait_for_socket() {
            return Err(format!(
                "the daemon never bound {}; it said:\n{}",
                socket.display(),
                std::fs::read_to_string(&log).unwrap_or_default()
            ));
        }
        Ok(daemon)
    }

    fn wait_for_socket(&self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&self.socket).is_ok() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn try_wait(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Graceful stop: `arreo server stop` (SIGTERM drain), with a kill as the
    /// bounded backstop. A hard kill is the backstop, never the first move:
    /// the pane's child would otherwise be orphaned and keep ticking.
    fn stop(&mut self) -> Result<(), String> {
        match self.child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(e) => return Err(format!("cannot ask after the daemon: {e}")),
        }
        let socket = self.socket.display().to_string();
        let stopped = run_cli(
            &self.sandbox,
            &self.cli,
            &["server", "stop", "--socket", &socket],
            CLI_DEADLINE,
        );
        if !stopped.ok() && !self.try_wait() {
            return Err(format!("server stop failed: {stopped}"));
        }
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !self.try_wait() {
            std::thread::sleep(Duration::from_millis(50));
        }
        if !self.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        Ok(())
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// One tick the CLI attach observed: the tick number, the pane's announced pid,
/// when it was printed, and the raw line.
type ObservedTick = (u64, u32, Instant, String);

/// The `arreo attach` client, as a long-lived child. Its stdout — the pane's
/// own transcript — is parsed as it arrives into (tick, pid, timestamp)
/// observations: these ARE the measurement (see the module doc). Killed on
/// drop.
struct AttachChild {
    child: Child,
    ticks: Arc<Mutex<Vec<ObservedTick>>>,
    stderr: Arc<Mutex<String>>,
    raw: Arc<Mutex<Vec<String>>>,
}

impl AttachChild {
    fn spawn(sandbox: &Sandbox, cli_bin: &Path, socket: &Path) -> Option<Self> {
        let mut child = sandbox
            .command(cli_bin)
            .arg("attach")
            .arg(PANE_ID)
            .arg("--socket")
            .arg(socket)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?;
        let stdout = child.stdout.take()?;
        let stderr = child.stderr.take()?;
        let ticks = Arc::new(Mutex::new(Vec::new()));
        let raw = Arc::new(Mutex::new(Vec::new()));
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        {
            let ticks = Arc::clone(&ticks);
            let raw = Arc::clone(&raw);
            std::thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    if let Some((n, pid)) = parse_tick(&line) {
                        let ts = Instant::now();
                        if let Ok(mut ticks) = ticks.lock() {
                            ticks.push((n, pid, ts, line.clone()));
                        }
                    }
                    if let Ok(mut raw) = raw.lock() {
                        raw.push(line);
                    }
                }
            });
        }
        {
            let stderr_buf = Arc::clone(&stderr_buf);
            std::thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if let Ok(mut buf) = stderr_buf.lock() {
                        buf.push_str(&line);
                        buf.push('\n');
                    }
                }
            });
        }
        Some(Self {
            child,
            ticks,
            stderr: stderr_buf,
            raw,
        })
    }

    fn ticks(&self) -> Vec<(u64, u32, Instant, String)> {
        self.ticks.lock().map(|t| t.clone()).unwrap_or_default()
    }

    fn last_tick(&self) -> Option<(u64, u32)> {
        self.ticks().last().map(|(n, pid, _, _)| (*n, *pid))
    }

    /// The mean tick period over the stream up to `up_to` ticks, for the
    /// transcript-clock cross-check.
    fn mean_tick_period(&self, up_to: usize) -> Option<Duration> {
        let ticks = self.ticks();
        if ticks.len() < 2 {
            return None;
        }
        let up_to = up_to.min(ticks.len());
        let first = ticks[0].2;
        let last = ticks[up_to - 1].2;
        let n = (up_to - 1) as f64;
        Some(Duration::from_secs_f64(
            last.duration_since(first).as_secs_f64() / n,
        ))
    }

    fn evidence(&self) -> String {
        let raw = self.raw.lock().map(|r| r.clone()).unwrap_or_default();
        let stderr = self.stderr.lock().map(|s| s.clone()).unwrap_or_default();
        let mut out = String::new();
        out.push_str("--- `arreo attach` stdout (the pane's transcript) ---\n");
        for line in &raw {
            out.push_str(line);
            out.push('\n');
        }
        out.push_str("\n--- `arreo attach` stderr (reconnect notices) ---\n");
        out.push_str(&stderr);
        out
    }
}

impl Drop for AttachChild {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// One `arreo` invocation, bounded by a deadline: a command that runs past the
/// deadline is killed and reported, never waited on for ever.
fn run_cli(sandbox: &Sandbox, bin: &Path, args: &[&str], deadline: Duration) -> Run {
    let start = Instant::now();
    let mut child = match sandbox
        .command(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return Run {
                code: None,
                output: format!("could not start {}: {e}", bin.display()),
            }
        }
    };
    // Both pipes are drained from their own threads: a child that fills a pipe
    // while this loop waits for it to exit would deadlock, which is exactly
    // what the deadline exists to prevent.
    let stdout_drain = drain(child.stdout.take().expect("stdout was piped"));
    let stderr_drain = drain(child.stderr.take().expect("stderr was piped"));
    let (code, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status.code(), false),
            Ok(None) if start.elapsed() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break (None, true);
            }
        }
    };
    let mut output = stdout_drain.join().unwrap_or_default();
    output.push_str(&stderr_drain.join().unwrap_or_default());
    if timed_out {
        output = format!("timed out after {deadline:?}; it had printed: {output:?}");
    }
    Run { code, output }
}

fn drain<R: Read + Send + 'static>(mut stream: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// What one `arreo` invocation did, bounded.
struct Run {
    code: Option<i32>,
    output: String,
}

impl Run {
    fn ok(&self) -> bool {
        self.code == Some(0)
    }
}

impl std::fmt::Display for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.output.trim())
    }
}

/// The input-during-cut sender: `arreo send tick CUT-<n>` in a tight loop,
/// each invocation one atomic Hello→Send→Ok round trip, logging whether the
/// daemon accepted it. Started just before the cut, stopped after it settles.
fn start_sender(
    sandbox: &Sandbox,
    cli_bin: &Path,
    socket: &Path,
    stop: &Arc<AtomicBool>,
    log: &Arc<Mutex<Vec<(String, bool)>>>,
) -> std::thread::JoinHandle<()> {
    let sandbox = sandbox.clone();
    let cli_bin = cli_bin.to_path_buf();
    let socket = socket.to_path_buf();
    let stop = Arc::clone(stop);
    let log = Arc::clone(log);
    std::thread::spawn(move || {
        let mut n = 0u64;
        while !stop.load(Ordering::Relaxed) {
            n += 1;
            // The pane's tty is in canonical mode: input is delivered to the
            // child's `read` only at a newline, so a send without one would sit
            // in the line discipline and never be echoed. The ack is keyed on
            // the text without the newline.
            let text = format!("CUT-{n}");
            let line = format!("{text}\n");
            let run = run_cli(
                &sandbox,
                &cli_bin,
                &[
                    "send",
                    PANE_ID,
                    &line,
                    "--socket",
                    socket.to_str().expect("utf-8"),
                ],
                CLI_DEADLINE,
            );
            if let Ok(mut log) = log.lock() {
                log.push((text, run.ok()));
            }
        }
    })
}

/// Parse `tick-<n> pid=<pid>` lines. Whole-line prefix matching only — `tick-1`
/// is a prefix of `tick-10`, so a substring test would misread.
fn parse_tick(line: &str) -> Option<(u64, u32)> {
    let rest = line.strip_prefix("tick-")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let n = digits.parse().ok()?;
    let pid = rest.split("pid=").nth(1)?.trim().parse().ok()?;
    Some((n, pid))
}

/// The newest tick visible on the TUI's rendered screen. The pane view shows
/// the tail of the pane's scrollback, so the max is the latest line shown.
/// The tick text sits inside the rendered row (after the sidebar and borders),
/// so the row is searched for the `tick-` marker rather than parsed from its
/// start.
fn tui_max_tick(tui: &TuiSession) -> Option<u64> {
    tui.screen()
        .lines()
        .filter_map(|line| {
            let idx = line.find("tick-")?;
            let rest = &line[idx + 5..];
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().ok()
        })
        .max()
}

fn screen_head(tui: &TuiSession) -> String {
    tui.screen().lines().take(12).collect::<Vec<_>>().join("\n")
}

/// Poll `f` every 20 ms until true or `limit` elapses.
fn until(limit: Duration, mut f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}
