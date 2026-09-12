//! T-0038: `arreo update --server` — the daemon half of the update.
//!
//! The client half (`arreo update --from`) is covered in `tests/update.rs`. This
//! file covers the verb that replaces `arreo-server` and hands a running daemon
//! over to the new binary, and it exists separately because the two operations
//! fail in different ways: a client swap can only hurt the client, while a server
//! update can take every agent on the machine down with it.
//!
//! ## What is tested here, and what is not
//!
//! Here: the refusals (a candidate that is not a server, one that does not run,
//! flags that describe the client), the no-daemon case, rollback, and the JSON
//! shape. The **handoff itself** is exercised by the daemon's own integration
//! tests (`crates/arreo-server/tests/handoff.rs`), which own the two-process
//! mechanics; this file's job is the verb's contract around them.
//!
//! ## The trap this file exists to avoid repeating
//!
//! **Never run this against `target/debug/arreo-server`.** `--server` installs
//! over the binary beside the running client, so a test that pointed it at the
//! build tree would swap the build's own daemon out from under every later test.
//! Every test copies both binaries into its own scratch directory first.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The client binary under test.
fn arreo() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arreo"))
}

/// The daemon binary built beside it (a real `arreo-server`).
fn arreo_server() -> PathBuf {
    arreo().parent().expect("debug dir").join("arreo-server")
}

/// A scratch directory that removes itself, even when a test fails. A debug
/// binary is ~128 MB and a failing test that skipped cleanup once filled a 12 GB
/// tmpfs; see `tests/update.rs`.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Scratch {
    /// A scratch directory **on the build tree's filesystem**.
    ///
    /// Not `std::env::temp_dir()`: on this box `/tmp` is a tmpfs and `target/` is
    /// on the root filesystem, so a hard link across them fails with `EXDEV` — the
    /// link falls back to a copy, and a copy of a 142 MB daemon per parallel test
    /// exhausts the tmpfs (`StorageFull` inside `copy_as`, which reads as anything
    /// but a disk problem). Keeping the scratch beside the binaries makes the link
    /// work, so the installed pair costs no space at all, and 95 GB of root
    /// filesystem replaces 1.9 GB of tmpfs as the headroom.
    ///
    /// Derived from this test binary's own path — `target/debug/deps/x` →
    /// `target/debug` → `target/test-scratch` — so it follows `CARGO_TARGET_DIR`
    /// rather than assuming the default layout.
    fn new(tag: &str) -> Self {
        let target = std::env::current_exe()
            .expect("test exe")
            .parent()
            .expect("deps dir")
            .parent()
            .expect("debug dir")
            .parent()
            .expect("target dir")
            .to_path_buf();
        let dir = target.join("test-scratch").join(format!(
            "arreo-cli-server-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

/// Install a binary at `dest` by **hard link** where possible.
///
/// A debug `arreo-server` is 142 MB and `arreo` 123 MB, and this file's tests run
/// in parallel: real copies cost ~265 MB per test, which exhausted a 12 GB tmpfs
/// during a full-suite run (and again during a targeted run, where the failure
/// surfaced as `StorageFull` inside this function rather than as anything about
/// the code under test).
///
/// A link is safe here for the same reason it is in `tests/update.rs`: the
/// update's only mutation is renaming a directory entry, so replacing `dest`
/// leaves the build tree's file untouched — the link *is* the thing being
/// replaced. `copy_distinct` deliberately still copies, because appending a byte
/// is what makes the candidate a different file.
fn copy_as(source: &Path, dest: &Path) {
    if std::fs::hard_link(source, dest).is_err() {
        std::fs::copy(source, dest).expect("copy the binary");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755)).expect("mode");
    }
}

/// A second, distinguishable, still-working binary: the same bytes plus one
/// trailing byte, which an ELF loader ignores.
fn copy_distinct(source: &Path, dest: &Path) {
    // A real copy: the appended byte is what makes it a different file, and a
    // hard link would change the original too.
    std::fs::copy(source, dest).expect("copy the binary");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755)).expect("mode");
    }
    let mut bytes = std::fs::read(dest).expect("read");
    bytes.push(b'\n');
    std::fs::write(dest, bytes).expect("write");
}

/// Install both binaries in a scratch directory, the way a real install has them.
/// Returns the client path to run.
fn install_pair(scratch: &Scratch) -> PathBuf {
    let client = scratch.path().join("arreo");
    copy_as(&arreo(), &client);
    copy_as(&arreo_server(), &scratch.path().join("arreo-server"));
    client
}

struct Run {
    code: i32,
    out: String,
}

impl Run {
    fn ok(&self) -> bool {
        self.code == 0
    }
}

/// Run the installed client with a socket path that belongs to the scratch dir,
/// so nothing can reach a daemon the developer happens to be running.
fn run(client: &Path, scratch: &Scratch, args: &[&str]) -> Run {
    let socket = scratch.path().join("arreo.sock");
    let output = Command::new(client)
        .args(args)
        .arg("--socket")
        .arg(&socket)
        .env("ARREO_STATE_DIR", scratch.path().join("state"))
        .output()
        .expect("the client runs");
    let mut out = String::from_utf8_lossy(&output.stdout).into_owned();
    out.push_str(&String::from_utf8_lossy(&output.stderr));
    Run {
        code: output.status.code().unwrap_or(-1),
        out,
    }
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_default()
}

/// Flags that describe what *this client* does after a swap are refused on the
/// server path rather than ignored: silently accepting `--reattach-pane` would
/// let an operator believe their pane was resumed when nothing looked at it.
#[test]
fn client_only_flags_are_refused_on_the_server_path() {
    let scratch = Scratch::new("flags");
    let client = install_pair(&scratch);
    let candidate = server_script(&scratch, "arreo-server-new");
    let server = scratch.path().join("arreo-server");
    let original = bytes(&server);

    for args in [
        vec!["update", "--server"],
        vec![
            "update",
            "--server",
            "--from",
            candidate.to_str().unwrap(),
            "--reattach-pane",
            "p1",
        ],
        vec![
            "update",
            "--server",
            "--from",
            candidate.to_str().unwrap(),
            "--no-reexec",
        ],
    ] {
        let refused = run(&client, &scratch, &args);
        assert_eq!(refused.code, 2, "{args:?} → {}", refused.out);
    }
    assert_eq!(bytes(&server), original, "a usage error installs nothing");
}

/// `--server` replaces the daemon, so a candidate that is not one is refused.
/// `arreo --version` succeeds, so "it runs" cannot be the only check: installing
/// a client over the server path leaves a daemon that cannot be started.
#[test]
fn a_candidate_that_is_not_a_server_is_refused() {
    let scratch = Scratch::new("not-server");
    let client = install_pair(&scratch);
    let server = scratch.path().join("arreo-server");
    let original = bytes(&server);

    // A candidate that runs and reports a *client* version: the check under test
    // is that `--server` refuses it, so the stand-in only has to answer.
    let imposter = scratch.path().join("client-copy");
    write_script(
        &imposter,
        "case \"$1\" in\n  --version) echo 'arreo 0.1.0'; exit 0 ;;\nesac\nexit 1",
    );

    let refused = run(
        &client,
        &scratch,
        &["update", "--server", "--from", imposter.to_str().unwrap()],
    );
    assert_eq!(refused.code, 1, "{}", refused.out);
    assert!(
        refused.out.contains("not an arreo server"),
        "the refusal says why: {}",
        refused.out
    );
    assert_eq!(bytes(&server), original, "the daemon binary is untouched");
    assert!(
        !scratch.path().join("arreo-server.prev").exists(),
        "and no previous binary was manufactured"
    );
}

/// A candidate that cannot run at all is refused before anything is installed.
#[test]
fn a_candidate_that_cannot_run_is_refused() {
    let scratch = Scratch::new("cannot-run");
    let client = install_pair(&scratch);
    let server = scratch.path().join("arreo-server");
    let original = bytes(&server);

    let text = scratch.path().join("not-a-binary");
    std::fs::write(&text, "this is not a program\n").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&text, std::fs::Permissions::from_mode(0o644)).expect("mode");
    }

    let refused = run(
        &client,
        &scratch,
        &["update", "--server", "--from", text.to_str().unwrap()],
    );
    assert_eq!(refused.code, 1, "{}", refused.out);
    assert_eq!(bytes(&server), original);
    assert!(!scratch.path().join("arreo-server.staged").exists());
}

/// With no daemon serving the socket there is nothing to hand over, and the verb
/// must say so rather than implying it restarted something. The install still
/// happens — the next start runs the new binary.
#[test]
fn with_no_daemon_it_installs_and_says_so() {
    let scratch = Scratch::new("no-daemon");
    let client = install_pair(&scratch);
    let server = scratch.path().join("arreo-server");
    let original = bytes(&server);
    let candidate = server_script(&scratch, "arreo-server-new");

    let done = run(
        &client,
        &scratch,
        &["update", "--server", "--from", candidate.to_str().unwrap()],
    );
    assert!(done.ok(), "{}", done.out);
    assert!(
        done.out.contains("no daemon was serving"),
        "it names the state rather than claiming a handoff: {}",
        done.out
    );
    assert_eq!(
        bytes(&server),
        bytes(&candidate),
        "the new binary is installed"
    );
    assert_eq!(
        bytes(&scratch.path().join("arreo-server.prev")),
        original,
        "and the previous one is kept"
    );
    assert!(
        !done.out.contains("handed over"),
        "no handoff is claimed when none happened: {}",
        done.out
    );
}

/// Re-running the same install changes nothing and does not churn `.prev`.
#[test]
fn installing_the_same_bytes_is_a_no_op() {
    let scratch = Scratch::new("noop");
    let client = install_pair(&scratch);
    let server = scratch.path().join("arreo-server");

    let done = run(
        &client,
        &scratch,
        &["update", "--server", "--from", server.to_str().unwrap()],
    );
    assert!(done.ok(), "{}", done.out);
    assert!(
        done.out.contains("byte-identical"),
        "the no-op is stated: {}",
        done.out
    );
    assert!(
        !scratch.path().join("arreo-server.prev").exists(),
        "a no-op does not manufacture a previous binary"
    );
}

/// Rollback restores the previous bytes, and says that a running daemon is not
/// affected until it restarts — the honest scope of what it did.
#[test]
fn rollback_restores_the_previous_server_binary() {
    let scratch = Scratch::new("rollback");
    let client = install_pair(&scratch);
    let server = scratch.path().join("arreo-server");
    let original = bytes(&server);
    let candidate = server_script(&scratch, "arreo-server-new");

    let installed = run(
        &client,
        &scratch,
        &["update", "--server", "--from", candidate.to_str().unwrap()],
    );
    assert!(installed.ok(), "{}", installed.out);

    let rolled = run(&client, &scratch, &["update", "--server", "--rollback"]);
    assert!(rolled.ok(), "{}", rolled.out);
    assert_eq!(bytes(&server), original, "the original bytes are back");
    assert!(
        rolled.out.contains("unchanged"),
        "it says the running daemon is unaffected: {}",
        rolled.out
    );

    let again = run(&client, &scratch, &["update", "--server", "--rollback"]);
    assert_eq!(again.code, 1, "{}", again.out);
    assert!(again.out.contains("no previous binary"), "{}", again.out);
}

/// The `--json` shape, so a script can tell an install from a no-op without
/// parsing prose.
#[test]
fn the_json_reports_the_outcome() {
    let scratch = Scratch::new("json");
    let client = install_pair(&scratch);
    let candidate = server_script(&scratch, "arreo-server-new");

    let done = run(
        &client,
        &scratch,
        &[
            "update",
            "--server",
            "--from",
            candidate.to_str().unwrap(),
            "--json",
        ],
    );
    assert!(done.ok(), "{}", done.out);
    let value: serde_json::Value = serde_json::from_str(done.out.trim()).expect("one JSON object");
    assert_eq!(value["changed"], serde_json::json!(true));
    assert_eq!(
        value["installed"],
        serde_json::json!(scratch.path().join("arreo-server").display().to_string())
    );
    assert!(
        value["version"]
            .as_str()
            .unwrap_or_default()
            .contains("arreo-server"),
        "the reported version identifies a server: {}",
        value["version"]
    );
    assert_eq!(
        value["handoff"]["daemon"],
        serde_json::Value::Null,
        "no daemon was serving, and the JSON says so rather than omitting it"
    );
}

// ---------------------------------------------------------------------------
// The failure paths, against a live daemon.
//
// These are the tests that matter most: the promise `--server` makes is that a
// handoff which cannot complete **changes nothing** — no install, no `.prev`
// churn, and the daemon that was serving keeps serving. Each one therefore
// asserts the daemon's liveness and the binary's bytes, not just an exit code.
// ---------------------------------------------------------------------------

/// A daemon started for a test, killed and reaped on drop.
struct Daemon {
    child: std::process::Child,
    pid: u32,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Start a daemon on `socket` and wait until it answers (polled, never slept).
///
/// The failure path reaps the child before panicking: a test that leaves a
/// process behind on a scratch socket is the "leaked process is
/// indistinguishable from a product defect" trap this project has already paid
/// for once, and a panic unwinds past everything that would have cleaned up.
fn start_daemon(server: &Path, socket: &Path, scratch: &Scratch) -> Daemon {
    let mut child = Command::new(server)
        .arg("--socket")
        .arg(socket)
        .env("ARREO_STATE_DIR", scratch.path().join("state"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("the daemon starts");
    let pid = child.id();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            return Daemon { child, pid };
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // Reap before failing, so a test that cannot start a daemon does not also
    // leave one running.
    let _ = child.kill();
    let _ = child.wait();
    panic!("the daemon never came up on {}", socket.display());
}

/// Is this pid still running? A zombie counts as not running — the same rule the
/// verb uses, for the same reason.
fn running(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat
            .rsplit_once(") ")
            .map(|(_, rest)| !rest.starts_with('Z'))
            .unwrap_or(false),
        Err(_) => false,
    }
}

/// A **stand-in server binary**: a script that answers `--version` the way a
/// server does and then fails, sleeps, or does nothing.
///
/// Most tests in this file are about the *verb's* contract — what it installs,
/// what it refuses, what it leaves behind — and for those the candidate only has
/// to run and identify itself. Using a real 142 MB `arreo-server` copy for each
/// made a parallel run exhaust a 12 GB tmpfs (`StorageFull` inside `copy_as`,
/// which reads as anything but a disk problem); a script is a few hundred bytes
/// and states the requirement more honestly.
///
/// The one test that needs a real binary is the handoff itself, which copies the
/// real `arreo-server` because nothing smaller can take a socket over.
fn server_script(scratch: &Scratch, name: &str) -> PathBuf {
    let path = scratch.path().join(name);
    write_script(
        &path,
        "case \"$1\" in\n  --version) echo 'arreo-server 9.9.9 (a test stand-in)'; exit 0 ;;\nesac\nexit 1",
    );
    path
}

/// Write an executable script — a candidate that answers `--version` like a
/// server and then does whatever the test needs it to do.
fn write_script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).expect("write the script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("mode");
    }
}

/// **The central promise.** A candidate that starts, claims to be a server, and
/// then fails must leave the machine exactly as it was: the daemon serving, the
/// socket answering, the binary byte-identical, and no `.prev` or `.staged` left
/// behind. With the install step moved before the handoff — the tempting order —
/// this test fails on the binary's bytes.
#[test]
fn a_candidate_that_fails_the_handoff_changes_nothing() {
    let scratch = Scratch::new("failed-handoff");
    let client = install_pair(&scratch);
    let server = scratch.path().join("arreo-server");
    let socket = scratch.path().join("arreo.sock");
    let original = bytes(&server);
    let daemon = start_daemon(&server, &socket, &scratch);

    let fake = scratch.path().join("fake-server");
    write_script(
        &fake,
        "case \"$1\" in\n  --version) echo 'arreo-server 9.9.9 (a fake)'; exit 0 ;;\nesac\necho 'fake: refusing to take over' >&2\nexit 1",
    );

    let failed = run(
        &client,
        &scratch,
        &["update", "--server", "--from", fake.to_str().unwrap()],
    );
    assert_eq!(failed.code, 1, "{}", failed.out);
    assert!(
        failed.out.contains("still running"),
        "it tells the operator the daemon is fine: {}",
        failed.out
    );
    assert!(running(daemon.pid), "the daemon is still running");
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_ok(),
        "and the socket still answers"
    );
    assert_eq!(bytes(&server), original, "nothing was installed");
    assert!(
        !scratch.path().join("arreo-server.prev").exists(),
        "and no previous binary was manufactured"
    );
    assert!(
        !scratch.path().join("arreo-server.staged").exists(),
        "and the staged copy was cleaned up"
    );
    assert!(
        !scratch.path().join("arreo.sock.handoff").exists(),
        "and the handoff socket was released"
    );
}

/// A candidate that hangs is abandoned after the timeout, and abandoning it
/// changes nothing either.
#[test]
fn a_candidate_that_hangs_is_abandoned_without_an_install() {
    let scratch = Scratch::new("hanging");
    let client = install_pair(&scratch);
    let server = scratch.path().join("arreo-server");
    let socket = scratch.path().join("arreo.sock");
    let original = bytes(&server);
    let daemon = start_daemon(&server, &socket, &scratch);

    let hang = scratch.path().join("hang-server");
    write_script(
        &hang,
        "case \"$1\" in\n  --version) echo 'arreo-server 9.9.9 (hangs)'; exit 0 ;;\nesac\nsleep 60",
    );

    let started = std::time::Instant::now();
    let failed = run(
        &client,
        &scratch,
        &[
            "update",
            "--server",
            "--from",
            hang.to_str().unwrap(),
            "--timeout-secs",
            "2",
        ],
    );
    assert_eq!(failed.code, 1, "{}", failed.out);
    assert!(
        failed.out.contains("did not complete within 2s"),
        "the timeout is reported as a timeout: {}",
        failed.out
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "and it gave up promptly rather than hanging"
    );
    assert!(running(daemon.pid), "the daemon is still running");
    assert_eq!(bytes(&server), original, "nothing was installed");
    assert!(!scratch.path().join("arreo-server.prev").exists());
}

/// A second updater is refused while one holds the lock, and the refusal does
/// not disturb the daemon (T-0070's lock, doing the same job on the server path).
#[test]
fn a_second_server_update_is_refused() {
    let scratch = Scratch::new("locked");
    let client = install_pair(&scratch);
    let server = scratch.path().join("arreo-server");
    let socket = scratch.path().join("arreo.sock");
    let daemon = start_daemon(&server, &socket, &scratch);

    let lock_path = scratch.path().join("arreo-server.update.lock");
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open the lock file");
    held.try_lock().expect("the test takes the lock");

    let refused = run(
        &client,
        &scratch,
        &["update", "--server", "--from", server.to_str().unwrap()],
    );
    assert_eq!(refused.code, 3, "{}", refused.out);
    assert!(
        refused.out.contains("already in progress"),
        "and says so: {}",
        refused.out
    );
    assert!(running(daemon.pid));
    assert!(std::os::unix::net::UnixStream::connect(&socket).is_ok());
}

/// **The whole point.** With a daemon running, `--server` hands it over: the
/// socket keeps answering on the same path, the process serving it is a
/// different pid, the old one is gone, and only then is the binary installed.
///
/// This is the one test that exercises the verb's readiness detection against a
/// real handoff. It asserts the *pid changed* rather than that a message was
/// printed, because that is the fact an operator depends on — and a candidate
/// that printed the right words without taking over cannot satisfy it.
#[test]
fn a_running_daemon_is_handed_over_to_the_new_binary() {
    let scratch = Scratch::new("handoff");
    let client = install_pair(&scratch);
    let server = scratch.path().join("arreo-server");
    let socket = scratch.path().join("arreo.sock");
    let original = bytes(&server);
    let daemon = start_daemon(&server, &socket, &scratch);
    let old_pid = daemon.pid;

    let candidate = scratch.path().join("arreo-server-new");
    copy_distinct(&arreo_server(), &candidate);

    let done = run(
        &client,
        &scratch,
        &["update", "--server", "--from", candidate.to_str().unwrap()],
    );
    assert!(done.ok(), "{}", done.out);
    assert!(
        done.out.contains("handed over"),
        "the cut is reported: {}",
        done.out
    );
    assert!(
        !done.out.contains("no daemon was serving"),
        "and not confused with the no-daemon case: {}",
        done.out
    );

    // The old process is gone — record it before the assertion so a failure says
    // which half broke.
    let old_still_running = running(old_pid);
    assert!(!old_still_running, "the outgoing daemon exited");
    assert!(
        std::os::unix::net::UnixStream::connect(&socket).is_ok(),
        "the socket still answers on the same path"
    );

    // A different daemon is serving: found by the same /proc scan the verb uses.
    let new_pid = find_daemon(&socket).expect("a daemon serves the socket");
    assert_ne!(new_pid, old_pid, "and it is a different process");

    // Only now the install: the new daemon runs the new bytes.
    assert_eq!(
        bytes(&server),
        bytes(&candidate),
        "the new binary is installed"
    );
    assert_eq!(
        bytes(&scratch.path().join("arreo-server.prev")),
        original,
        "and the previous one is kept for rollback"
    );
    // The handoff socket is cleaned up, so a second update is not confused by it.
    assert!(!scratch.path().join("arreo.sock.handoff").exists());
}

/// Find the daemon serving `socket` the way the verb does: a process whose argv
/// runs `arreo-server` and names this socket.
fn find_daemon(socket: &Path) -> Option<u32> {
    let want = socket.to_string_lossy().to_string();
    for entry in std::fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else { continue };
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let parts: Vec<&str> = cmdline
            .split(|b| *b == 0)
            .filter_map(|s| std::str::from_utf8(s).ok())
            .collect();
        if parts.iter().any(|p| p.ends_with("arreo-server")) && parts.iter().any(|p| *p == want) {
            return Some(pid);
        }
    }
    None
}
