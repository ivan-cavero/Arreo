//! T-0070: `arreo update` end to end, through the real binary.
//!
//! The unit tests in `arreo_core::update` prove the swap's mechanics; the `update`
//! slice proves the invariant against a live daemon. This file sits between them:
//! it drives the **verb** — argument handling, exit codes, what an operator sees —
//! against real files, and it is where the round trip (install → roll back) is
//! checked byte for byte.
//!
//! ## The trap this file exists to avoid repeating
//!
//! **Never run `update` against `target/debug/arreo`.** The verb installs over the
//! binary it is running from, so a test that invoked it there would swap the build
//! tree's own binary out from under every later test in the workspace. Every test
//! here copies the real binary into its own scratch directory and drives the copy.
//!
//! ## Two distinct binaries that both work
//!
//! The candidate must differ from the installed one for a swap to happen, and it
//! must still *run* so `--version` and `--rollback` can be exercised. Appending a
//! byte to a copy gives exactly that: an ELF loader ignores trailing bytes, so the
//! copy is a different file that behaves identically. The alternative — a script
//! that prints a version — cannot run `--rollback`, which is half of what is being
//! tested.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The `arreo` binary under test.
fn arreo() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arreo"))
}

/// A scratch directory that removes itself, even when a test fails.
///
/// Not tidiness: a debug `arreo` is ~128 MB, each test keeps two of them, and a
/// failing test that skips its own cleanup filled a 12 GB tmpfs in one run. A
/// guard makes that impossible rather than unlikely.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl Scratch {
    /// A scratch directory **on the build tree's filesystem**, so `copy_as`'s hard
    /// link works.
    ///
    /// `std::env::temp_dir()` is a tmpfs here while `target/` is on the root
    /// filesystem; a link across them fails with `EXDEV` and silently degrades to
    /// a 123 MB copy per parallel test, which exhausted the tmpfs during a
    /// full-suite run. See `tests/update_server.rs`, where the same fix landed for
    /// the same reason — one cause, two files.
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
            "arreo-cli-update-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

/// Install a copy of the real binary at `dest`.
///
/// A **hard link**, not a copy: the same inode, so it costs nothing, and the
/// update's only mutation is renaming a directory entry — which replaces this
/// link and leaves the build tree's binary untouched. 128 MB per test would
/// otherwise be paid several times over.
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

/// A second, distinguishable, *working* binary: the same bytes plus one trailing
/// byte, which an ELF loader ignores.
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

/// What one invocation produced.
struct Run {
    code: i32,
    out: String,
}

impl Run {
    fn ok(&self) -> bool {
        self.code == 0
    }
}

/// Run the installed copy, in its own state directory so the resume token of a
/// test never touches the real one.
fn run(installed: &Path, state: &Path, args: &[&str]) -> Run {
    let output = Command::new(installed)
        .args(args)
        .env("ARREO_STATE_DIR", state)
        .output()
        .expect("the installed binary runs");
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

/// The round trip: install a distinct binary, then put the previous one back.
///
/// Both halves are asserted on **bytes**, not on a version string, because the
/// claim is "the previous binary is preserved and can be restored" — a claim about
/// files.
#[test]
fn an_install_then_a_rollback_returns_the_original_bytes() {
    let scratch = Scratch::new("round-trip");
    let installed = scratch.path().join("arreo");
    let candidate = scratch.path().join("arreo-new");
    let state = scratch.path().join("state");
    copy_as(&arreo(), &installed);
    copy_distinct(&arreo(), &candidate);
    let original = bytes(&installed);
    assert_ne!(
        original,
        bytes(&candidate),
        "the candidate must differ, or nothing would be installed"
    );

    // The operator's view before: it runs, and it says what it is.
    let before = run(&installed, &state, &["--version"]);
    assert!(before.ok(), "the installed binary runs: {}", before.out);

    let updated = run(
        &installed,
        &state,
        &[
            "update",
            "--from",
            &candidate.display().to_string(),
            "--no-reexec",
        ],
    );
    assert!(updated.ok(), "install failed: {}", updated.out);
    assert_eq!(
        bytes(&installed),
        bytes(&candidate),
        "the installed binary is now the candidate"
    );
    assert_eq!(
        bytes(&scratch.path().join("arreo.prev")),
        original,
        "the previous binary is preserved, byte for byte"
    );
    let after = run(&installed, &state, &["--version"]);
    assert!(after.ok(), "the new binary runs: {}", after.out);

    let rolled_back = run(&installed, &state, &["update", "--rollback"]);
    assert!(rolled_back.ok(), "rollback failed: {}", rolled_back.out);
    assert_eq!(
        bytes(&installed),
        original,
        "rollback restores the original bytes"
    );
    assert!(
        run(&installed, &state, &["--version"]).ok(),
        "and the restored binary runs"
    );

    // A second rollback has nothing to restore, and says so rather than
    // pretending: exit 1, naming the path it looked for.
    let again = run(&installed, &state, &["update", "--rollback"]);
    assert_eq!(again.code, 1, "{}", again.out);
    assert!(
        again.out.contains("no previous binary"),
        "the refusal names what is missing: {}",
        again.out
    );
}

/// Re-running the same install is a no-op, which is what makes the re-exec safe:
/// after the swap the new binary runs the same command and must stop rather than
/// install itself again.
#[test]
fn installing_the_binary_that_is_already_installed_changes_nothing() {
    let scratch = Scratch::new("no-op");
    let installed = scratch.path().join("arreo");
    let state = scratch.path().join("state");
    copy_as(&arreo(), &installed);
    let original = bytes(&installed);

    let run_once = run(
        &installed,
        &state,
        &[
            "update",
            "--from",
            &installed.display().to_string(),
            "--no-reexec",
        ],
    );
    assert!(run_once.ok(), "{}", run_once.out);
    assert!(
        run_once.out.contains("byte-identical"),
        "the no-op is stated, not silent: {}",
        run_once.out
    );
    assert_eq!(bytes(&installed), original);
    assert!(
        !scratch.path().join("arreo.prev").exists(),
        "a no-op does not manufacture a previous binary"
    );
}

/// Without `--no-reexec` the new binary takes over and finishes the command. The
/// line that proves it is the *second* run's "already running" — which is the new
/// binary executing, not the old one.
#[test]
fn the_new_binary_finishes_the_command_after_the_swap() {
    let scratch = Scratch::new("reexec");
    let installed = scratch.path().join("arreo");
    let candidate = scratch.path().join("arreo-new");
    let state = scratch.path().join("state");
    copy_as(&arreo(), &installed);
    copy_distinct(&arreo(), &candidate);

    let updated = run(
        &installed,
        &state,
        &["update", "--from", &candidate.display().to_string()],
    );
    assert!(updated.ok(), "{}", updated.out);
    assert!(
        updated.out.contains("installed") && updated.out.contains("byte-identical"),
        "the handover is both announced and completed: {}",
        updated.out
    );
}

/// A candidate that cannot run is refused **before** anything is installed: an
/// updater that publishes a broken binary has broken the install.
#[test]
fn a_candidate_that_cannot_run_leaves_the_install_untouched() {
    let scratch = Scratch::new("refused");
    let installed = scratch.path().join("arreo");
    let state = scratch.path().join("state");
    copy_as(&arreo(), &installed);
    let original = bytes(&installed);

    let text = scratch.path().join("not-a-binary");
    std::fs::write(&text, "this is not a program\n").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&text, std::fs::Permissions::from_mode(0o644)).expect("mode");
    }

    let refused = run(
        &installed,
        &state,
        &[
            "update",
            "--from",
            &text.display().to_string(),
            "--no-reexec",
        ],
    );
    assert_eq!(refused.code, 1, "{}", refused.out);
    assert_eq!(
        bytes(&installed),
        original,
        "a refused update leaves the binary byte-identical"
    );
    assert!(
        !scratch.path().join("arreo.staged").exists(),
        "and leaves nothing staged"
    );
}

/// Two updaters at once: the second is told, rather than racing the first.
///
/// The lock is taken here by the *test* process, on the same file the verb uses,
/// which is the only way to hold it still while a second process runs. That the
/// lock is an OS lock is the point — it is held by the open file, so this works
/// across processes and cannot go stale.
#[cfg(unix)]
#[test]
fn a_second_updater_is_refused_while_the_lock_is_held() {
    let scratch = Scratch::new("lock");
    let installed = scratch.path().join("arreo");
    let candidate = scratch.path().join("arreo-new");
    let state = scratch.path().join("state");
    copy_as(&arreo(), &installed);
    copy_distinct(&arreo(), &candidate);
    let original = bytes(&installed);

    let lock_path = scratch.path().join("arreo.update.lock");
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .expect("open the lock file");
    held.try_lock().expect("the test takes the lock");

    let refused = run(
        &installed,
        &state,
        &[
            "update",
            "--from",
            &candidate.display().to_string(),
            "--no-reexec",
        ],
    );
    assert_eq!(refused.code, 3, "expected 'in progress': {}", refused.out);
    assert!(
        refused.out.contains("already in progress"),
        "and says so: {}",
        refused.out
    );
    assert_eq!(
        bytes(&installed),
        original,
        "the refused updater changed nothing"
    );

    // Released, the update proceeds: the refusal was about the lock, not the
    // request.
    held.unlock().expect("release");
    let ok = run(
        &installed,
        &state,
        &[
            "update",
            "--from",
            &candidate.display().to_string(),
            "--no-reexec",
        ],
    );
    assert!(ok.ok(), "after release it installs: {}", ok.out);
}

/// `--check` has no channel to check: the anonymous fetch-and-verify path needs
/// the signing key of T-0036, and this build must say so rather than pretend.
#[test]
fn check_refuses_because_there_is_no_release_channel() {
    let scratch = Scratch::new("check");
    let installed = scratch.path().join("arreo");
    let state = scratch.path().join("state");
    copy_as(&arreo(), &installed);
    let original = bytes(&installed);

    let checked = run(&installed, &state, &["update", "--check"]);
    assert_eq!(checked.code, 2, "{}", checked.out);
    assert!(
        checked.out.contains("no release channel") && checked.out.contains("--from"),
        "the refusal names the working alternative: {}",
        checked.out
    );
    assert_eq!(
        bytes(&installed),
        original,
        "--check changes nothing, which is the whole of what it promises"
    );
}

/// The `--json` shape, so a script can tell a real install from a no-op without
/// parsing prose.
#[test]
fn the_json_reports_whether_anything_changed() {
    let scratch = Scratch::new("json");
    let installed = scratch.path().join("arreo");
    let candidate = scratch.path().join("arreo-new");
    let state = scratch.path().join("state");
    copy_as(&arreo(), &installed);
    copy_distinct(&arreo(), &candidate);

    let installed_json = run(
        &installed,
        &state,
        &[
            "update",
            "--from",
            &candidate.display().to_string(),
            "--no-reexec",
            "--json",
        ],
    );
    assert!(installed_json.ok(), "{}", installed_json.out);
    let value: serde_json::Value =
        serde_json::from_str(installed_json.out.trim()).expect("one JSON object");
    assert_eq!(value["changed"], serde_json::json!(true));
    assert_eq!(
        value["previous"],
        serde_json::json!(scratch.path().join("arreo.prev").display().to_string())
    );

    let noop = run(
        &installed,
        &state,
        &[
            "update",
            "--from",
            &candidate.display().to_string(),
            "--no-reexec",
            "--json",
        ],
    );
    assert!(noop.ok(), "{}", noop.out);
    let value: serde_json::Value = serde_json::from_str(noop.out.trim()).expect("json");
    assert_eq!(
        value["changed"],
        serde_json::json!(false),
        "the second install of the same bytes reports no change"
    );
}

/// Usage errors are refused before anything is read or written: a flag that needs
/// a value and does not get one must not be treated as a default.
#[test]
fn a_bad_invocation_is_refused_without_touching_the_install() {
    let scratch = Scratch::new("usage");
    let installed = scratch.path().join("arreo");
    let state = scratch.path().join("state");
    copy_as(&arreo(), &installed);
    let original = bytes(&installed);

    for args in [
        vec!["update"],
        vec!["update", "--nonsense"],
        vec!["update", "--from"],
        vec!["update", "--rollback", "--from", "/tmp/whatever"],
    ] {
        let refused = run(&installed, &state, &args);
        assert_eq!(refused.code, 2, "{args:?} → {}", refused.out);
    }
    assert_eq!(bytes(&installed), original);
}
