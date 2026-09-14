//! T-0091: `arreo spawn --worktree` and `arreo worktrees list|remove`, end to end
//! on the real binaries.
//!
//! The claim is behavioural, not structural. Two panes are given a worktree each
//! of one real repository and told to write **the same filename** in it, so a
//! daemon that quietly started both in one directory passes every log line and
//! fails on the two files' contents. What the slice asserts is read back from
//! outside the product — `git worktree list --porcelain`, the files the panes
//! wrote, `git status --porcelain` in the main checkout, the exit codes and the
//! words the CLI printed — because a check the code under test can satisfy by
//! being asked is not a check.
//!
//! Hermetic: the daemon, the CLI and every `git` run in one scratch directory
//! (`target/test-scratch/T-0091-<pid>`, removed on the way out) with `HOME` and
//! every XDG variable pointed inside it, against a repository the slice makes and
//! commits. The daemon is stopped the way an operator stops one, so the run
//! leaves no pane behind, and nothing outside the scratch is read or written.
//!
//! The order is the feature's story, and it is load-bearing: the two panes have
//! deliberately different lifetimes, so the refusal that names a dirty checkout
//! (the first pane, gone) is proved before the refusal that names a live agent
//! (the second, still working), and both before the removal that goes through.

use crate::harness::bins;
use crate::update_slice::{
    alive_panes, first_line, run_cli, Daemon, Report, Run, Sandbox, Scratch, CLI_DEADLINE,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// The pane that exits first, whose checkout is the dirty one: the removal that
/// is refused for what is *in* the directory.
const SHORT_PANE: &str = "one";
/// The pane that outlives it, whose removal is refused for who is *in* the
/// directory — the one question `git` cannot answer.
const LONG_PANE: &str = "two";

/// The word the first pane writes into its copy of [`TASK_FILE`].
const WORD_ONE: &str = "first";
/// And the second pane's. Different words, same filename: that is the whole test.
const WORD_TWO: &str = "second";

/// The filename both panes write. One file in one shared directory cannot hold
/// two words, so this is what a worktree that was really a `cd` fails on.
const TASK_FILE: &str = "task.txt";
/// Where each pane records its own working directory, for the cwd check.
const WHERE_FILE: &str = "where.txt";

/// How long the first pane stays up. Long enough that its files exist and its
/// `pwd` has been read (checks 3–5) before it exits, short enough that the wait
/// for it to be *gone* is a second or two rather than a timeout.
const SHORT_LIFE: u64 = 5;
/// The second pane's, which has to outlive the first by enough that check 7 can
/// ask for its removal while it is still working.
const LONG_LIFE: u64 = 12;

/// How long any single wait may take. Every one of them is a poll against a real
/// process, so the bound is generous and the failure is a named verdict rather
/// than a stall.
const WAIT: Duration = Duration::from_secs(10);

pub fn run(_rest: &[String]) -> ExitCode {
    // No options: the slice is the proof, and every step of it is required.
    let mut report = Report::new("worktree: ");
    slice(&mut report);
    report.finish()
}

fn slice(report: &mut Report) {
    let (server_bin, cli_bin, _tui_bin) = bins();
    let root = match scratch_root() {
        Ok(root) => root,
        Err(e) => {
            report.check("fixtures", false, &e);
            return;
        }
    };
    // Declared before the daemon: `Drop` runs in reverse declaration order, so
    // the daemon is stopped before the directory it lives in is removed.
    let _scratch = Scratch(root.clone());
    let sandbox = match Sandbox::new(root.clone()) {
        Ok(sandbox) => sandbox,
        Err(e) => {
            report.check("fixtures", false, &e);
            return;
        }
    };
    let layout = Layout::new(root);

    // 1. The fixtures: the two real binaries, one real repository with one
    //    commit, the `[worktree]` section both processes read, and the clean main
    //    checkout everything below is measured against.
    let fixture = match fixtures(&sandbox, &layout, &server_bin, &cli_bin) {
        Ok(fixture) => fixture,
        Err(e) => {
            report.check("fixtures", false, &e);
            return;
        }
    };
    report.check("fixtures", true, &fixture.detail);

    // The daemon reads `[worktree]` from `$ARREO_CONFIG` — and the shared
    // `Daemon` starts the server the way every other slice starts it, socket
    // only, with no arguments of ours — so the environment is where the setting
    // goes. Set once, before anything is spawned, so no two processes can
    // resolve different files. The CLI reads the same variable for the root
    // (`--config` first, then this), and is given `--repo` on every call because
    // its repository default is its working directory, which is the sandbox and
    // not the repository.
    std::env::set_var("ARREO_CONFIG", &layout.config);
    let daemon = match Daemon::spawn(&sandbox, &server_bin, &cli_bin, &layout.socket) {
        Ok(daemon) => daemon,
        Err(e) => {
            report.check("two panes, two worktrees", false, &e);
            return;
        }
    };
    report.say(format!(
        "worktree: a real daemon (pid {}) on {}, worktrees under {}, from the one commit in {}",
        daemon.id,
        layout.socket.display(),
        layout.worktrees.display(),
        layout.repo.display()
    ));

    // 2. Two panes, each in a checkout of its own. `--worktree` with no name
    //    means "name it after the pane id", so the branches are `arreo/one` and
    //    `arreo/two` and nothing has to be kept in step by hand.
    let first = spawn_pane(
        &sandbox,
        &cli_bin,
        &layout,
        SHORT_PANE,
        &pane_script(WORD_ONE, SHORT_LIFE),
    );
    let second = spawn_pane(
        &sandbox,
        &cli_bin,
        &layout,
        LONG_PANE,
        &pane_script(WORD_TWO, LONG_LIFE),
    );
    let listed =
        git(&sandbox, &layout.repo, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    let entries = porcelain_worktrees(&listed);
    let ours = entries
        .iter()
        .filter(|(_, branch)| branch.starts_with("arreo/"))
        .count();
    let one_path = entry_path(&entries, &branch_of(SHORT_PANE));
    let two_path = entry_path(&entries, &branch_of(LONG_PANE));
    report.check(
        "two panes, two worktrees",
        first.is_ok() && second.is_ok() && ours == 2 && one_path.is_some() && two_path.is_some(),
        &format!(
            "{}; {}; `git worktree list --porcelain` names {} — {ours} of them arreo/*",
            said(&first),
            said(&second),
            branches(&entries)
        ),
    );
    let (Ok(_), Ok(_), Some(one_path), Some(two_path)) = (first, second, one_path, two_path) else {
        // Without two checkouts there is nothing to read, and the failure above
        // is the verdict; the rest would only restate it.
        return;
    };

    // The two listings are taken here, while both panes exist and before
    // anything is removed: `list` has to be a statement about the pair, and check
    // 9 reports what it said. The second asks a socket nothing is listening on —
    // the same moment and the same worktrees, one daemon away.
    let json = worktrees(
        &sandbox,
        &cli_bin,
        &layout,
        &layout.socket_arg(),
        &["list", "--json"],
    );
    let absent = layout.absent_socket_arg();
    let offline = worktrees(&sandbox, &cli_bin, &layout, &absent, &["list", "--json"]);

    // 3. Disjoint file edits: the same filename in two checkouts, each holding
    //    its own pane's word.
    let (one_task, one_word) = wait_for_word(&one_path.join(TASK_FILE), WORD_ONE, WAIT);
    let (two_task, two_word) = wait_for_word(&two_path.join(TASK_FILE), WORD_TWO, WAIT);
    report.check(
        "disjoint file edits",
        one_task && two_task,
        &format!(
            "{}/{TASK_FILE} holds {one_word:?} (want {WORD_ONE:?}); {}/{TASK_FILE} holds \
             {two_word:?} (want {WORD_TWO:?})",
            one_path.display(),
            two_path.display()
        ),
    );

    // 4. The child's own cwd: what `pwd` printed inside the pane, against the
    //    path git reports for that pane's branch. The pane is *in* its checkout,
    //    not merely near one.
    let one_want = one_path.display().to_string();
    let two_want = two_path.display().to_string();
    let (one_pwd_ok, one_pwd) = wait_for_word(&one_path.join(WHERE_FILE), &one_want, WAIT);
    let (two_pwd_ok, two_pwd) = wait_for_word(&two_path.join(WHERE_FILE), &two_want, WAIT);
    report.check(
        "the child's own cwd",
        one_pwd_ok && two_pwd_ok,
        &format!(
            "one's `pwd` is {one_pwd:?} (git says {one_want:?}); two's `pwd` is {two_pwd:?} (git \
             says {two_want:?})"
        ),
    );

    // 5. The main checkout is untouched: the same `git status --porcelain` it
    //    had before anything ran, and neither pane's file in it.
    let after = git(&sandbox, &layout.repo, &["status", "--porcelain"]).unwrap_or_else(|e| e);
    let stray: Vec<&str> = [TASK_FILE, WHERE_FILE]
        .into_iter()
        .filter(|name| layout.repo.join(name).exists())
        .collect();
    report.check(
        "the main checkout is untouched",
        after.trim() == fixture.baseline && stray.is_empty(),
        &format!(
            "`git status --porcelain` in {} is {:?} (it was {:?}); {} in the main checkout",
            layout.repo.display(),
            after.trim(),
            fixture.baseline,
            if stray.is_empty() {
                format!("neither {TASK_FILE} nor {WHERE_FILE} is")
            } else {
                format!("{} is", stray.join(" and "))
            }
        ),
    );

    // 6. A dirty remove is refused, and the refusal names the files. The first
    //    pane has to be *gone* first: while it is alive the refusal is about
    //    liveness, which is the next check's subject. An exited shell leaves its
    //    files behind — nothing but a removal cleans a checkout — so the
    //    directory is dirty exactly as an operator would find it.
    let (one_gone, one_state) =
        wait_for_liveness(&sandbox, &cli_bin, &layout, SHORT_PANE, false, WAIT);
    let refused = worktrees(
        &sandbox,
        &cli_bin,
        &layout,
        &layout.socket_arg(),
        &["remove", SHORT_PANE],
    );
    report.check(
        "a dirty remove is refused, and the refusal names the files",
        one_gone
            && !refused.ok()
            && refused.output.contains(TASK_FILE)
            && !refused.output.contains("is live")
            && one_path.exists(),
        &format!(
            "{one_state}; `worktrees remove {SHORT_PANE}` exited {:?} saying {:?}; {} is still \
             there",
            refused.code,
            first_line(&refused.output),
            one_path.display()
        ),
    );

    // 7. The live refusal: the second pane is still working, so its removal is
    //    refused for the one reason git cannot see. Its checkout is dirty too —
    //    both refusals apply — and liveness has to be the one that wins, or an
    //    operator would be told to commit their way out of a running agent.
    let (two_alive, two_state) =
        wait_for_liveness(&sandbox, &cli_bin, &layout, LONG_PANE, true, WAIT);
    let live = worktrees(
        &sandbox,
        &cli_bin,
        &layout,
        &layout.socket_arg(),
        &["remove", LONG_PANE],
    );
    report.check(
        "the live refusal",
        two_alive
            && !live.ok()
            && live.output.contains("is live")
            && !live.output.contains(TASK_FILE)
            && two_path.exists(),
        &format!(
            "{two_state}; `worktrees remove {LONG_PANE}` exited {:?} saying {:?}; {} is still there",
            live.code,
            first_line(&live.output),
            two_path.display()
        ),
    );

    // 8. The removal that goes through: both panes have exited, and the forced
    //    removal takes the first pane's checkout away while the branch, which
    //    holds the commits, stays. `--force` because an exited shell leaves its
    //    files behind — check 6 is where those files were proved to be there.
    let (two_gone, two_gone_state) =
        wait_for_liveness(&sandbox, &cli_bin, &layout, LONG_PANE, false, WAIT);
    let removed = worktrees(
        &sandbox,
        &cli_bin,
        &layout,
        &layout.socket_arg(),
        &["remove", SHORT_PANE, "--force"],
    );
    let branch = git(
        &sandbox,
        &layout.repo,
        &["branch", "--list", &branch_of(SHORT_PANE)],
    )
    .unwrap_or_default();
    let left =
        git(&sandbox, &layout.repo, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    let left = porcelain_worktrees(&left);
    let still_listed = entry_path(&left, &branch_of(SHORT_PANE)).is_some();
    report.check(
        "a clean remove is done",
        two_gone
            && removed.ok()
            && !one_path.exists()
            && !branch.trim().is_empty()
            && !still_listed,
        &format!(
            "{two_gone_state}; `worktrees remove {SHORT_PANE} --force` exited {:?} saying {:?}; {} \
             exists: {}; `git branch --list {}` says {:?}; `git worktree list --porcelain` names {}",
            removed.code,
            first_line(&removed.output),
            one_path.display(),
            one_path.exists(),
            branch_of(SHORT_PANE),
            branch.trim(),
            branches(&left)
        ),
    );

    // 9. `list` reports what git says: the JSON taken above, while both panes
    //    existed, and then the case the verb exists for — no daemon to ask, so
    //    liveness is unknown and the listing is still a listing.
    let (listing, _) = split_listing(&json.output);
    let shape = listing.starts_with('{')
        && listing.ends_with('}')
        && listing.contains("\"schema\":1")
        && listing.contains("\"pane\":\"one\"")
        && listing.contains("\"branch\":\"arreo/one\"")
        && listing.contains("\"pane\":\"two\"")
        && listing.contains("\"branch\":\"arreo/two\"")
        && listing.contains("\"live\":true");
    let (off_listing, off_notes) = split_listing(&offline.output);
    let unknown = off_listing.matches("\"live\":null").count();
    let offline_ok = offline.ok()
        && off_notes.contains("liveness unknown")
        && off_listing.contains("\"pane\":\"one\"")
        && unknown == 2;
    report.check(
        "list reports what git says",
        json.ok() && shape && offline_ok,
        &format!(
            "with the daemon up: exit {:?}, {listing:?}; with nothing listening on {absent}: exit \
             {:?}, {:?} on stderr and {unknown} rows saying \"live\":null",
            json.code,
            offline.code,
            first_line(off_notes)
        ),
    );
}

/// Where everything in this slice lives: all of it inside one scratch directory,
/// which [`Scratch`] removes on the way out.
struct Layout {
    root: PathBuf,
    /// The repository the worktrees are checkouts of, and the main checkout the
    /// "untouched" check is about.
    repo: PathBuf,
    /// `[worktree] root`: where the per-pane checkouts go. Deliberately **not**
    /// inside the repository — worktrees under their own repository are the
    /// arrangement in which a worktree that was really a `cd` looks right.
    worktrees: PathBuf,
    /// The `[worktree]` section as a file. The daemon is given it through
    /// `$ARREO_CONFIG` and the CLI reads the same variable, so both processes
    /// resolve one root and one repository.
    config: PathBuf,
    socket: PathBuf,
}

impl Layout {
    fn new(root: PathBuf) -> Self {
        Self {
            repo: root.join("repo"),
            worktrees: root.join("worktrees"),
            config: root.join("arreo.toml"),
            socket: root.join("arreo.sock"),
            root,
        }
    }

    fn write_config(&self) -> Result<(), String> {
        let body = format!(
            "[worktree]\nroot = {:?}\nrepo = {:?}\n",
            self.worktrees.display().to_string(),
            self.repo.display().to_string()
        );
        fs::write(&self.config, body).map_err(|e| format!("{}: {e}", self.config.display()))
    }

    fn socket_arg(&self) -> String {
        self.socket.display().to_string()
    }

    fn repo_arg(&self) -> String {
        self.repo.display().to_string()
    }

    /// A socket path nothing is listening on: the "no daemon reachable" case,
    /// which `list` has to survive rather than fail.
    fn absent_socket_arg(&self) -> String {
        self.root.join("no-daemon.sock").display().to_string()
    }
}

/// The scratch directory, named after the task and this process so two runs at
/// once cannot collide.
///
/// Under the workspace's `target/`, never `/tmp`: a stray daemon or pane is
/// `pkill -f target/test-scratch` away, and a slice that leaves one behind
/// leaves it where the next run looks.
fn scratch_root() -> Result<PathBuf, String> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("xtask lives one level below the workspace root")?
        .join("target")
        .join("test-scratch");
    let root = base.join(format!("T-0091-{}", std::process::id()));
    // A leftover from a run that was killed outright would make the fixtures
    // fail in confusing ways. `Scratch` removes the directory on the way out;
    // this is the belt for those braces.
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    Ok(root)
}

/// What every later check stands on.
struct Fixtures {
    /// `git status --porcelain` in the main checkout, before any pane exists.
    baseline: String,
    /// The same facts, as the observed values check 1 prints.
    detail: String,
}

fn fixtures(
    sandbox: &Sandbox,
    layout: &Layout,
    server: &Path,
    cli: &Path,
) -> Result<Fixtures, String> {
    if !server.is_file() {
        return Err(format!(
            "{} is not built (`cargo build` first)",
            server.display()
        ));
    }
    if !cli.is_file() {
        return Err(format!(
            "{} is not built (`cargo build` first)",
            cli.display()
        ));
    }
    layout.write_config()?;
    make_repo(sandbox, &layout.repo)?;
    let commits = git(sandbox, &layout.repo, &["rev-list", "--count", "HEAD"])?
        .trim()
        .to_string();
    if commits != "1" {
        return Err(format!(
            "{} has {commits} commits; the fixture is one",
            layout.repo.display()
        ));
    }
    let baseline = git(sandbox, &layout.repo, &["status", "--porcelain"])?
        .trim()
        .to_string();
    if !baseline.is_empty() {
        return Err(format!(
            "the main checkout is not clean before anything runs: {baseline:?}"
        ));
    }
    Ok(Fixtures {
        detail: format!(
            "{} and {} are built; {} is a repository with {commits} commit on main and \
             `git status --porcelain` {:?}; [worktree] root={} repo={}",
            server.display(),
            cli.display(),
            layout.repo.display(),
            baseline,
            layout.worktrees.display(),
            layout.repo.display()
        ),
        baseline,
    })
}

/// One real repository with one commit: the checkout every worktree is made from.
///
/// The identity is set locally, so the commit does not depend on — or read —
/// whatever the machine's git configuration happens to say.
fn make_repo(sandbox: &Sandbox, repo: &Path) -> Result<(), String> {
    fs::create_dir_all(repo).map_err(|e| format!("{}: {e}", repo.display()))?;
    git(sandbox, repo, &["init", "-q", "-b", "main"])?;
    git(sandbox, repo, &["config", "user.name", "arreo e2e"])?;
    git(
        sandbox,
        repo,
        &["config", "user.email", "e2e@arreo.invalid"],
    )?;
    fs::write(repo.join("README.md"), "the main checkout\n")
        .map_err(|e| format!("{}: {e}", repo.display()))?;
    git(sandbox, repo, &["add", "README.md"])?;
    git(sandbox, repo, &["commit", "-q", "-m", "initial"])?;
    Ok(())
}

/// Run `git` in `dir`, bounded, in the sandbox's environment.
///
/// Through [`run_cli`] rather than `Command::output()`: the deadline and the
/// sandboxed `HOME` are then the same ones every other invocation gets, so `git`
/// cannot read the developer's `~/.gitconfig` or write to their repository.
fn git(sandbox: &Sandbox, dir: &Path, args: &[&str]) -> Result<String, String> {
    let at = dir.display().to_string();
    let mut argv = vec!["-C", at.as_str()];
    argv.extend_from_slice(args);
    let run = run_cli(sandbox, Path::new("git"), &argv, CLI_DEADLINE);
    if !run.ok() {
        return Err(format!(
            "`git {}` in {at} exited {:?}: {}",
            args.join(" "),
            run.code,
            first_line(&run.output)
        ));
    }
    Ok(run.output)
}

/// `(path, branch)` for every worktree `git worktree list --porcelain` reports,
/// with the branch stripped of `refs/heads/`.
///
/// Parsed here rather than through `arreo_core::worktree::parse_porcelain` so
/// the proof reads git's own words instead of the product's reading of them.
fn porcelain_worktrees(text: &str) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            if let Some(previous) = path.take() {
                out.push((previous, std::mem::take(&mut branch)));
            }
            path = Some(PathBuf::from(rest));
        } else if let Some(rest) = line.strip_prefix("branch ") {
            branch = rest.strip_prefix("refs/heads/").unwrap_or(rest).to_string();
        }
    }
    if let Some(previous) = path {
        out.push((previous, branch));
    }
    out
}

/// The checkout git reports for `branch`, if it reports one.
fn entry_path(entries: &[(PathBuf, String)], branch: &str) -> Option<PathBuf> {
    entries
        .iter()
        .find(|(_, named)| named == branch)
        .map(|(path, _)| path.clone())
}

/// Every branch `git worktree list` named, for a detail line.
fn branches(entries: &[(PathBuf, String)]) -> String {
    entries
        .iter()
        .map(|(_, branch)| branch.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The branch a pane's worktree is on: `arreo/<pane>`.
///
/// Spelled out here rather than asked of `arreo_core::worktree::branch_for`,
/// because the check asserts the documented name and not whatever the code
/// happens to do.
fn branch_of(pane: &str) -> String {
    format!("arreo/{pane}")
}

/// What a pane does: its own directory, the same filename as its sibling, and a
/// lifetime long enough to be asked about afterwards.
fn pane_script(word: &str, alive_secs: u64) -> String {
    format!("pwd > {WHERE_FILE}; echo {word} > {TASK_FILE}; sleep {alive_secs}")
}

/// `arreo spawn <id> sh -c <script> --worktree`, against the real daemon.
///
/// The bare `--worktree` is the documented "name it after the pane id", which is
/// what makes the branch names the checks expect (`arreo/one`, `arreo/two`) a
/// property of the product rather than of this file.
fn spawn_pane(
    sandbox: &Sandbox,
    cli: &Path,
    layout: &Layout,
    id: &str,
    script: &str,
) -> Result<String, String> {
    let socket = layout.socket_arg();
    let run = run_cli(
        sandbox,
        cli,
        &[
            "spawn",
            id,
            "sh",
            "-c",
            script,
            "--worktree",
            "--socket",
            &socket,
        ],
        CLI_DEADLINE,
    );
    if !run.ok() {
        return Err(format!(
            "`arreo spawn {id} sh -c … --worktree` exited {:?}: {}",
            run.code,
            first_line(&run.output)
        ));
    }
    Ok(first_line(&run.output))
}

/// One `arreo worktrees …` invocation, bounded.
///
/// The repository is named on every call because the CLI's default is its
/// working directory, which is the sandbox and not the repository; the root comes
/// from `$ARREO_CONFIG`, the file the daemon was given.
fn worktrees(sandbox: &Sandbox, cli: &Path, layout: &Layout, socket: &str, sub: &[&str]) -> Run {
    let mut args = vec!["worktrees".to_string()];
    args.extend(sub.iter().map(|word| word.to_string()));
    args.push("--repo".to_string());
    args.push(layout.repo_arg());
    args.push("--socket".to_string());
    args.push(socket.to_string());
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    run_cli(sandbox, cli, &argv, CLI_DEADLINE)
}

/// A spawn attempt as one line: the CLI's own words, or the refusal.
fn said(result: &Result<String, String>) -> String {
    match result {
        Ok(words) => words.clone(),
        Err(e) => format!("failed: {e}"),
    }
}

/// Wait, bounded, for `path` to hold `want`; `(did it, what it held)`.
///
/// A poll, because the file is written by a real shell in a real pane and how
/// fast that happens is the machine's business. The verdict carries what was
/// actually there — `"<absent>"` when the pane never wrote the file at all,
/// which is a different failure from the wrong word in it.
fn wait_for_word(path: &Path, want: &str, within: Duration) -> (bool, String) {
    let deadline = Instant::now() + within;
    let mut seen = String::from("<absent>");
    loop {
        match fs::read_to_string(path) {
            Ok(text) => {
                seen = text.trim().to_string();
                if seen == want {
                    return (true, seen);
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return (false, e.to_string()),
        }
        if Instant::now() >= deadline {
            return (false, seen);
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Wait, bounded, for the daemon to report `id` as alive (or as exited).
///
/// A poll of the daemon's own listing rather than a sleep: the two panes have
/// different lifetimes on purpose, and "the shell has finished" is a fact to
/// observe, not a duration to guess.
fn wait_for_liveness(
    sandbox: &Sandbox,
    cli: &Path,
    layout: &Layout,
    id: &str,
    alive: bool,
    within: Duration,
) -> (bool, String) {
    let socket = layout.socket_arg();
    let deadline = Instant::now() + within;
    loop {
        let listing = run_cli(sandbox, cli, &["panes", "--socket", &socket], CLI_DEADLINE);
        let is_alive = alive_panes(&listing.output).iter().any(|pane| pane == id);
        let seen = if is_alive { "alive" } else { "exited" };
        if is_alive == alive {
            return (true, format!("the daemon lists {id} as {seen}"));
        }
        if Instant::now() >= deadline {
            return (false, format!("the daemon still lists {id} as {seen}"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The listing and the notes: `run_cli` joins stdout before stderr, and
/// `worktrees list --json` writes exactly one line to stdout.
fn split_listing(output: &str) -> (&str, &str) {
    match output.split_once('\n') {
        Some((line, rest)) => (line.trim_end(), rest),
        None => (output.trim_end(), ""),
    }
}
