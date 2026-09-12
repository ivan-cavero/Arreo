//! `arreo update` — swap this binary for another one (T-0070, ROADMAP §3.13).
//!
//! One sentence: put a new `arreo` in place without ever leaving the path missing,
//! non-executable or half-written, and without touching anything that owns a PTY.
//!
//! ## The invariant this verb must not break
//!
//! **The client update path never signals, reaps, restarts or stops a
//! PTY-bearing process, and never stops the daemon.** The daemon owns the agents;
//! this verb owns the client binary. Everything below can reach exactly three
//! things — the binary path, the `.prev` slot beside it, and the resume token —
//! and [`arreo_core::update`] has no `kill`, no `waitpid` and no service-manager
//! call anywhere in it. The slice proves it by recording the daemon's pid and
//! eight pane pids across a real update and asserting none of them moved.
//!
//! ## Where the artifact comes from, and why that is not a shortcut
//!
//! `--from <path>` names a binary the operator already has. That is a real
//! feature — installing a build you made, or one you fetched and inspected — and
//! it is where a channel source will feed in. The **anonymous** form (`arreo
//! update` with no `--from`), which fetches and verifies a signed release, needs
//! the signing key of T-0036 and is deliberately absent: this verb refuses rather
//! than installing something it cannot verify. See `--check`.
//!
//! ## Re-exec
//!
//! After a successful swap the new binary runs, with the same arguments — so the
//! code that continues the work is the code that was just installed. The second
//! invocation is not a loop: it finds the installed binary byte-identical to what
//! it was asked to install and stops swapping, which is also the honest answer for
//! an operator who runs the same command twice. What it does instead is *resume*:
//! if the resume token names a pane, it reattaches to it, which is the thing that
//! makes a client restart cheap rather than a fresh start.

use crate::ExitCode;
use arreo_core::update::{self, resume, UpdateError};

/// Exit codes, small and stable (the vocabulary the rest of the CLI uses).
const OK: u8 = 0;
const FAILED: u8 = 1;
const USAGE: u8 = 2;
const IN_PROGRESS: u8 = 3;
const NOT_WRITABLE: u8 = 4;

/// `arreo update [--from PATH] [--rollback] [--check] [--json] [--reattach-pane ID]
/// [--no-reexec] [--socket PATH]`
pub fn run(rest: &[String]) -> ExitCode {
    let args = match parse(rest) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("update: {message}");
            usage();
            return ExitCode::from(USAGE);
        }
    };

    if args.check {
        // The channel half (T-0037) needs a signed index and the signing key's
        // custodian; this build has neither, and saying so is the honest answer.
        // Installing a binary from a URL this version cannot verify would be the
        // opposite of what the rest of this file is for.
        println!(
            "update: no release channel is configured in this build. Arreo is pre-launch: \
             there is no published artifact to check or fetch."
        );
        println!(
            "        Install a binary you already have with `arreo update --from <path>`; the \
             signed anonymous path is T-0037."
        );
        return ExitCode::from(USAGE);
    }

    let current = match update::current_binary() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("update: cannot find the running binary: {e}");
            return ExitCode::from(FAILED);
        }
    };

    // One updater at a time. The lock is an OS lock held by this open file, so a
    // killed updater cannot leave it stale — see `arreo_core::update`.
    let _lock = match update::UpdateLock::acquire(&current) {
        Ok(lock) => lock,
        Err(UpdateError::Locked(path)) => {
            eprintln!("update: another update is already in progress (holding {path})");
            return ExitCode::from(IN_PROGRESS);
        }
        Err(e) => {
            eprintln!("update: {e}");
            return ExitCode::from(FAILED);
        }
    };

    if args.rollback {
        return rollback(&current, &args);
    }

    let Some(source) = args.from.as_deref() else {
        eprintln!(
            "update: nothing to install. Pass --from <path> with a binary you have, or \
             --rollback, or --check."
        );
        usage();
        return ExitCode::from(USAGE);
    };

    match install(&current, std::path::Path::new(source), &args) {
        Ok((outcome, hand_over)) => {
            // **Report before handing over.** `exec` replaces this process image,
            // so anything printed afterwards is never printed at all — the first
            // version of this printed after the hand-over and `arreo update ... |
            // cat` showed only the new binary's output, silently losing the lines
            // that said what had been installed.
            if args.json {
                println!("{}", outcome.as_json());
            } else {
                for line in &outcome.lines {
                    println!("{line}");
                }
            }
            if let Some(hand_over) = hand_over {
                for line in hand_over.run(&args) {
                    println!("{line}");
                }
            }
            OK.into()
        }
        Err(e) => {
            eprintln!("update: {e}");
            ExitCode::from(exit_code(&e))
        }
    }
}

fn usage() {
    eprintln!(
        "usage: arreo update --from <path> [--json] [--no-reexec] [--reattach-pane ID] [--socket PATH]"
    );
    eprintln!("       arreo update --rollback [--json]");
    eprintln!("       arreo update --check");
    eprintln!("  --from      a binary to install in place of this one (already on disk)");
    eprintln!("  --rollback  put the previous binary back");
    eprintln!("  --check     report the available version from the release channel (needs T-0037)");
    eprintln!(
        "  exit codes: 0 ok · 1 failed · 2 usage · 3 update in progress · 4 path not writable"
    );
}

#[derive(Debug, Default)]
struct Args {
    from: Option<String>,
    rollback: bool,
    check: bool,
    json: bool,
    no_reexec: bool,
    reattach_pane: Option<String>,
    socket: Option<String>,
}

fn parse(rest: &[String]) -> Result<Args, String> {
    let mut args = Args::default();
    let mut i = 0;
    while i < rest.len() {
        let flag = rest[i].as_str();
        let value = || -> Result<String, String> {
            rest.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--from" => {
                args.from = Some(value()?);
                i += 2;
            }
            "--reattach-pane" => {
                args.reattach_pane = Some(value()?);
                i += 2;
            }
            "--socket" => {
                args.socket = Some(value()?);
                i += 2;
            }
            "--rollback" => {
                args.rollback = true;
                i += 1;
            }
            "--check" => {
                args.check = true;
                i += 1;
            }
            "--json" => {
                args.json = true;
                i += 1;
            }
            "--no-reexec" => {
                args.no_reexec = true;
                i += 1;
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if args.rollback && (args.from.is_some() || args.check) {
        return Err(
            "--rollback installs the previous binary; it takes no --from or --check".into(),
        );
    }
    Ok(args)
}

/// What the verb did, for the human and for `--json`.
struct Outcome {
    lines: Vec<String>,
    changed: bool,
    version: String,
    previous: Option<String>,
}

impl Outcome {
    fn as_json(&self) -> String {
        serde_json::json!({
            "changed": self.changed,
            "version": self.version,
            "previous": self.previous,
        })
        .to_string()
    }
}

/// Install, and say whether the caller should hand over to the new binary.
///
/// The hand-over is *returned* rather than performed here so the caller can report
/// first — see the comment at the call site for why that ordering is load-bearing.
fn install(
    current: &std::path::Path,
    source: &std::path::Path,
    args: &Args,
) -> Result<(Outcome, Option<HandOver>), UpdateError> {
    if !source.exists() {
        return Err(UpdateError::Io {
            path: source.display().to_string(),
            detail: "no such file".to_string(),
        });
    }

    // Already what was asked for: stop before touching anything, and resume. This
    // is the branch the re-exec lands in, and it is also the honest answer for an
    // operator who runs the same command twice.
    if update::identical(source, current)? {
        let version = update::verify_runs(current)?;
        // A resume says what happened; only a plain re-run of the same install
        // needs telling that there was nothing to do. Printing both would make
        // the re-exec's contribution look like a second, redundant update.
        let mut lines = if args.reattach_pane.is_some() {
            Vec::new()
        } else {
            vec![format!(
                "already running {version} ({} is byte-identical); nothing to do",
                source.display()
            )]
        };
        lines.extend(reattach(args)?);
        return Ok((
            Outcome {
                lines,
                changed: false,
                version,
                previous: None,
            },
            None,
        ));
    }

    // The resume token is written **before** the swap: a client that has swapped
    // but cannot say where it was has lost the operator's place.
    let target = args.socket.clone().unwrap_or_else(|| {
        arreo_core::mesh::session::default_socket()
            .display()
            .to_string()
    });
    let mut token = resume::Resume::new(target);
    if let Some(pane) = &args.reattach_pane {
        token = token.pane(pane.clone());
    }
    resume::save(&token)?;

    let staged = update::stage(source, current)?;
    update::swap(&staged, current)?;
    let version = update::verify_runs(current)?;
    let previous = update::prev_path(current);

    let lines = vec![
        format!("installed {}", current.display()),
        format!("version: {version}"),
        format!("previous kept at {}", previous.display()),
    ];

    let outcome = Outcome {
        lines,
        changed: true,
        version,
        previous: Some(previous.display().to_string()),
    };
    if args.no_reexec || args.json {
        // `--no-reexec` asks for the install alone; `--json` asks for one
        // machine-readable report, and a second process printing a second object
        // would be two answers to one question.
        return Ok((outcome, None));
    }
    // The new binary runs the rest, so the code that continues is the code that
    // was just installed.
    Ok((
        outcome,
        Some(HandOver {
            current: current.to_path_buf(),
        }),
    ))
}

/// The hand-over to the binary that was just installed.
///
/// It carries the path rather than asking for it again, and that is not tidiness:
/// **after the swap this process's own path is stale.** `/proc/self/exe` names the
/// file the image was loaded from — the *old* inode, which the swap has moved to
/// `.prev` — so the kernel reports it as `… (deleted)` and re-deriving the path
/// fails with ENOENT. The swap already knows where the new binary is; the
/// hand-over takes it from there. (Found by running the verb: the hand-over
/// reported "cannot find the new binary: /tmp/…/arreo (deleted)".)
struct HandOver {
    current: std::path::PathBuf,
}

impl HandOver {
    fn run(&self, args: &Args) -> Vec<String> {
        reexec(&self.current, args)
    }
}

fn rollback(current: &std::path::Path, args: &Args) -> ExitCode {
    match update::rollback(current) {
        Ok(()) => {
            let version = match update::verify_runs(current) {
                Ok(version) => version,
                Err(e) => {
                    // The restored binary must run; if it does not, say so loudly
                    // rather than claiming a successful rollback.
                    eprintln!("update: rolled back, but {e}");
                    return ExitCode::from(FAILED);
                }
            };
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({"changed": true, "version": version, "rolled_back": true})
                );
            } else {
                println!("rolled back to {version} ({})", current.display());
            }
            OK.into()
        }
        Err(e) => {
            eprintln!("update: {e}");
            ExitCode::from(exit_code(&e))
        }
    }
}

/// Re-execute the (now new) binary with the same arguments, and return anything
/// it printed — or a note explaining why the process did not hand over.
fn reexec(current: &std::path::Path, args: &Args) -> Vec<String> {
    let mut command = std::process::Command::new(current);
    // The **verb**, then the same flags. A re-exec that dropped `update` would
    // run `arreo --from <path>`, which is not a verb at all and answers with the
    // usage text — the failure the CLI test caught on its first run.
    command.arg("update");
    command
        .arg("--from")
        .arg(args.from.as_deref().unwrap_or_default());
    if let Some(pane) = &args.reattach_pane {
        command.arg("--reattach-pane").arg(pane);
    }
    if let Some(socket) = &args.socket {
        command.arg("--socket").arg(socket);
    }
    command.arg("--no-reexec");

    #[cfg(unix)]
    {
        // **Flush before handing over.** `exec` replaces the image, and anything
        // still in stdout's buffer goes with it — so `arreo update ... > log`
        // would have lost the lines saying what was installed. Rust's stdout is
        // block-buffered when it is not a terminal, which is exactly the case a
        // redirect creates.
        let _ = std::io::Write::flush(&mut std::io::stdout());
        // `exec` replaces this process image: the shell that started the update
        // sees the new binary's exit status, and there is no window where two
        // versions are running.
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        // Only reached when exec failed, which leaves this process alive.
        vec![format!(
            "installed, but could not hand over to the new binary: {error} \
             (run it again to use it)"
        )]
    }
    #[cfg(not(unix))]
    {
        match command.output() {
            Ok(output) => {
                let mut text = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if text.is_empty() {
                    text = String::from_utf8_lossy(&output.stderr).trim().to_string();
                }
                text.lines().map(str::to_string).collect()
            }
            Err(e) => vec![format!(
                "installed, but could not start the new binary: {e} (run it again to use it)"
            )],
        }
    }
}

/// Reattach to the pane the resume token names, if any.
///
/// This is what makes a re-exec a *resume*: the client comes back to the pane it
/// was working with, by reading from where it had got to. It talks to the daemon
/// over the socket — the daemon that was never touched — which is the point.
fn reattach(args: &Args) -> Result<Vec<String>, UpdateError> {
    let Some(token) = resume::load() else {
        return Ok(Vec::new());
    };
    let Some(pane) = token.pane.clone() else {
        return Ok(Vec::new());
    };
    let socket = args.socket.clone().unwrap_or_else(|| token.target.clone());
    match read_pane(std::path::Path::new(&socket), &pane, token.from_line) {
        Ok(lines) => {
            let mut out = vec![format!(
                "resumed pane {pane} from {socket} ({} line(s) after {})",
                lines.len(),
                token.from_line
            )];
            out.extend(lines);
            Ok(out)
        }
        Err(e) => Ok(vec![format!(
            "resumed: could not read pane {pane} from {socket}: {e}"
        )]),
    }
}

/// One `read` against the daemon, using the same client every other verb uses.
///
/// Its own current-thread runtime rather than the CLI's `rt::block_on`, which is
/// specialised to a future returning an exit code: this one returns data, and a
/// second specialisation of that helper would be the third place in the file that
/// builds a runtime.
fn read_pane(
    socket: &std::path::Path,
    pane: &str,
    from_line: usize,
) -> Result<Vec<String>, String> {
    use arreo_core::proto::{Message, VERSION};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    runtime.block_on(async {
        let mut client = arreo_core::mesh::session::Client::connect(socket)
            .await
            .map_err(|e| e.to_string())?;
        let reply = client
            .call(&Message::Read {
                v: VERSION,
                id: pane.to_string(),
                from_line,
            })
            .await
            .map_err(|e| e.to_string())?;
        match reply {
            Message::Delta { lines, .. } | Message::Snapshot { lines, .. } => Ok(lines),
            Message::Error { message, .. } => Err(message),
            other => Err(format!("unexpected {other:?}")),
        }
    })
}

fn exit_code(error: &UpdateError) -> u8 {
    match error {
        UpdateError::NotWritable { .. } => NOT_WRITABLE,
        UpdateError::Locked(_) => IN_PROGRESS,
        UpdateError::Io { .. } | UpdateError::NotExecutable(_) | UpdateError::NoPrevious(_) => {
            FAILED
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn the_arguments_are_parsed_and_refused_before_anything_is_touched() {
        let parsed = parse(&args(&[
            "--from",
            "/tmp/new-arreo",
            "--reattach-pane",
            "pane-1",
            "--json",
        ]))
        .expect("parses");
        assert_eq!(parsed.from.as_deref(), Some("/tmp/new-arreo"));
        assert_eq!(parsed.reattach_pane.as_deref(), Some("pane-1"));
        assert!(parsed.json);

        // A flag that needs a value and does not get one is a usage error, not a
        // silent default.
        assert!(parse(&args(&["--from"])).is_err());
        assert!(parse(&args(&["--nonsense"])).is_err());
        // `--rollback` and `--from` are different intentions; obeying both would
        // mean installing and uninstalling in one command.
        assert!(parse(&args(&["--rollback", "--from", "/tmp/x"])).is_err());
    }
}
