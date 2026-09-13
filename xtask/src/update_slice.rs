//! T-0070 update slice: prove that swapping the client binary cannot touch a
//! PTY-bearing process.
//!
//! One sentence: eight panes count upward on a real daemon, `arreo update` swaps
//! the client's own binary underneath itself, and the counters keep climbing —
//! which is what "the update path never signals, reaps, restarts or stops a
//! PTY-bearing process" looks like from outside.
//!
//! ## Why the panes count, and why that is the proof
//!
//! The invariant is about processes the update must not touch, so the obvious
//! assertion is "the pane pids did not change". That assertion cannot be made:
//! `PaneInfo` carries an `id`, an `alive` flag and an `alert`, and **no pid at
//! all** — pane pids are not on the wire, by design, because the daemon does not
//! hand out handles to the processes it owns. Recording them is not an option the
//! protocol offers.
//!
//! So the proof is behavioural instead. Each pane runs a monotonic counter:
//!
//! ```text
//! echo PANE-<N>-MARKER; i=0; while :; do i=$((i+1)); echo tick-$i; sleep 1; done
//! ```
//!
//! A process that was restarted starts over: it prints its marker a *second* time
//! and `tick-1` a second time. A process that was left alone keeps counting. After
//! the update the slice therefore asserts, per pane, that the marker appears
//! exactly once, that `tick-1` appears exactly once, and that the highest tick is
//! *past* where it was before. The count of `tick-1` is the decisive one: a
//! restarted pane's counter starts climbing again too, so "the number went up" on
//! its own would prove nothing.
//!
//! The daemon *is* observable, because this slice spawns it and holds the
//! [`Child`]: it can assert `try_wait()` is still `None` (never exited, never
//! reaped) and that `id()` is unchanged (never signalled and replaced).
//!
//! ## The trap this slice must never fall into
//!
//! **Never point the verb at `target/debug/arreo`.** `arreo update` installs into
//! the path of the binary it is running from, so running it against the build
//! tree's own binary would `rename(2)` a copy over that artefact — every later
//! test, every later slice and the developer's own next `cargo run` would then be
//! exercising a different file than the one they just built, with no error to say
//! so. The slice therefore copies the real binary into its scratch directory and
//! drives the *copy*; `target/debug/arreo` is only ever read, never written.
//!
//! ## Two distinct binaries out of one
//!
//! `--from` must name a binary that differs from the installed one, or the verb
//! correctly decides there is nothing to do. The slice gets a second, working
//! binary by copying the first and appending a byte: **an ELF ignores trailing
//! bytes**, so both run while differing in length and content. That is also what
//! makes `--rollback` observable at all — without two distinct binaries there is
//! no swap to undo.
//!
//! ## Hermetic
//!
//! One daemon on a socket inside the scratch directory, panes that are `/bin/sh`,
//! no network, `target/debug` read-only, and every process started with `HOME` and
//! the XDG directories pointed inside the scratch directory — this verb writes a
//! resume token, and it must not land in the developer's own `~/.local/state`.
//!
//! `--interactive-evidence` writes the transcript and the raw before/after pane
//! reads to `.loop/evidence/T-0070/`.

use crate::harness::bins;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

/// How long one CLI invocation may take before the slice calls it a failure.
///
/// **A deadline on every call, because a slice that hangs is worse than one that
/// fails.** A hung command produces no verdict at all: the run stalls until
/// something outside kills it and the operator learns nothing. Every healthy
/// invocation here finishes in well under a second (the reattach budget under test
/// is 2 s), so this bounds a stall far above any real latency and turns it into a
/// named FAIL carrying whatever the command had printed.
const CLI_DEADLINE: Duration = Duration::from_secs(15);

/// The panes whose counters carry the invariant. Eight, because the claim is about
/// *every* PTY-bearing process and one pane would be a sample of one.
const PANES: usize = 8;

/// The reattach budget, from the task: a resume that takes longer than this is a
/// restart with extra steps.
const REATTACH_BUDGET: Duration = Duration::from_secs(2);

/// The counter each pane runs.
///
/// The marker is printed once, at start, and is what makes a restart visible: a
/// process that was restarted prints it again.
fn tick_script(n: usize) -> String {
    format!("echo PANE-{n}-MARKER; i=0; while :; do i=$((i+1)); echo tick-$i; sleep 1; done")
}

fn pane_id(n: usize) -> String {
    format!("pane-{n}")
}

pub fn run(rest: &[String]) -> ExitCode {
    let evidence = rest.iter().any(|a| a == "--interactive-evidence");
    let evidence_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .join(".loop")
        .join("evidence")
        .join("T-0070");
    if evidence {
        let _ = fs::create_dir_all(&evidence_dir);
    }

    let mut report = Report::default();
    slice(&mut report);

    if evidence {
        let _ = fs::write(evidence_dir.join("transcript.txt"), report.transcript());
        for (name, body) in [
            ("panes-before.txt", &report.panes_before),
            ("panes-after.txt", &report.panes_after),
        ] {
            if !body.is_empty() {
                let _ = fs::write(evidence_dir.join(name), body);
            }
        }
    }
    report.finish()
}

/// The checks, in the order the task lists them.
///
/// Split out from [`run`] so that every exit path — including a missing binary —
/// still prints the summary line and still writes the evidence.
fn slice(report: &mut Report) {
    let (server_bin, cli_bin, _tui_bin) = bins();
    for bin in [&server_bin, &cli_bin] {
        if !bin.exists() {
            report.check(
                "the binaries under test are built",
                false,
                &format!(
                    "{} does not exist; build first (`cargo build -p arreo-cli -p arreo-server`)",
                    bin.display()
                ),
            );
            return;
        }
    }

    let root = match scratch_root() {
        Ok(root) => root,
        Err(e) => {
            report.check("the scratch directory can be made", false, &e);
            return;
        }
    };
    // Declared before the daemon so the daemon is stopped first: dropping
    // `Scratch` removes the socket out from under a live daemon.
    let _scratch = Scratch(root.clone());
    let sandbox = match Sandbox::new(root.clone()) {
        Ok(sandbox) => sandbox,
        Err(e) => {
            report.check("the scratch environment can be made", false, &e);
            return;
        }
    };

    let socket = root.join("arreo.sock");
    let sock = socket.display().to_string();

    // ---- the fixtures: copies, never the build tree -------------------------
    //
    // **The trap.** `arreo update` installs into the path of the binary it runs
    // from. Pointing it at `target/debug/arreo` would replace the build tree's own
    // artefact, and every later test and slice would silently be running a
    // different file than the one `cargo build` produced. So the slice drives a
    // copy, and the real binary is only ever read.
    let installed = root.join("arreo");
    let baseline = root.join("arreo-original");
    let candidate = root.join("arreo-new");
    let third = root.join("arreo-third");
    for (dst, tail) in [
        (&installed, &b""[..]),
        (&baseline, &b""[..]),
        // One trailing byte: an ELF ignores it, so this still runs, but it is no
        // longer byte-identical to the installed binary — which is the difference
        // between a swap this slice can observe and a no-op.
        (&candidate, &b"\n"[..]),
        // A different tail again, so the reattach check has its own candidate to
        // install (installing the *same* bytes would correctly no-op).
        (&third, &b"\n# the third candidate\n"[..]),
    ] {
        if let Err(e) = copy_with_tail(&cli_bin, dst, tail) {
            report.check(
                "the fixture binaries are copies of the real one",
                false,
                &format!("{}: {e}", dst.display()),
            );
            return;
        }
    }
    let (installed_path, baseline_path, candidate_path, third_path) = (
        installed.display().to_string(),
        baseline.display().to_string(),
        candidate.display().to_string(),
        third.display().to_string(),
    );
    report.say(format!(
        "update: driving the copy at {installed_path}; {} is only ever read from",
        cli_bin.display()
    ));

    // The two-candidates trick is an assumption about ELF, so it is checked
    // rather than assumed: if a candidate could not run, every later check would be
    // reporting on a fixture that was never valid.
    let distinct = !same_bytes(&installed, &candidate).unwrap_or(true)
        && !same_bytes(&installed, &third).unwrap_or(true)
        && !same_bytes(&candidate, &third).unwrap_or(true);
    let candidates_run = run_cli(&sandbox, &candidate, &["--version"], CLI_DEADLINE).ok()
        && run_cli(&sandbox, &third, &["--version"], CLI_DEADLINE).ok();
    report.check(
        "the candidates differ from the installed binary and still run",
        distinct && candidates_run,
        &format!("distinct={distinct} both_run={candidates_run}"),
    );
    if !(distinct && candidates_run) {
        return;
    }

    // ---- the daemon, and eight panes with counters --------------------------
    let mut daemon = match Daemon::spawn(&sandbox, &server_bin, &cli_bin, &socket) {
        Ok(daemon) => daemon,
        Err(e) => {
            report.check("the daemon starts and binds its socket", false, &e);
            return;
        }
    };
    let daemon_pid_before = daemon.id;
    report.say(format!(
        "update: the daemon is pid {daemon_pid_before} on {sock} — spawned by this slice, so its \
         liveness is observable (this process holds its Child)"
    ));

    for n in 1..=PANES {
        let id = pane_id(n);
        let script = tick_script(n);
        let r = run_cli(
            &sandbox,
            &installed,
            &["spawn", &id, "/bin/sh", "-c", &script, "--socket", &sock],
            CLI_DEADLINE,
        );
        if !r.ok() {
            report.check(
                "eight panes are running their counters",
                false,
                &format!("spawn {id} exited {:?}: {r}", r.code),
            );
            return;
        }
    }

    // Let the counters climb before recording them: an update can only be shown to
    // leave a counter climbing if one was climbing already.
    std::thread::sleep(Duration::from_secs(4));

    let mut before = Vec::with_capacity(PANES);
    let mut before_raw = String::new();
    for n in 1..=PANES {
        let id = pane_id(n);
        let marker = format!("PANE-{n}-MARKER");
        let r = read_pane(&sandbox, &installed, &id, &sock);
        before_raw.push_str(&pane_evidence(&id, r.code, &r.output));
        before.push(parse_pane(&marker, &r.output));
    }
    report.panes_before = before_raw;

    // Precondition for the invariant: the fixture must be well-formed, i.e. every
    // pane alive with exactly one start behind it. Without this, a later "exactly
    // one start" assertion could pass on a pane that never started at all.
    let listing = run_cli(
        &sandbox,
        &installed,
        &["panes", "--socket", &sock],
        CLI_DEADLINE,
    );
    let alive = alive_panes(&listing.output);
    let all_up = (1..=PANES).all(|n| alive.contains(&pane_id(n)) && before[n - 1].markers == 1);
    report.check(
        "eight panes are running their counters",
        all_up,
        &format!("alive={alive:?} before={}", describe(&before)),
    );
    if !all_up {
        return;
    }
    report.say(format!(
        "update: pane counters before the update: {}",
        describe(&before)
    ));

    // ---- 1. the invariant ---------------------------------------------------
    let r = run_cli(
        &sandbox,
        &installed,
        &[
            "update",
            "--from",
            &candidate_path,
            "--no-reexec",
            "--socket",
            &sock,
        ],
        CLI_DEADLINE,
    );
    report.say(format!(
        "update: `update --from arreo-new --no-reexec` exited {:?} in {:.2} s: {}",
        r.code,
        r.elapsed.as_secs_f64(),
        first_line(&r.output)
    ));

    // (a) the daemon: never signalled, never reaped. `try_wait() == None` is both
    // halves at once — a daemon that had been signalled and reaped would report a
    // status here.
    let daemon_check = daemon.still_running();
    let same_pid = daemon.id == daemon_pid_before;
    report.check(
        "the daemon was never signalled or reaped",
        daemon_check.is_ok() && same_pid,
        &format!(
            "pid {daemon_pid_before} -> {} ({})",
            daemon.id,
            daemon_check
                .err()
                .unwrap_or_else(|| "still running, try_wait() is None".to_string())
        ),
    );

    // (b) and (c): the panes. The counters run at 1 Hz, so wait long enough that a
    // counter which was never interrupted must have moved past the value recorded
    // before the update.
    std::thread::sleep(Duration::from_secs(3));
    let mut after = Vec::with_capacity(PANES);
    let mut after_raw = String::new();
    let mut markers_ok = true;
    for n in 1..=PANES {
        let id = pane_id(n);
        let marker = format!("PANE-{n}-MARKER");
        let r = read_pane(&sandbox, &installed, &id, &sock);
        let read = parse_pane(&marker, &r.output);
        markers_ok &= read.markers >= 1;
        after_raw.push_str(&pane_evidence(&id, r.code, &r.output));
        after.push(read);
    }
    report.panes_after = after_raw;

    let listing = run_cli(
        &sandbox,
        &installed,
        &["panes", "--socket", &sock],
        CLI_DEADLINE,
    );
    let alive = alive_panes(&listing.output);
    let all_alive = (1..=PANES).all(|n| alive.contains(&pane_id(n)));
    report.check(
        "every pane is still alive and still printing its marker",
        all_alive && markers_ok,
        &format!("alive={alive:?} markers_ok={markers_ok}"),
    );

    // The restart proof. `tick-1` and the marker each appear exactly once: a
    // restarted process re-runs the script from the top and prints both again. The
    // highest tick is reported alongside because it is the number a reader expects
    // to see climb, but on its own it is not evidence — a restarted counter climbs
    // too.
    let restarted: Vec<String> = (1..=PANES)
        .filter(|n| {
            let (b, a) = (&before[n - 1], &after[n - 1]);
            !(a.markers == 1 && a.first_ticks == 1 && a.highest() > b.highest())
        })
        .map(|n| {
            format!(
                "pane-{n}: starts {}->{} tick-1 {}->{} tick {}->{}",
                before[n - 1].markers,
                after[n - 1].markers,
                before[n - 1].first_ticks,
                after[n - 1].first_ticks,
                before[n - 1].highest(),
                after[n - 1].highest()
            )
        })
        .collect();
    report.check(
        "no pane was restarted: every counter continued instead of resetting",
        restarted.is_empty(),
        &format!("restarted or stalled: {}", restarted.join("; ")),
    );
    report.say(format!(
        "update: pane counters after the update:  {}",
        describe(&after)
    ));

    // ---- 2. the swap happened ----------------------------------------------
    report.check(
        "the installed binary is now the candidate",
        same_bytes(&installed, &candidate).unwrap_or(false),
        &format!("{installed_path} vs {candidate_path}"),
    );
    let prev = root.join("arreo.prev");
    report.check(
        "the binary it replaced is kept beside it as .prev",
        same_bytes(&prev, &baseline).unwrap_or(false),
        &format!("{} vs {baseline_path}", prev.display()),
    );
    let swapped_version = run_cli(&sandbox, &installed, &["--version"], CLI_DEADLINE);
    report.check(
        "the installed binary runs",
        swapped_version.ok(),
        &format!(
            "exited {:?}: {}",
            swapped_version.code,
            swapped_version.output.trim()
        ),
    );
    let start_ok = run_cli(&sandbox, &installed, &["--version"], CLI_DEADLINE);
    let mut version_points = vec![("before the update", start_ok.ok())];

    // ---- 3. reattach -------------------------------------------------------
    //
    // A **second install**, holding the same bytes as the first one now does, so
    // that installing the third candidate is a real swap rather than the no-op the
    // verb correctly makes of installing bytes it already has. A hard link rather
    // than another 128 MB copy: this check needs a second *name* for those bytes,
    // not an independent copy of them, and the verb only ever renames — it never
    // writes through the name.
    let reattach_dir = root.join("reattach");
    let reattach_bin = reattach_dir.join("arreo");
    if let Err(e) =
        fs::create_dir_all(&reattach_dir).and_then(|()| fs::hard_link(&candidate, &reattach_bin))
    {
        report.check(
            "the reattach install can be prepared",
            false,
            &format!("{}: {e}", reattach_bin.display()),
        );
        return;
    }
    let r = run_cli(
        &sandbox,
        &reattach_bin,
        &[
            "update",
            "--from",
            &third_path,
            "--reattach-pane",
            "pane-1",
            "--socket",
            &sock,
        ],
        CLI_DEADLINE,
    );
    let resumed = r.output.lines().find(|l| l.contains("resumed pane"));
    let pane_came_with_it = r.output.contains("PANE-1-MARKER");
    report.check(
        "the update reattached the pane it was asked to resume",
        resumed.is_some() && pane_came_with_it,
        &format!(
            "no `resumed pane pane-1` line carrying the pane's output; it printed: {:?}",
            r.output.lines().take(6).collect::<Vec<_>>()
        ),
    );
    report.say(format!(
        "update: reattach: {:?} in {:.2} s (budget {:.0} s); the pane came back with {:?}",
        resumed.map(str::trim),
        r.elapsed.as_secs_f64(),
        REATTACH_BUDGET.as_secs_f64(),
        r.output
            .lines()
            .find(|l| l.contains("MARKER"))
            .map(str::trim)
    ));
    report.check(
        "the reattach finished inside its 2 s budget",
        r.ok() && r.elapsed < REATTACH_BUDGET,
        &format!("exited {:?} in {:.2} s", r.code, r.elapsed.as_secs_f64()),
    );
    let after_reattach = run_cli(&sandbox, &reattach_bin, &["--version"], CLI_DEADLINE);
    version_points.push(("after the reattach install", after_reattach.ok()));

    // ---- 4. rollback -------------------------------------------------------
    let r = run_cli(
        &sandbox,
        &installed,
        &["update", "--rollback", "--socket", &sock],
        CLI_DEADLINE,
    );
    report.check(
        "rollback restored the previous binary",
        r.ok() && same_bytes(&installed, &baseline).unwrap_or(false),
        &format!(
            "exited {:?} saying {:?}; {installed_path} vs {baseline_path}",
            r.code,
            first_line(&r.output)
        ),
    );
    let rolled_back_version = run_cli(&sandbox, &installed, &["--version"], CLI_DEADLINE);
    report.check(
        "the rolled-back binary runs",
        rolled_back_version.ok(),
        &format!("exited {:?}", rolled_back_version.code),
    );
    version_points.push(("after the rollback", rolled_back_version.ok()));

    // ---- 5. a refused candidate changes nothing ----------------------------
    //
    // A copy of the *third* candidate's bytes with the execute bit cleared, so the
    // refusal comes from the candidate and not from the verb finding it already
    // installed (that branch exits 0, correctly, and would be a green test of
    // nothing).
    let refused = root.join("arreo-noexec");
    if let Err(e) = copy_with_tail(&third, &refused, b"").and_then(|()| clear_execute(&refused)) {
        report.check("the refused candidate can be prepared", false, &e);
        return;
    }
    let refused_path = refused.display().to_string();
    // Nothing left by an earlier check may be mistaken for this one's leavings.
    let staged = root.join("arreo.staged");
    let _ = fs::remove_file(&staged);
    let r = run_cli(
        &sandbox,
        &installed,
        &[
            "update",
            "--from",
            &refused_path,
            "--no-reexec",
            "--socket",
            &sock,
        ],
        CLI_DEADLINE,
    );
    report.check(
        "a candidate with no execute bit is refused",
        !r.ok() && r.code.is_some(),
        &format!("exited {:?} saying {:?}", r.code, first_line(&r.output)),
    );
    report.say(format!(
        "update: the refusal was exit {:?}: {}",
        r.code,
        first_line(&r.output)
    ));
    report.check(
        "a refused candidate leaves the installed binary byte-identical",
        same_bytes(&installed, &baseline).unwrap_or(false),
        &format!("{installed_path} vs {baseline_path}"),
    );
    report.check(
        "a refused candidate leaves no .staged file behind",
        !staged.exists(),
        &format!("{} is present after the refusal", staged.display()),
    );
    let after_refusal = run_cli(&sandbox, &installed, &["--version"], CLI_DEADLINE);
    version_points.push(("after the refused candidate", after_refusal.ok()));

    // ---- 6. a second updater is refused ------------------------------------
    //
    // The lock is taken by *this* process, through the same `File::try_lock` the
    // verb uses, so the refusal under test is the real cross-process one rather
    // than a mock. The path is derived exactly as the verb derives it: beside the
    // binary, `<binary>.update.lock`.
    let lock_path = root.join("arreo.update.lock");
    match take_lock(&lock_path) {
        Ok(held) => {
            let r = run_cli(
                &sandbox,
                &installed,
                &[
                    "update",
                    "--from",
                    &candidate_path,
                    "--no-reexec",
                    "--socket",
                    &sock,
                ],
                CLI_DEADLINE,
            );
            report.check(
                "a second updater is refused with exit 3 and says why",
                r.code == Some(3) && r.output.contains("another update is already in progress"),
                &format!("exited {:?} saying {:?}", r.code, first_line(&r.output)),
            );
            report.say(format!(
                "update: with the lock held by this process, the second updater was exit {:?}: {}",
                r.code,
                first_line(&r.output)
            ));
            drop(held);
        }
        // Never a silent pass: the check is reported as skipped, with the reason,
        // and the summary counts it.
        Err(reason) => report.skip(
            "a second updater is refused with exit 3 and says why",
            &format!("this process could not take the OS lock: {reason}"),
        ),
    }
    let after_lock = run_cli(&sandbox, &installed, &["--version"], CLI_DEADLINE);
    version_points.push(("after the lock refusal", after_lock.ok()));

    // ---- 8. the channel: `--check`, and what it refuses --------------------
    //
    // A channel is a URL, and a `file://` one is a directory — the same transport
    // a self-hosted mirror inside a firewall serves. It is also why everything
    // here runs with no network: the sandbox points `ARREO_CHANNEL_URL` at an
    // empty `file://` channel precisely so no case in this slice can reach
    // GitHub by accident.
    let channel = root.join("channel");
    if let Err(e) = fs::create_dir_all(&channel) {
        report.check(
            "the channel directory can be made",
            false,
            &format!("{}: {e}", channel.display()),
        );
        return;
    }
    let channel_url = format!("file://{}/", channel.display());
    let default_channel = format!("file://{}/", root.join("channel-default").display());
    let check = |args: &[&str]| run_cli(&sandbox, &installed, args, CLI_DEADLINE);

    // (a) An empty channel: exit 0, and the answer said out loud. This is the
    // state the repository is in today, and a check that failed here would be
    // wrong every day until launch.
    let r = check(&["update", "--check", "--channel", &channel_url]);
    report.check(
        "an empty channel reports \"no releases yet\" with exit 0",
        r.ok() && r.output.contains("no releases yet") && r.output.contains(&channel_url),
        &format!("exited {:?} saying {:?}", r.code, first_line(&r.output)),
    );
    report.say(format!(
        "update: `--check` against an empty channel: exit {:?}, {:?}",
        r.code,
        first_line(&r.output)
    ));
    let r = check(&["update", "--check", "--channel", &channel_url, "--json"]);
    report.check(
        "`--check --json` answers with one machine-readable object",
        r.ok() && r.output.trim().starts_with('{') && r.output.contains("\"available\":false"),
        &format!("exited {:?} saying {:?}", r.code, first_line(&r.output)),
    );

    // (b) An index with no signature beside it is a **broken release**, not an
    // empty channel, and the verifier's own sentence names what it looked for.
    let index = channel.join("arreo-index.json");
    if let Err(e) = fs::write(&index, br#"{"version":"9.9.9","artifacts":{}}"#) {
        report.check(
            "the unsigned index fixture can be written",
            false,
            &format!("{}: {e}", index.display()),
        );
        return;
    }
    let r = check(&["update", "--check", "--channel", &channel_url]);
    report.check(
        "an unsigned index is refused with the verifier's sentence, naming the file",
        r.code == Some(1)
            && r.output.contains("no signature at")
            && r.output.contains("arreo-index.json.minisig")
            && r.output.contains(&channel_url),
        &format!("exited {:?} saying {:?}", r.code, first_line(&r.output)),
    );
    report.say(format!(
        "update: the unsigned index was refused: exit {:?}, {}",
        r.code,
        first_line(&r.output)
    ));

    // (c) A signature that is not a signature — corruption, or a file that was
    // never one — is refused as a bad signature, naming the file.
    if let Err(e) = fs::write(
        channel.join("arreo-index.json.minisig"),
        b"this is not a minisign signature\n",
    ) {
        report.check(
            "the bad-signature fixture can be written",
            false,
            &format!("{}: {e}", channel.display()),
        );
        return;
    }
    let r = check(&["update", "--check", "--channel", &channel_url]);
    report.check(
        "a signature that is not a signature is refused as a bad one",
        r.code == Some(1)
            && r.output.contains("signature does not authenticate")
            && r.output.contains("arreo-index.json.minisig"),
        &format!("exited {:?} saying {:?}", r.code, first_line(&r.output)),
    );

    // (d) **The anonymous update refuses before it installs anything.** No
    // `--from`, a channel whose index cannot be verified: the verb stops at the
    // index, prints the verifier's sentence, and leaves the install byte-identical
    // with nothing staged beside it.
    let r = run_cli(
        &sandbox,
        &installed,
        &["update", "--channel", &channel_url],
        CLI_DEADLINE,
    );
    report.check(
        "the anonymous update refuses before it installs anything",
        r.code == Some(1)
            && r.output.contains("signature does not authenticate")
            && same_bytes(&installed, &baseline).unwrap_or(false)
            && !staged.exists(),
        &format!(
            "exited {:?} saying {:?}; {installed_path} vs {baseline_path}: same={}",
            r.code,
            first_line(&r.output),
            same_bytes(&installed, &baseline).unwrap_or(false)
        ),
    );
    report.say(format!(
        "update: the anonymous update against the broken channel: exit {:?}, {}",
        r.code,
        first_line(&r.output)
    ));

    // (e) One precedence for the channel URL: the flag wins over the environment,
    // and the environment is read when there is no flag.
    let r = run_cli_env(
        &sandbox,
        &installed,
        &[("ARREO_CHANNEL_URL", &channel_url)],
        &["update", "--check", "--channel", &default_channel],
        CLI_DEADLINE,
    );
    report.check(
        "`--channel` wins over ARREO_CHANNEL_URL",
        r.ok() && r.output.contains("no releases yet"),
        &format!("exited {:?} saying {:?}", r.code, first_line(&r.output)),
    );
    let r = run_cli_env(
        &sandbox,
        &installed,
        &[("ARREO_CHANNEL_URL", &channel_url)],
        &["update", "--check"],
        CLI_DEADLINE,
    );
    report.check(
        "ARREO_CHANNEL_URL is read when no --channel is given",
        r.code == Some(1) && r.output.contains("signature does not authenticate"),
        &format!("exited {:?} saying {:?}", r.code, first_line(&r.output)),
    );

    // (f)+(g) The half that needs *signed* bytes. The workspace ships no signer
    // (T-0036: the key exists only as a CI secret), so these cases use the real
    // `minisign` when the machine has it and report a loud skip otherwise — never
    // a pass. `arreo-core`'s own tests assert the same accept path
    // deterministically, writing the format from `ed25519-dalek`.
    match minisign() {
        Some(minisign) => {
            let keys = root.join("channel-keys");
            let fixture = fs::create_dir_all(&keys)
                .map_err(|e| format!("{}: {e}", keys.display()))
                .and_then(|()| throwaway_keypair(&minisign, &keys));
            match fixture {
                Ok((_public, secret, public_text)) => {
                    // A signed index and a signed artifact, exactly as a release
                    // job publishes them.
                    let artifact = channel.join("arreo-candidate");
                    let signed_index = format!(
                        "{{\"version\":\"9.9.9\",\"artifacts\":{{\"{}\":\"arreo-candidate\"}}}}",
                        arreo_core::update::channel::host_target()
                    );
                    let prepared =
                        copy_with_tail(&cli_bin, &artifact, b"\n# the channel's candidate\n")
                            .and_then(|()| {
                                fs::write(&index, signed_index.as_bytes())
                                    .map_err(|e| format!("{}: {e}", index.display()))
                            })
                            .and_then(|()| sign(&minisign, &secret, &index))
                            .and_then(|()| sign(&minisign, &secret, &artifact));
                    if let Err(e) = prepared {
                        report.check("the signed channel fixture can be published", false, &e);
                    } else {
                        // (f) The product binary still refuses it: its trust set is
                        // the key compiled into it, and this key is not that key.
                        let r = check(&["update", "--check", "--channel", &channel_url]);
                        report.check(
                            "a real signature from a key this build does not trust is refused by key id",
                            r.code == Some(1)
                                && r.output.contains("signed by key")
                                && r.output.contains("this build trusts 076F2F7CEBE0AF51"),
                            &format!("exited {:?} saying {:?}", r.code, first_line(&r.output)),
                        );
                        report.say(format!(
                            "update: a throwaway minisign key's signature: exit {:?}, {}",
                            r.code,
                            first_line(&r.output)
                        ));

                        // (g) The accept path: the *same* signed bytes, checked
                        // through the channel code with the trust set the fixture was
                        // signed for — and then handed to the very install path
                        // `--from` uses, so a verified artifact becomes an install
                        // without a second copy of the install logic.
                        match arreo_core::update::verify::TrustSet::parse(&public_text) {
                            None => report.check(
                                "the throwaway public key parses",
                                false,
                                "minisign wrote a public key this build cannot parse",
                            ),
                            Some(trust) => match fetch_and_verify(&trust, &channel_url, &root) {
                                Err(e) => report.check(
                                    "a signed index is fetched and verified",
                                    false,
                                    &e,
                                ),
                                Ok((release, verified)) => {
                                    report.check(
                                        "a signed index is fetched, verified and reports its version and artifact",
                                        release.version == "9.9.9"
                                            && release.artifact == "arreo-candidate"
                                            && verified.artifact.ends_with("arreo-candidate"),
                                        &format!(
                                            "version {:?}, artifact {:?}, verified {}",
                                            release.version,
                                            release.artifact,
                                            verified.artifact.display()
                                        ),
                                    );
                                    let fetched = verified.artifact.display().to_string();
                                    let r = run_cli(
                                        &sandbox,
                                        &installed,
                                        &["update", "--from", &fetched, "--no-reexec"],
                                        CLI_DEADLINE,
                                    );
                                    report.check(
                                        "the verified artifact installs through the same path --from uses",
                                        r.ok()
                                            && same_bytes(&installed, &verified.artifact)
                                                .unwrap_or(false),
                                        &format!(
                                            "exited {:?} saying {:?}",
                                            r.code,
                                            first_line(&r.output)
                                        ),
                                    );
                                    report.say(format!(
                                        "update: the channel's verified artifact was installed through \
                                         --from: exit {:?}, {}",
                                        r.code,
                                        first_line(&r.output)
                                    ));
                                    let after_channel =
                                        run_cli(&sandbox, &installed, &["--version"], CLI_DEADLINE);
                                    version_points
                                        .push(("after the channel install", after_channel.ok()));
                                }
                            },
                        }
                    }
                }
                Err(e) => report.check("the throwaway keypair can be made", false, &e),
            }
        }
        None => {
            report.skip(
                "a real signature from a key this build does not trust is refused by key id",
                "minisign is not installed on this machine",
            );
            report.skip(
                "a signed index is fetched, verified and installed through the channel code",
                "minisign is not installed on this machine",
            );
        }
    }

    // ---- 9. runnable at every point ----------------------------------------
    //
    // The crash-safety *property* is unit-tested in `arreo-core` (a crash between
    // the swap's steps leaves the old or the new binary at the path, never
    // nothing); repeating that here would be a second, weaker copy of a test that
    // already exists. What this slice asserts instead is the end-to-end
    // consequence: at every point the slice looked, the path held a binary that
    // runs.
    report.check(
        "`--version` runs at every point the slice checked",
        version_points.iter().all(|(_, ok)| *ok),
        &version_points
            .iter()
            .map(|(point, ok)| format!("{point}: {}", if *ok { "runs" } else { "DOES NOT RUN" }))
            .collect::<Vec<_>>()
            .join(", "),
    );

    // ---- teardown ----------------------------------------------------------
    // Through the daemon's own verb, not a signal: a `kill(2)`ed daemon leaves the
    // panes' shell loops orphaned (that loop never exits on its own), and eight
    // spinning `sh` processes per run is a leak this slice would be creating.
    // `server stop` is SIGTERM, the drain path that takes the panes with it.
    daemon.stop();
    report.say(format!(
        "update: the daemon was stopped through its own verb (cleanly: {}) and the panes went \
         with it",
        daemon.stopped_cleanly
    ));
}

/// A verdict counter plus the transcript of what happened.
///
/// The transcript is a side effect of `say`/`check` rather than a second pass, so
/// the evidence file and the terminal cannot drift apart.
#[derive(Default)]
struct Report {
    passes: usize,
    skipped: usize,
    failures: usize,
    lines: Vec<String>,
    panes_before: String,
    panes_after: String,
}

impl Report {
    fn say(&mut self, line: String) {
        println!("{line}");
        self.lines.push(line);
    }

    fn check(&mut self, name: &str, ok: bool, detail: &str) {
        let line = if ok {
            self.passes += 1;
            format!("[PASS] update: {name}")
        } else {
            self.failures += 1;
            format!("[FAIL] update: {name}: {detail}")
        };
        self.say(line);
    }

    fn skip(&mut self, name: &str, reason: &str) {
        self.skipped += 1;
        self.say(format!("[SKIP] update: {name}: {reason}"));
    }

    /// The summary line, in the format the other slices use, and the exit status
    /// that goes with it: any failure at all is non-zero.
    fn finish(&self) -> ExitCode {
        let summary = format!(
            "update: {} passed, {} skipped, {} failed",
            self.passes, self.skipped, self.failures
        );
        println!("{summary}");
        if self.failures > 0 {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        }
    }

    fn transcript(&self) -> String {
        let mut out = self.lines.join("\n");
        out.push('\n');
        out.push_str(&format!(
            "update: {} passed, {} skipped, {} failed\n",
            self.passes, self.skipped, self.failures
        ));
        out
    }
}

/// The scratch directory, removed when it goes out of scope.
///
/// Its four binary copies are ~128 MB each, so a failed run must not leave half a
/// gigabyte in the temporary directory for the next one to trip over; the evidence
/// that matters is in the transcript.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The environment every process in this slice gets.
///
/// Everything that could name a real user's state is pointed inside the scratch
/// directory: the resume token this verb writes must not land in the developer's
/// own `~/.local/state`, and the slice must not depend on — or disturb — whatever
/// configuration happens to exist on the machine.
#[derive(Clone)]
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(root: PathBuf) -> Result<Self, String> {
        for dir in [
            "home",
            "state",
            "data",
            "config",
            "run",
            "arreo-state",
            "channel-default",
        ] {
            fs::create_dir_all(root.join(dir))
                .map_err(|e| format!("{}/{dir}: {e}", root.display()))?;
        }
        Ok(Self { root })
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
            .env("ARREO_STATE_DIR", self.root.join("arreo-state"))
            // The channel every invocation reads unless it is told otherwise: an
            // **empty `file://` directory** in the scratch. The anonymous update
            // is a real path now, and a slice that reached the built-in default
            // would dial GitHub — which is exactly the test-that-fails-on-a-plane
            // the task forbids.
            .env(
                "ARREO_CHANNEL_URL",
                format!("file://{}/", self.root.join("channel-default").display()),
            );
        command
    }
}

/// Where the slice puts its scratch. `ARREO_E2E_SCRATCH` overrides the temporary
/// directory, which matters because the four 128 MB copies below need real room — a
/// small or nearly-full `/tmp` would otherwise fail the slice for a reason that has
/// nothing to do with the update path.
fn scratch_root() -> Result<PathBuf, String> {
    let root = std::env::var_os("ARREO_E2E_SCRATCH")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(format!("arreo-e2e-update-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    Ok(root)
}

/// The daemon, held as a [`Child`] so its liveness is observable.
///
/// This is the half of the invariant that *is* checkable against a pid: the slice
/// spawned the daemon, so it can ask whether it ever exited, and whether the pid it
/// handed out is still the pid of the process it started.
struct Daemon {
    child: Child,
    id: u32,
    cli: PathBuf,
    sandbox: Sandbox,
    socket: PathBuf,
    log: PathBuf,
    stopped_cleanly: bool,
}

impl Daemon {
    fn spawn(
        sandbox: &Sandbox,
        server_bin: &Path,
        cli_bin: &Path,
        socket: &Path,
    ) -> Result<Self, String> {
        let log = sandbox.root.join("server.log");
        let out = File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
        let err = out
            .try_clone()
            .map_err(|e| format!("{}: {e}", log.display()))?;
        // The daemon's own output goes to a file rather than a pipe: a pipe nobody
        // drains fills up and the daemon dies on its next log line (EPIPE), which
        // reads exactly like the update having killed it.
        let child = sandbox
            .command(server_bin)
            .arg("--socket")
            .arg(socket)
            .stdin(Stdio::null())
            .stdout(Stdio::from(out))
            .stderr(Stdio::from(err))
            .spawn()
            .map_err(|e| format!("{}: {e}", server_bin.display()))?;
        let id = child.id();
        let daemon = Self {
            child,
            id,
            cli: cli_bin.to_path_buf(),
            sandbox: sandbox.clone(),
            socket: socket.to_path_buf(),
            log,
            stopped_cleanly: false,
        };
        if !daemon.wait_for_socket() {
            return Err(format!(
                "the daemon never bound {}; it said: {}",
                socket.display(),
                daemon.log_tail()
            ));
        }
        Ok(daemon)
    }

    /// Bounded, and a verdict rather than a panic: the shared harness's
    /// `wait_bound` asserts, and an assertion here would abandon the summary line
    /// and the evidence file a reader needs.
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

    /// Still running, or why not.
    ///
    /// `try_wait() == None` is the whole of the daemon half of the invariant: it
    /// means the process this slice started has not exited, so nothing signalled it
    /// to death and nothing reaped it out from under the slice.
    fn still_running(&mut self) -> Result<(), String> {
        match self.child.try_wait() {
            Ok(None) => Ok(()),
            Ok(Some(status)) => Err(format!("the daemon exited with {status}")),
            Err(e) => Err(format!("cannot ask after the daemon: {e}")),
        }
    }

    /// Stop the daemon the way an operator would, and take its panes with it.
    ///
    /// `kill(2)` would do the first half and not the second: a SIGKILLed daemon
    /// leaves every pane's process orphaned and still running, and this counter
    /// script never exits on its own. `arreo server stop` is SIGTERM, the drain
    /// path that stops the panes too.
    fn stop(&mut self) {
        if self.still_running().is_err() {
            return;
        }
        let socket = self.socket.display().to_string();
        let stopped = run_cli(
            &self.sandbox,
            &self.cli,
            &["server", "stop", "--socket", &socket],
            Duration::from_secs(15),
        );
        self.stopped_cleanly = stopped.ok();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        // The drain did not finish in time. A backstop, and a reported one: the
        // caller says whether the graceful path worked.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    fn log_tail(&self) -> String {
        let text = fs::read_to_string(&self.log).unwrap_or_default();
        let lines: Vec<&str> = text.lines().rev().take(10).collect();
        lines.into_iter().rev().collect::<Vec<_>>().join("\n")
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What one `arreo` invocation did, bounded by a deadline.
struct Run {
    /// The exit code, or `None` when the command was killed for running past its
    /// deadline or died on a signal.
    code: Option<i32>,
    output: String,
    elapsed: Duration,
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

/// Run a binary, bounded: a command that runs past `deadline` is killed and
/// reported as a failure that carries what it had printed.
fn run_cli(sandbox: &Sandbox, bin: &Path, args: &[&str], deadline: Duration) -> Run {
    run_cli_env(sandbox, bin, &[], args, deadline)
}

/// The same, with extra environment: how the channel precedence is tested.
fn run_cli_env(
    sandbox: &Sandbox,
    bin: &Path,
    env: &[(&str, &str)],
    args: &[&str],
    deadline: Duration,
) -> Run {
    let start = Instant::now();
    let mut command = sandbox.command(bin);
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = match command
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
                elapsed: start.elapsed(),
            }
        }
    };

    // Both pipes are drained from their own threads. A child that fills a pipe
    // while this loop waits for it to exit would deadlock, and a deadlocked slice is
    // precisely what the deadline exists to prevent — so the wait must not be able
    // to block on the child's output.
    let stdout_drain = drain(child.stdout.take().expect("stdout was piped"));
    let stderr_drain = drain(child.stderr.take().expect("stderr was piped"));

    let (code, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status.code(), false),
            Ok(None) if start.elapsed() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                // Kill, then still collect what it wrote: a timeout with no output
                // says nothing about *where* the command stalled, and the partial
                // output is the only evidence there is.
                let _ = child.kill();
                let _ = child.wait();
                break (None, true);
            }
            // Cannot be waited for at all: a failure rather than spinning here.
            Err(_) => break (None, true),
        }
    };

    let mut output = stdout_drain.join().unwrap_or_default();
    output.push_str(&stderr_drain.join().unwrap_or_default());
    if timed_out {
        output = format!("timed out after {deadline:?}; it had printed: {output:?}");
    }
    Run {
        code,
        output,
        elapsed: start.elapsed(),
    }
}

/// Read a pipe to the end on its own thread.
fn drain<R: Read + Send + 'static>(mut stream: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stream.read_to_end(&mut buf);
        String::from_utf8_lossy(&buf).into_owned()
    })
}

/// One `arreo read` against the daemon, bounded like every other invocation.
fn read_pane(sandbox: &Sandbox, bin: &Path, id: &str, socket: &str) -> Run {
    run_cli(
        sandbox,
        bin,
        &["read", id, "--socket", socket],
        CLI_DEADLINE,
    )
}

/// What one pane's read says about its counter.
#[derive(Default, Clone)]
struct PaneRead {
    /// Every `tick-N` line, in order.
    ticks: Vec<u64>,
    /// How many times the pane printed its start marker.
    markers: usize,
    /// How many times it printed `tick-1`.
    first_ticks: usize,
}

impl PaneRead {
    fn highest(&self) -> u64 {
        self.ticks.iter().copied().max().unwrap_or(0)
    }
}

/// Read a pane's scrollback into the three facts the invariant needs.
///
/// Whole lines, never substrings: `tick-1` is a prefix of `tick-10`, and a
/// substring count would report a restart on a pane that had merely been running
/// for ten seconds.
fn parse_pane(marker: &str, text: &str) -> PaneRead {
    let mut read = PaneRead::default();
    for line in text.lines() {
        let line = line.trim_end();
        if line == marker {
            read.markers += 1;
        } else if let Some(rest) = line.strip_prefix("tick-") {
            if let Ok(n) = rest.trim().parse::<u64>() {
                if n == 1 {
                    read.first_ticks += 1;
                }
                read.ticks.push(n);
            }
        }
    }
    read
}

/// The pane ids the daemon reports as alive.
fn alive_panes(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let id = fields.next()?;
            let state = fields.next()?;
            (state == "alive").then(|| id.to_string())
        })
        .collect()
}

/// Per-pane counter evidence, dense enough to read in a transcript: the highest
/// tick, how many times the pane started, and how many times it printed `tick-1`.
fn describe(reads: &[PaneRead]) -> String {
    reads
        .iter()
        .enumerate()
        .map(|(i, read)| {
            format!(
                "pane-{} tick {} (starts {}, tick-1 {})",
                i + 1,
                read.highest(),
                read.markers,
                read.first_ticks
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// One pane's raw read, for the evidence file a reviewer reads line by line.
fn pane_evidence(id: &str, code: Option<i32>, output: &str) -> String {
    let newline = if output.ends_with('\n') { "" } else { "\n" };
    format!("--- {id} (read exited {code:?}) ---\n{output}{newline}")
}

fn first_line(text: &str) -> String {
    text.lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Are these two files byte-identical?
///
/// Length first, then 64 KiB at a time: these are ~128 MB binaries, and reading
/// two of them fully into memory to answer a yes/no question would allocate a
/// quarter of a gigabyte to compare them.
fn same_bytes(a: &Path, b: &Path) -> Result<bool, String> {
    let mut left = File::open(a).map_err(|e| format!("{}: {e}", a.display()))?;
    let mut right = File::open(b).map_err(|e| format!("{}: {e}", b.display()))?;
    let (la, lb) = (
        left.metadata()
            .map_err(|e| format!("{}: {e}", a.display()))?
            .len(),
        right
            .metadata()
            .map_err(|e| format!("{}: {e}", b.display()))?
            .len(),
    );
    if la != lb {
        return Ok(false);
    }
    let (mut buf_a, mut buf_b) = ([0u8; 64 * 1024], [0u8; 64 * 1024]);
    loop {
        let (n, m) = (
            fill(&mut left, &mut buf_a).map_err(|e| format!("{}: {e}", a.display()))?,
            fill(&mut right, &mut buf_b).map_err(|e| format!("{}: {e}", b.display()))?,
        );
        if n != m {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
        if buf_a[..n] != buf_b[..m] {
            return Ok(false);
        }
    }
}

/// Read until `buf` is full or the file ends. `read` may return short, and taking a
/// short read for the end of the file would compare two files only as far as the
/// first pipe buffer.
fn fill(file: &mut File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Copy a binary, optionally appending bytes.
///
/// `fs::copy` carries the mode across, which is what keeps the copy runnable; the
/// tail is what makes two candidates out of one binary (an ELF ignores trailing
/// bytes).
fn copy_with_tail(source: &Path, destination: &Path, tail: &[u8]) -> Result<(), String> {
    fs::copy(source, destination).map_err(|e| format!("copy to {}: {e}", destination.display()))?;
    if tail.is_empty() {
        return Ok(());
    }
    use std::io::Write;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(destination)
        .map_err(|e| format!("{}: {e}", destination.display()))?;
    file.write_all(tail)
        .map_err(|e| format!("{}: {e}", destination.display()))
}

/// Clear the execute bits, so the candidate cannot be run.
///
/// Unix decides this with a mode bit; a platform that decides by extension cannot
/// express "present but not runnable" this way, and says so instead of quietly
/// producing a candidate the verb would happily install.
fn clear_execute(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut permissions = meta.permissions();
        permissions.set_mode(permissions.mode() & !0o111);
        fs::set_permissions(path, permissions).map_err(|e| format!("{}: {e}", path.display()))
    }
    #[cfg(not(unix))]
    {
        Err(format!(
            "{}: this platform has no execute bit to clear, so a candidate that is present but \
             not runnable cannot be built here",
            path.display()
        ))
    }
}

/// Take the update lock the way the verb takes it: an OS lock on a file beside the
/// binary, held by this process's open file description.
///
/// Any failure to take it is returned as a reason, so the caller can skip the
/// concurrency check loudly instead of reporting a pass it never observed.
fn take_lock(path: &Path) -> Result<File, String> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(format!(
            "{} is already locked by someone else",
            path.display()
        )),
        Err(std::fs::TryLockError::Error(e)) => Err(format!(
            "no OS lock is available on this platform for {}: {e}",
            path.display()
        )),
    }
}

/// Fetch and verify the channel's newest release with the trust set the fixture
/// was signed for.
///
/// This is the anonymous update's fetch half, driven through the real channel
/// code — the only part the product binary cannot exercise in a test, because its
/// trust set is the key compiled into it, and the secret of that key is nowhere
/// on this machine (T-0036's precedent: the accept path is proved against keys
/// whose secrets are in the test's hand).
fn fetch_and_verify(
    trust: &arreo_core::update::verify::TrustSet,
    url: &str,
    root: &Path,
) -> Result<
    (
        arreo_core::update::channel::Release,
        arreo_core::update::verify::Verified,
    ),
    String,
> {
    let channel = arreo_core::update::channel::Channel::new(url).map_err(|e| e.to_string())?;
    let work = root.join("channel-work");
    let release = arreo_core::update::channel::check(trust, &channel, &work)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "the signed channel reports no release".to_string())?;
    let verified = arreo_core::update::channel::fetch(trust, &channel, &release, &work)
        .map_err(|e| e.to_string())?;
    Ok((release, verified))
}

/// `minisign`, if this machine has it.
///
/// The workspace ships no signer (that is the T-0036 decision: the key exists
/// only as a CI secret), so a case that needs *signed* bytes outside the library's
/// own tests needs the real tool. When it is absent the case reports a loud skip
/// naming what is missing, never a pass — the T-0019 no-delegation precedent.
fn minisign() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("minisign"))
        .find(|candidate| candidate.is_file())
}

/// A throwaway minisign keypair in `dir`, plus its public key text.
///
/// `-W` makes the key unencrypted, which is the shape CI keys are made in and the
/// shape a test can feed to `-S` without a password prompt.
fn throwaway_keypair(minisign: &Path, dir: &Path) -> Result<(PathBuf, PathBuf, String), String> {
    let public = dir.join("throwaway.pub");
    let secret = dir.join("throwaway.key");
    let generated = Command::new(minisign)
        .arg("-G")
        .arg("-W")
        .arg("-p")
        .arg(&public)
        .arg("-s")
        .arg(&secret)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("running minisign -G: {e}"))?;
    if !generated.status.success() {
        return Err(format!(
            "minisign -G failed: {}",
            String::from_utf8_lossy(&generated.stderr).trim()
        ));
    }
    let text = fs::read_to_string(&public).map_err(|e| format!("{}: {e}", public.display()))?;
    Ok((public, secret, text))
}

/// Sign `file` with `secret`, writing the sibling `.minisig` a release job would
/// ship.
fn sign(minisign: &Path, secret: &Path, file: &Path) -> Result<(), String> {
    let mut child = Command::new(minisign)
        .arg("-S")
        .arg("-s")
        .arg(secret)
        .arg("-m")
        .arg(file)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("running minisign -S: {e}"))?;
    // minisign reads a password from stdin unless the key is unencrypted; one
    // empty line is what its own release job feeds for the same shape.
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write as _;
        let _ = stdin.write_all(b"\n");
    }
    let output = child
        .wait_with_output()
        .map_err(|e| format!("waiting for minisign -S: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "minisign -S failed on {}: {}",
            file.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(())
}
