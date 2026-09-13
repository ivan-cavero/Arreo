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
//! `--from <path>` names a binary the operator already has — a real feature
//! (installing a build you made, or one you fetched and inspected). The
//! **anonymous** form (`arreo update` with no `--from`) asks the release channel
//! for the newest release instead: the index is fetched and verified against the
//! key compiled into this binary, then the artifact is fetched and verified the
//! same way, and only then does the file it produced enter the install path
//! `--from` uses. There is one install path, not two, and one verifier, not two —
//! see `arreo_core::update::channel`, which owns the fetch and never installs.
//!
//! `--check` is the same fetch with the install left out: it reports the version
//! and artifact the channel publishes (or "no releases yet"), and refuses an
//! index it cannot verify with the verifier's own sentence.
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
use arreo_core::update::channel::{self, Channel};
use arreo_core::update::verify::TrustSet;
use arreo_core::update::{self, resume, UpdateError};

/// Exit codes, small and stable (the vocabulary the rest of the CLI uses).
const OK: u8 = 0;
const FAILED: u8 = 1;
const USAGE: u8 = 2;
const IN_PROGRESS: u8 = 3;
const NOT_WRITABLE: u8 = 4;

/// `arreo update [--from PATH] [--rollback] [--check] [--json] [--reattach-pane ID]
/// [--no-reexec] [--socket PATH]`
///
/// `arreo update verify <path> [--sig PATH] [--manifest PATH]` is the other half
/// of the verb — it checks a release artifact instead of installing one — and is
/// read off before the install flags are parsed, because it takes a positional
/// argument and an install never does.
pub fn run(rest: &[String]) -> ExitCode {
    if rest.first().map(String::as_str) == Some("verify") {
        return verify(&rest[1..]);
    }
    let args = match parse(rest) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("update: {message}");
            usage();
            return ExitCode::from(USAGE);
        }
    };

    // One updater at a time — and `--check` takes the same lock before it
    // touches the channel work dir: a check shares <state>/channel/ with a
    // concurrent update, and while the verifier accepts only bytes it read
    // itself (so the race is fail-safe either way), the lock makes the race
    // impossible instead of merely harmless. The lock is an OS lock held by
    // this open file, so a killed updater cannot leave it stale — see
    // `arreo_core::update`.
    let current = match update::current_binary() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("update: cannot find the running binary: {e}");
            return ExitCode::from(FAILED);
        }
    };
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

    if args.check {
        return check(&args);
    }

    if args.server {
        return server(&args);
    }

    if args.rollback {
        return rollback(&current, &args);
    }

    // **Where the artifact comes from.** `--from` names one the operator already
    // has; without it, the channel is asked for the newest release, which is
    // fetched and verified before this process will look at it. Both end at the
    // same `install`, so an anonymous update is a `--from` update whose source
    // came off the wire — there is no second install path to keep in step.
    let source = match args.from.clone() {
        Some(path) => std::path::PathBuf::from(path),
        None => match fetch_release(&args) {
            Ok(Some(path)) => path,
            Ok(None) => return OK.into(),
            Err(code) => return ExitCode::from(code),
        },
    };

    match install(&current, &source, &args) {
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

/// The channel the operator configured: `--channel`, else `ARREO_CHANNEL_URL`,
/// else the built-in default.
///
/// The precedence lives in one place because two answers to "which channel" would
/// let `--check` and `arreo update` look at different ones.
fn configured_channel(args: &Args) -> Result<Channel, String> {
    let (url, origin) = match &args.channel {
        Some(url) => (url.clone(), "--channel"),
        None => match std::env::var("ARREO_CHANNEL_URL") {
            Ok(url) => (url, "ARREO_CHANNEL_URL"),
            Err(_) => (channel::DEFAULT_URL.to_string(), "the default channel"),
        },
    };
    Channel::new(url).map_err(|e| format!("{origin}: {e}"))
}

/// `arreo update --check` — what the channel publishes, and nothing else.
///
/// ## Exit codes, and why an empty channel is 0
///
/// **0** the check ran: either "no releases yet", or the version and artifact the
/// channel reports. **1** a refusal — the index has no signature, was signed by a
/// key this build does not trust, or changed after signing, and the verifier's own
/// sentence is printed. **2** usage: a channel URL this build cannot fetch.
///
/// An empty channel is *not* an error, because it is not the failure of anything:
/// before a first release there is genuinely nothing to report, and a script that
/// treated "no releases yet" as a failure would be wrong every day until launch.
fn check(args: &Args) -> ExitCode {
    let channel = match configured_channel(args) {
        Ok(channel) => channel,
        Err(message) => {
            eprintln!("update: {message}");
            usage();
            return ExitCode::from(USAGE);
        }
    };
    let release = match channel::check(TrustSet::pinned(), &channel, &channel::work_dir()) {
        Ok(release) => release,
        Err(e) => return ExitCode::from(refuse(&channel, &e)),
    };

    match release {
        Some(release) => {
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "channel": channel.url(),
                        "available": true,
                        "version": release.version,
                        "target": release.target,
                        "artifact": release.artifact,
                        "key_id": release.key_id,
                        "digest": release.digest,
                    })
                );
            } else {
                println!("channel:  {}", channel.url());
                println!("version:  {}", release.version);
                println!("artifact: {} (for {})", release.artifact, release.target);
                println!(
                    "  key    {} (pinned in supply-chain/arreo.pub, compiled into this binary)",
                    release.key_id
                );
                println!("  sha256 {} (the index)", release.digest);
                println!("install it with `arreo update`");
            }
        }
        None => {
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "channel": channel.url(),
                        "available": false,
                        "version": serde_json::Value::Null,
                        "artifact": serde_json::Value::Null,
                    })
                );
            } else {
                println!("no releases yet at {}", channel.url());
                println!(
                    "nothing is published on this channel yet; when a release is, `arreo update` \
                     will fetch it and verify it before installing anything"
                );
            }
        }
    }
    OK.into()
}

/// Print a channel refusal the way the verifier wrote it, and name the channel
/// that served it.
///
/// The sentence is the verifier's own — `MissingSignature`, `UnknownKeyId`,
/// `BadSignature`, `DigestMismatch` — and never a paraphrase, so an operator who
/// has seen one refusal has seen them all. The second line adds the one thing the
/// sentence cannot know: the URL the file came from, because the sentence names
/// the local copy the check wrote.
fn refuse(channel: &Channel, error: &channel::Error) -> u8 {
    eprintln!("update: {error}");
    if matches!(error, channel::Error::Verify(_)) {
        eprintln!(
            "update: the channel at {} served the file this refusal names; nothing was installed",
            channel.url()
        );
    }
    FAILED
}

/// The anonymous half: ask the channel for the newest release, fetch it, verify
/// it, and return the local path the install path should be handed.
///
/// `Ok(None)` is an empty channel — nothing to install, and nothing wrong. Every
/// refusal returns the exit code it should be reported as, having already printed
/// the verifier's sentence.
fn fetch_release(args: &Args) -> Result<Option<std::path::PathBuf>, u8> {
    let channel = configured_channel(args).map_err(|message| {
        eprintln!("update: {message}");
        usage();
        USAGE
    })?;
    let work = channel::work_dir();
    let release = match channel::check(TrustSet::pinned(), &channel, &work) {
        Ok(Some(release)) => release,
        Ok(None) => {
            if args.json {
                println!(
                    "{}",
                    serde_json::json!({
                        "changed": false,
                        "channel": channel.url(),
                        "available": false,
                    })
                );
            } else {
                println!("no releases yet at {channel}; nothing to install");
            }
            return Ok(None);
        }
        Err(e) => return Err(refuse(&channel, &e)),
    };
    // One line before a download that can take a while — and never in `--json`
    // mode, where a second object would be a second answer to one question.
    if !args.json {
        println!(
            "fetching arreo {} ({}) from {channel}",
            release.version, release.artifact
        );
    }
    match channel::fetch(TrustSet::pinned(), &channel, &release, &work) {
        Ok(verified) => Ok(Some(verified.artifact)),
        Err(e) => Err(refuse(&channel, &e)),
    }
}

/// What `arreo update verify` was asked to check.
#[derive(Debug)]
struct VerifyArgs {
    artifact: String,
    sig: Option<String>,
    manifest: Option<String>,
    json: bool,
}

/// `arreo update verify <path> [--sig <path>] [--manifest <path>] [--json]` — the
/// user door onto [`arreo_core::update::verify`] (T-0036).
///
/// ## What it prints, and why those three things
///
/// The artifact, the key id that signed it, and the SHA-256 digest. The key id is
/// compared against the key **compiled into this binary**, never against anything
/// in the environment, the working directory or the release page — which is what
/// makes the three lines auditable: an operator can write them down and check
/// them later, and a release announcement can be compared against them. `ok` on
/// its own is a claim nobody can audit.
///
/// ## Why there is no bypass
///
/// A `--force` here would make every other line of the signed-release story
/// decorative: the whole claim is that nothing downstream can be talked into
/// trusting an artifact. So a refusal is exit 1 — missing signature, unknown key
/// id, bad signature, digest mismatch or an unreadable file, each naming what
/// failed — and a usage error is exit 2. The only recourse a script has is to
/// fetch the artifact again, which is the correct answer to every one of those.
///
/// ## `--manifest`
///
/// With `--manifest SHA256SUMS`, the manifest's **own** signature is verified
/// first and the artifact's digest is then checked against its entry: the order
/// the install scripts use, and the only way a digest means anything. The
/// manifest is the trusted digest source — never a web page, never a file name.
fn verify(rest: &[String]) -> ExitCode {
    let args = match parse_verify(rest) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("update: {message}");
            verify_usage();
            return ExitCode::from(USAGE);
        }
    };
    let artifact = std::path::Path::new(&args.artifact);

    // The manifest first: an unverified manifest is a list of digests from
    // nowhere, so nothing in it is consulted until its own signature holds.
    if let Some(manifest) = args.manifest.as_deref() {
        let manifest = std::path::Path::new(manifest);
        if let Err(e) = arreo_core::update::verify::verify(manifest, None) {
            eprintln!("update: {e}");
            return ExitCode::from(FAILED);
        }
    }

    let signature = args.sig.as_deref().map(std::path::Path::new);
    let verified = match arreo_core::update::verify::verify(artifact, signature) {
        Ok(verified) => verified,
        Err(e) => {
            eprintln!("update: {e}");
            return ExitCode::from(FAILED);
        }
    };

    if let Some(manifest) = args.manifest.as_deref() {
        if let Err(e) = arreo_core::update::verify::check_manifest_digest(
            artifact,
            std::path::Path::new(manifest),
        ) {
            eprintln!("update: {e}");
            return ExitCode::from(FAILED);
        }
    }

    let name = verified
        .artifact
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| args.artifact.clone());
    if args.json {
        println!(
            "{}",
            serde_json::json!({
                "verified": true,
                "artifact": name,
                "path": verified.artifact.display().to_string(),
                "key_id": verified.key_id,
                "digest": verified.digest,
            })
        );
    } else {
        println!("verified {name}");
        println!(
            "  key    {} (pinned in supply-chain/arreo.pub, compiled into this binary)",
            verified.key_id
        );
        println!("  sha256 {}", verified.digest);
    }
    OK.into()
}

fn parse_verify(rest: &[String]) -> Result<VerifyArgs, String> {
    let mut artifact: Option<String> = None;
    let mut sig = None;
    let mut manifest = None;
    let mut json = false;
    let mut i = 0;
    while i < rest.len() {
        let flag = rest[i].as_str();
        let value = || -> Result<String, String> {
            rest.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag {
            "--sig" => {
                sig = Some(value()?);
                i += 2;
            }
            "--manifest" => {
                manifest = Some(value()?);
                i += 2;
            }
            "--json" => {
                json = true;
                i += 1;
            }
            other if other.starts_with('-') => return Err(format!("unknown flag {other}")),
            other => {
                if artifact.replace(other.to_string()).is_some() {
                    return Err(format!(
                        "verify checks one artifact; {other} is a second one"
                    ));
                }
                i += 1;
            }
        }
    }
    Ok(VerifyArgs {
        artifact: artifact
            .ok_or_else(|| "verify needs the path of the artifact to check".to_string())?,
        sig,
        manifest,
        json,
    })
}

fn verify_usage() {
    eprintln!("usage: arreo update verify <path> [--sig <path>] [--manifest <path>] [--json]");
    eprintln!("  <path>      the artifact to check; its signature is <path>.minisig");
    eprintln!("  --sig       the signature to use instead of the sibling <path>.minisig");
    eprintln!("  --manifest  a signed SHA256SUMS; verified first, then used to check the digest");
    eprintln!("  exit codes: 0 verified · 1 refused · 2 usage — there is no bypass flag");
}

/// `arreo update --server --from <path>` — the daemon half of the update (T-0038).
///
/// ## The order, and why it is this order
///
/// ```text
/// 1. stage the candidate beside the installed server binary
/// 2. prove the staged binary runs            (--version)
/// 3. spawn the *staged* binary in handoff mode
/// 4. wait until a different process serves the socket
/// 5. only now install over the server path   (hard link + one atomic rename)
/// ```
///
/// The tempting order is to install first and then ask the daemon to take over,
/// and it is worse in exactly the way this codebase keeps choosing against: if
/// the handoff fails, the install has already happened, so the machine is left
/// with a new binary on disk, the old one running from memory, and `.prev`
/// holding something the operator did not ask for. **Handing off from the staged
/// path makes a failed handoff change nothing at all** — no swap, no `.prev`
/// churn, and the daemon still serving the binary it was already running. The
/// swap is the cheap, atomic, always-possible step, so it goes last.
///
/// ## Why the child gets no stdio
///
/// The new daemon outlives this command. If it inherited our stdout/stderr and
/// the operator piped `arreo update --server | tee`, then our exit would close
/// that pipe and the daemon's next log line would be written to a broken
/// descriptor — a daemon that can be killed by its launcher exiting is precisely
/// what a handoff exists to prevent. So the child is given null stdio, and this
/// command reports what happened itself. The cost is real and worth naming: a
/// daemon started this way logs nowhere, which is why a supervised install
/// (`arreo service install`) remains the way to run one for real.
///
/// ## Why argv[0] is the installed path, not the staged one
///
/// The running process's image is `<binary>.staged` until the swap. `arreo server
/// stop` and this command both find a daemon by scanning `/proc` for a process
/// whose argv runs `arreo-server` and names the socket — so the child is spawned
/// with argv[0] set to the installed path it is about to occupy, and every such
/// scan keeps working across the cut.
fn server(args: &Args) -> ExitCode {
    let current = match server_binary() {
        Ok(path) => path,
        Err(message) => {
            eprintln!("update: {message}");
            return ExitCode::from(FAILED);
        }
    };
    if !current.is_file() {
        // `is_file` is false for a missing binary and for one this user cannot
        // read, and the two need different advice.
        eprintln!(
            "update: no `arreo-server` beside this binary (looked for {}). \
             --server replaces the daemon; install one first.",
            current.display()
        );
        return ExitCode::from(FAILED);
    }
    // One updater at a time, keyed on the server binary (the client lock is a
    // different file, so a client update and a server update can proceed
    // together — they touch different paths).
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
        return match update::rollback(&current) {
            Ok(()) => {
                println!("rolled back {}", current.display());
                println!(
                    "the running daemon is unchanged; it takes effect when the daemon \
                     next starts"
                );
                OK.into()
            }
            Err(e) => {
                eprintln!("update: {e}");
                ExitCode::from(exit_code(&e))
            }
        };
    }

    let Some(source) = args.from.as_deref() else {
        eprintln!("update: nothing to install. Pass --from <path> with a server binary you have.");
        usage();
        return ExitCode::from(USAGE);
    };
    // Re-running the same install is a no-op, and saying so is better than
    // manufacturing a `.prev` for an install that changes nothing (the same rule
    // the client path follows).
    if update::identical(std::path::Path::new(source), &current).unwrap_or(false) {
        let version = update::verify_runs(&current).unwrap_or_else(|_| "unknown".to_string());
        println!(
            "already running {version} ({} is byte-identical); nothing to do",
            source
        );
        return OK.into();
    }
    let socket = args
        .socket
        .clone()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(crate::default_socket);

    // 1-2. Stage beside the installed binary and prove it runs. `stage` refuses a
    // candidate that is not executable; `verify_runs` refuses one that is
    // executable but not a working `arreo-server` — which for a *server* matters
    // more than for a client, because the daemon about to adopt every PTY on this
    // machine is the thing being trusted.
    let staged = match update::stage(std::path::Path::new(source), &current) {
        Ok(staged) => staged,
        Err(e) => {
            eprintln!("update: {e}");
            return ExitCode::from(exit_code(&e));
        }
    };
    let version = match update::verify_runs(&staged) {
        Ok(version) => version,
        Err(e) => {
            let _ = std::fs::remove_file(&staged);
            eprintln!("update: the candidate does not run: {e}");
            return ExitCode::from(FAILED);
        }
    };
    // "It runs" is not enough for this path: `arreo --version` also succeeds, and
    // installing a *client* over the *server* path would leave the machine with a
    // daemon that cannot be started. The version string is the one piece of
    // self-description a binary offers, so it is what gets checked — the check is
    // cheap and the failure it prevents is a bricked daemon.
    if !version.contains(SERVER_BINARY_NAME) {
        let _ = std::fs::remove_file(&staged);
        eprintln!(
            "update: {source} is not an arreo server (it reports {version:?}); \
             --server replaces the daemon, not the client"
        );
        return ExitCode::from(FAILED);
    }

    // 3-4. Hand the daemon over, if one is running.
    //
    // On Windows there is no descriptor passing, so no handoff exists: the binary
    // is installed and takes effect at the next daemon start — §3.13's deferred
    // update, which is what that platform is specified to get (T-0039). The
    // distinction is reported, not glossed: an operator must never think a cut
    // happened when the daemon is still running the old code.
    #[cfg(not(unix))]
    let handoff = {
        if crate::find_daemon_pid(&socket).is_some() {
            Handoff::Deferred
        } else {
            Handoff::NoDaemon
        }
    };
    #[cfg(unix)]
    let handoff = match crate::find_daemon_pid(&socket) {
        None => Handoff::NoDaemon,
        Some(pid) => match wait_for_takeover(&staged, &current, &socket, pid, args) {
            Ok(new_pid) => Handoff::TookOver {
                from: pid,
                to: new_pid,
            },
            Err(HandoffError { code, message }) => {
                // The handoff failed, so nothing is installed and nothing
                // changed: the staged copy is dropped and the daemon that was
                // serving keeps serving the binary it was already running.
                let _ = std::fs::remove_file(&staged);
                eprintln!("update: {message}");
                eprintln!(
                    "update: nothing was installed; the daemon serving {} is still running \
                     (pid {pid})",
                    socket.display()
                );
                return ExitCode::from(code);
            }
        },
    };

    // 5. Install. At this point a daemon may already be running the staged
    // binary, so a failure here must say that plainly rather than implying the
    // update did not happen.
    let previous = match update::swap(&staged, &current) {
        Ok(()) => update::prev_path(&current).display().to_string(),
        Err(e) => {
            eprintln!(
                "update: the handoff succeeded but installing over {} failed: {e}",
                current.display()
            );
            eprintln!(
                "update: the new daemon is running from {}; `arreo update --server --rollback` \
                 will not see it until the next start",
                staged.display()
            );
            return ExitCode::from(exit_code(&e));
        }
    };

    if args.json {
        let handoff = match &handoff {
            Handoff::NoDaemon => serde_json::json!({"daemon": null}),
            Handoff::TookOver { from, to } => {
                serde_json::json!({"daemon": {"from_pid": from, "to_pid": to}})
            }
            #[cfg(not(unix))]
            Handoff::Deferred => serde_json::json!({"daemon": {"deferred": true}}),
        };
        println!(
            "{}",
            serde_json::json!({
                "changed": true,
                "installed": current.display().to_string(),
                "version": version,
                "previous": previous,
                "handoff": handoff,
            })
        );
    } else {
        println!("installed {}", current.display());
        println!("version: {version}");
        println!("previous kept at {previous}");
        match handoff {
            Handoff::NoDaemon => println!(
                "no daemon was serving {}; it will run the new binary when it next starts",
                socket.display()
            ),
            Handoff::TookOver { from, to } => {
                println!(
                    "handed over: the daemon serving {} is now pid {to} (was {from})",
                    socket.display()
                );
                println!("no agent was restarted: the PTYs and their processes were untouched");
            }
            #[cfg(not(unix))]
            Handoff::Deferred => println!(
                "update pending: a daemon is serving {} and this platform cannot hand it over \
                 without a restart; the new binary takes effect when it next starts",
                socket.display()
            ),
        }
    }
    OK.into()
}

/// A handoff that did not happen, with the exit code it should be reported as.
///
/// Two codes, not one, because "someone else is updating this machine right now"
/// and "this update does not work" call for different actions from an operator
/// and from a script — and collapsing them was a real (if minor) defect the
/// security re-review found.
struct HandoffError {
    code: u8,
    message: String,
}

/// What the daemon half of the update did.
enum Handoff {
    /// Nothing was serving the socket, so there was nothing to hand over.
    NoDaemon,
    /// The daemon was handed over to the new binary without a restart.
    TookOver { from: u32, to: u32 },
    /// Windows: installed, and it takes effect at the next daemon start.
    #[cfg(not(unix))]
    Deferred,
}

/// Spawn the staged server binary in handoff mode and wait until it is the
/// process serving `socket`.
///
/// ## What counts as "it took over"
///
/// **The outgoing daemon is no longer running, and the socket still answers.**
/// Both halves are load-bearing, and the obvious weaker condition — "some
/// daemon is answering on the socket" — is wrong: the *incoming* daemon appears
/// in `/proc` the moment it starts (before it has taken anything over) and the
/// *outgoing* one keeps answering until it exits, so a poll on "someone answers"
/// can fire before the cut has happened and report a success that has not
/// occurred.
///
/// The ordering in ADR 0021 is what makes the two-part condition exact: the
/// outgoing daemon exits only *after* the incoming one has committed, so
/// "old is gone" cannot be true before the cut is real. It holds for the crash
/// case too — if the outgoing daemon dies mid-handoff the incoming one aborts
/// and serves nothing, so the second half fails and this reports a failure
/// rather than a success.
///
/// **A zombie counts as not running.** The outgoing daemon may not have been
/// reaped by whoever started it (a shell, or a test that has not waited), so
/// `/proc/<pid>` can linger after it exits. Reading the `stat` state handles
/// that and the reaped case alike.
fn wait_for_takeover(
    staged: &std::path::Path,
    current: &std::path::Path,
    socket: &std::path::Path,
    previous_pid: u32,
    args: &Args,
) -> Result<u32, HandoffError> {
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    let timeout = Duration::from_secs(args.timeout_secs.unwrap_or(DEFAULT_HANDOFF_TIMEOUT_SECS));
    let mut command = Command::new(staged);
    // argv[0] is the path this process is about to occupy, so `/proc` scans for
    // `arreo-server` keep finding the daemon across the cut (see the fn docs).
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.arg0(current);
    }
    // See the fn docs: a TTY is safe to inherit and worth inheriting; anything
    // else is a stream that can close under the daemon and kill it via a panicking
    // `eprintln!`.
    let stderr = if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
        Stdio::inherit()
    } else {
        Stdio::null()
    };
    let mut child = command
        .arg("--handoff-from")
        .arg(socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .map_err(|e| HandoffError {
            code: FAILED,
            message: format!("cannot start the new daemon ({}): {e}", staged.display()),
        })?;

    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Exit 3 is the daemon's "another handoff holds the lock", which
                // is not a failed update but a *deferred* one: the machine is
                // being handed over by someone else right now, and retrying is
                // the whole remedy. Reported as "in progress" so a script sees
                // the same code it would from the updater's own lock, rather
                // than a generic failure that reads like a broken machine.
                let code = if status.code() == Some(3) {
                    IN_PROGRESS
                } else {
                    FAILED
                };
                let why = if code == IN_PROGRESS {
                    "another handoff is already in progress on this machine".to_string()
                } else {
                    format!(
                        "it was refused, or the handoff failed (run `arreo-server \
                         --handoff-from {}` by hand for the reason)",
                        socket.display()
                    )
                };
                return Err(HandoffError {
                    code,
                    message: format!("the new daemon exited ({status}) before taking over — {why}"),
                });
            }
            Ok(None) => {}
            Err(e) => {
                return Err(HandoffError {
                    code: FAILED,
                    message: format!("cannot check on the new daemon: {e}"),
                })
            }
        }
        if let Some(pid) = takeover(
            crate::find_daemon_pid(socket),
            previous_pid,
            daemon_running(previous_pid),
        ) {
            return Ok(pid);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    // **This kills the direct child, not its descendants.** `Child::kill` signals
    // one pid, and a candidate that spawned children before hanging would leave
    // them behind (observed while testing a deliberately hanging fake server: the
    // `sh` wrapper died and its `sleep` did not). Reaping the whole tree needs a
    // process group plus a group-kill syscall, which this crate has no dependency
    // for; the practical exposure today is nil because an `arreo-server` waiting
    // on a handoff has adopted nothing and spawned nothing. It stops being nil
    // when the handoff carries panes, so it is filed rather than noted and
    // forgotten — see T-0077.
    let _ = child.kill();
    let _ = child.wait();
    Err(HandoffError {
        code: FAILED,
        message: format!(
            "the handoff did not complete within {}s (the outgoing daemon, pid {previous_pid}, was \
             still running at the last check)",
            timeout.as_secs()
        ),
    })
}

/// **The readiness rule**, as a pure function of the two facts the poll can
/// observe: which pid is answering the socket, and whether the outgoing daemon
/// is still running.
///
/// Pure on purpose. The property being expressed — "do not report a takeover
/// before it has happened" — is a race, and a race cannot be tested by racing: a
/// test that polls a real handoff passes with the rule weakened, because by the
/// time it can look, the cut has completed anyway. (That is not a hypothesis: it
/// was tried, and the weakened rule passed the integration test.) So the rule
/// lives here, where every combination including the dangerous one is a row in a
/// table, and `wait_for_takeover` only supplies the facts.
///
/// The dangerous combination is the first row: a *different* daemon is answering
/// while the old one still runs. That is exactly the state during a handoff — the
/// incoming process appears in `/proc` long before it has taken anything over —
/// and treating it as success would print a cut that had not happened.
fn takeover(found: Option<u32>, previous: u32, previous_running: bool) -> Option<u32> {
    let found = found?;
    if found == previous || previous_running {
        return None;
    }
    Some(found)
}

/// Is `pid` still running?
///
/// Linux answers from `/proc`, and **a zombie counts as not running** — which is
/// the case that matters, because the outgoing daemon is rarely ours to reap and
/// `/proc/<pid>` otherwise outlives it.
///
/// Elsewhere there is no `/proc`: this reports `false`, so readiness falls back
/// to "a daemon is answering" and can in principle fire a moment early. That is
/// recorded rather than hidden — the alternative is parsing `sysctl`/`kqueue` in
/// a CLI, and the property being weakened (proving the *old* process is gone) is
/// not one the operator can act on anyway. Linux is the handoff's target;
/// Windows takes the deferred path (see `deferred_server_install`).
#[cfg(target_os = "linux")]
fn daemon_running(pid: u32) -> bool {
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false; // gone, or a pid we cannot read: not running for our purposes
    };
    // `pid (comm) state ...` — `comm` may contain spaces and parentheses, so the
    // state is whatever follows the *last* `)`.
    match stat.rsplit_once(") ") {
        Some((_, rest)) => !rest.starts_with('Z'),
        None => false,
    }
}

#[cfg(not(target_os = "linux"))]
fn daemon_running(_pid: u32) -> bool {
    false
}

/// How long `--server` waits for a handoff before giving up. Generous because
/// the alternative to waiting is a failed update, and the handoff itself is
/// bounded by its own timeout on the daemon side.
const DEFAULT_HANDOFF_TIMEOUT_SECS: u64 = 30;

/// How often the takeover poll re-checks. Short enough to feel immediate, long
/// enough that a `/proc` scan per tick is not the cost of the operation.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);

/// The `arreo-server` beside the running `arreo`.
///
/// A dist install and a `target/debug` build both put the two binaries in one
/// directory, and the daemon that serves this machine is the one shipped with
/// the client — so "beside me" is the only rule that cannot pick up an unrelated
/// binary from `$PATH`.
fn server_binary() -> Result<std::path::PathBuf, String> {
    let client =
        update::current_binary().map_err(|e| format!("cannot find the running binary: {e}"))?;
    let dir = client
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", client.display()))?;
    Ok(dir.join(SERVER_BINARY_NAME))
}

/// The daemon binary's name. One constant: `arreo server stop` finds the process
/// by the same string, and two spellings would mean one of them stops working.
#[cfg(unix)]
const SERVER_BINARY_NAME: &str = "arreo-server";
#[cfg(windows)]
const SERVER_BINARY_NAME: &str = "arreo-server.exe";

fn usage() {
    eprintln!(
        "usage: arreo update [--channel URL | ARREO_CHANNEL_URL=URL] [--json] [--no-reexec] [--reattach-pane ID] [--socket PATH]"
    );
    eprintln!(
        "       arreo update --from <path> [--json] [--no-reexec] [--reattach-pane ID] [--socket PATH]"
    );
    eprintln!("       arreo update --rollback [--json]");
    eprintln!("       arreo update --check [--channel URL] [--json]");
    eprintln!("       arreo update verify <path> [--sig <path>] [--manifest <path>] [--json]");
    eprintln!(
        "       arreo update --server --from <path> [--json] [--socket PATH] [--timeout-secs N]"
    );
    eprintln!("  --from      a binary to install in place of this one (already on disk)");
    eprintln!(
        "  --channel   the release channel to fetch from (a URL): file:// for a mirror or a test,"
    );
    eprintln!("              https:// for the default (ARREO_CHANNEL_URL, else this repository's");
    eprintln!("              GitHub Releases `latest` URL). Only read by --check and the anonymous update");
    eprintln!("  --rollback  put the previous binary back");
    eprintln!(
        "  --check     report the version and artifact the channel publishes, verifying the index"
    );
    eprintln!(
        "              first; an empty channel is \"no releases yet\", and the verifier's refusal"
    );
    eprintln!("              (exit 1) is what an unsigned or altered index gets");
    eprintln!(
        "  verify      check an artifact's minisign signature against the key compiled into this \
         binary; prints the key id and digest, and refuses (exit 1) with no bypass"
    );
    eprintln!(
        "  --server    with --from: replace the `arreo-server` beside this binary and hand the \
         running daemon over to it, without killing an agent"
    );
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
    /// Where the anonymous update reads from. `None` means `ARREO_CHANNEL_URL`,
    /// else the built-in default — see `configured_channel`.
    channel: Option<String>,
    /// Replace the **server** binary and hand the daemon over to it (T-0038),
    /// instead of swapping this client binary (T-0070). Two different
    /// operations with the same verb because they are one story to an operator
    /// ("update arreo"), and different flags because they act on different
    /// processes.
    server: bool,
    /// How long to wait for the handoff to complete. Only meaningful with
    /// `--server`.
    timeout_secs: Option<u64>,
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
            "--channel" => {
                args.channel = Some(value()?);
                i += 2;
            }
            "--server" => {
                args.server = true;
                i += 1;
            }
            "--timeout-secs" => {
                let raw = value()?;
                args.timeout_secs = Some(raw.parse().map_err(|_| {
                    format!("--timeout-secs needs a number of seconds, got {raw:?}")
                })?);
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
    if args.check
        && (args.from.is_some() || args.server || args.no_reexec || args.reattach_pane.is_some())
    {
        // `--check` reports what the channel publishes and installs nothing, so a
        // flag that describes an install — or the daemon swap — would be obeyed
        // silently, which is exactly the class of flag this verb refuses.
        return Err(
            "--check only reports what the channel publishes; it takes no --from, --server, \
             --no-reexec or --reattach-pane"
                .into(),
        );
    }
    if args.channel.is_some() && (args.from.is_some() || args.rollback || args.server) {
        // `--channel` names where the *anonymous* update reads from; a `--from`
        // install never reads a channel, `--rollback` only reads `.prev`, and the
        // server half has no channel story yet (T-0038). Accepting it silently
        // would let an operator believe a channel was consulted when it was not.
        return Err(
            "--channel names where the anonymous update fetches from; it cannot be combined with \
             --from, --rollback or --server"
                .into(),
        );
    }
    if args.server && (args.reattach_pane.is_some() || args.no_reexec) {
        // Both flags describe what *this* client process does after a swap. The
        // server path swaps a different binary and hands a daemon over, so
        // neither has a meaning — and accepting them silently would let an
        // operator believe their pane was resumed when nothing looked at it.
        return Err(
            "--reattach-pane and --no-reexec describe this client; --server swaps the daemon"
                .into(),
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

    /// Every combination of the readiness rule, as a table. The facts are
    /// parameters precisely so this test is deterministic: a version that read
    /// `/proc` itself would depend on whether the pid the test happened to choose
    /// exists on the machine, which is not a property of the rule.
    ///
    /// The first row is the race that matters. During a handoff the incoming
    /// daemon is already answering while the outgoing one still runs — so
    /// "a different pid answers" alone would report a cut that has not happened.
    /// That is the row a naive implementation gets wrong, and the reason the rule
    /// is a function instead of a condition buried in the poll loop.
    #[test]
    fn readiness_requires_the_outgoing_daemon_to_be_gone() {
        // A different daemon answers while the old one still runs: NOT a takeover.
        assert_eq!(takeover(Some(200), 100, true), None);
        // The old daemon is gone and a different one answers: the cut happened.
        assert_eq!(takeover(Some(200), 100, false), Some(200));
        // The pid answering is still the outgoing daemon: no cut has occurred.
        assert_eq!(takeover(Some(100), 100, false), None);
        // Nothing answers yet, whatever the outgoing daemon is doing.
        assert_eq!(takeover(None, 100, false), None);
        assert_eq!(takeover(None, 100, true), None);
        // And a re-answer from the same pid while the old daemon runs is not a cut.
        assert_eq!(takeover(Some(100), 100, true), None);
    }

    /// The same rule against the real world: a pid that has certainly exited is
    /// not running, and with it gone a different pid is accepted.
    #[test]
    fn a_reaped_pid_is_not_running() {
        let mut child = std::process::Command::new("/bin/true")
            .spawn()
            .expect("spawn a process to outlive");
        let pid = child.id();
        child.wait().expect("reap it");
        assert!(!daemon_running(pid), "a reaped pid is not running");
        assert_eq!(
            takeover(Some(pid + 1), pid, daemon_running(pid)),
            Some(pid + 1),
            "a different pid, with the old one gone, is a takeover"
        );
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
        // `--check` reports what the channel publishes and installs nothing, so a
        // flag that describes an install (or the daemon swap) alongside it is a
        // usage error rather than a silently ignored one.
        assert!(parse(&args(&["--check", "--from", "/tmp/x"])).is_err());
        assert!(parse(&args(&["--check", "--server"])).is_err());
        assert!(parse(&args(&["--check", "--no-reexec"])).is_err());
        // `--channel` names where the anonymous update reads from; `--from` never
        // reads a channel.
        assert!(parse(&args(&["--channel", "file:///tmp/x", "--from", "/tmp/y"])).is_err());
        assert!(parse(&args(&["--channel", "file:///tmp/x", "--server"])).is_err());
    }

    /// `arreo update verify` takes one positional artifact and nothing that could
    /// be mistaken for a way past the check.
    ///
    /// The last two assertions are the "no bypass flag" claim as a test: a
    /// `--force` or `--insecure` gets the usage error, because a verifier that
    /// can be argued out of refusing is a verifier that will be.
    #[test]
    fn verify_takes_one_artifact_and_offers_no_bypass() {
        let parsed = parse_verify(&args(&[
            "/tmp/arreo-0.1.0",
            "--sig",
            "/tmp/arreo-0.1.0.minisig",
            "--manifest",
            "/tmp/SHA256SUMS",
        ]))
        .expect("parses");
        assert_eq!(parsed.artifact, "/tmp/arreo-0.1.0");
        assert_eq!(parsed.sig.as_deref(), Some("/tmp/arreo-0.1.0.minisig"));
        assert_eq!(parsed.manifest.as_deref(), Some("/tmp/SHA256SUMS"));
        assert!(!parsed.json);

        // No artifact at all is a usage error, not a check of something else.
        assert!(parse_verify(&args(&[])).is_err());
        // Two artifacts: the second would otherwise be silently ignored.
        assert!(parse_verify(&args(&["/tmp/a", "/tmp/b"])).is_err());
        // A flag that needs a value and does not get one.
        assert!(parse_verify(&args(&["/tmp/a", "--sig"])).is_err());
        // And there is no flag that skips the check.
        assert!(parse_verify(&args(&["/tmp/a", "--force"])).is_err());
        assert!(parse_verify(&args(&["/tmp/a", "--insecure"])).is_err());
    }
}
