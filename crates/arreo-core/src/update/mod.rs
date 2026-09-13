//! The client update: stage a binary beside the running one and swap it in
//! atomically (T-0070, ROADMAP §3.13).
//!
//! One sentence: `arreo update --from <path>` puts a new binary in place without
//! ever leaving the path it replaces missing, non-executable, or half-written —
//! and without touching a single PTY-bearing process.
//!
//! ## The invariant, which is the point of the whole module
//!
//! **The client update path never signals, reaps, restarts or stops a
//! PTY-bearing process, and never stops the daemon.** It touches exactly three
//! things: the CLI/TUI binary path, the `.prev` slot beside it, and the resume
//! token. That is what makes an update *safe for the agents* — the daemon owns
//! the PTYs, and the client is not the daemon, so replacing the client cannot
//! end anyone's work. Nothing in this module can reach a process: there is no
//! `kill`, no `waitpid`, no service-manager call, and the only child process it
//! ever spawns is the new binary asking for `--version`.
//!
//! ## Why one rename, and why `.prev` is a hard link
//!
//! The obvious sequence — rename the current binary away, then rename the new one
//! in — has a window where the path holds nothing. A crash inside that window
//! leaves an install that cannot be run to repair itself, which is the worst
//! failure an updater can cause. So on Unix the order is:
//!
//! ```text
//! 1. copy the new binary to `<current>.staged`   (same directory: same filesystem)
//! 2. hard_link(current, `<current>.prev`)        (adds a name; removes nothing)
//! 3. rename(`<current>.staged`, current)         (atomic replace; one syscall)
//! ```
//!
//! Step 2 gives `.prev` a second name for the *old inode*, and step 3 is a single
//! atomic replacement — so **at no instant is `current` missing or
//! non-executable**, and a crash after any step leaves a runnable binary at the
//! path (old before step 3, new after it). The task that specified this predicted
//! a two-rename window and asked for crash-injection between the renames; the
//! hard link removes the window instead, which the tests assert step by step.
//!
//! Windows cannot do that: a running image cannot be replaced, only renamed away
//! (T-0039 owns the server half of that story). Its sequence keeps a bounded
//! window and restores the old binary if the second rename fails.
//!
//! ## Why the lock is an OS lock, not a pid file
//!
//! "Update already in progress" has to be true across processes, and a stale lock
//! has to stop being true when its holder dies without cleaning up. A pid file
//! answers the second question by *guessing* (is this pid alive? did it get
//! reused? does this platform have `/proc`?). `File::try_lock` answers it by
//! construction: the lock is held by the open file description, so the kernel
//! releases it when the process ends — cleanly, by signal, or by `SIGKILL`. No
//! timeout, no liveness probe, no stale case.

pub mod resume;

/// Release signature verification (T-0036): the one door every artifact passes
/// through before it is trusted, shared by the CLI, the daemon and the TUI.
pub mod verify;

use std::fs;
use std::path::{Path, PathBuf};

/// What can go wrong, each with the path it happened to.
///
/// The variants exist so the CLI can print the *right* advice: a path the process
/// may not write is a different conversation from a file that is not a binary.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("{path}: {detail}")]
    Io { path: String, detail: String },
    /// The candidate is not a runnable binary — refusing before the swap is the
    /// difference between a failed update and a broken install.
    #[error("{0} is not a runnable binary")]
    NotExecutable(String),
    #[error("another update is already in progress (holding {0})")]
    Locked(String),
    #[error("no previous binary at {0}: nothing to roll back to")]
    NoPrevious(String),
    /// The path is owned by a package manager, so replacing it would be undone by
    /// the next upgrade — and fighting it is not this tool's job.
    #[error("{path} is not writable by this user; if it was installed by a package manager, use it instead: {advice}")]
    NotWritable { path: String, advice: String },
}

impl UpdateError {
    fn io(path: &Path, detail: impl ToString) -> Self {
        Self::Io {
            path: path.display().to_string(),
            detail: detail.to_string(),
        }
    }
}

/// The running binary's real path, symlinks resolved.
///
/// Canonicalized on purpose: `arreo` is commonly a symlink into a versioned
/// directory, and both the swap and the `.prev` slot must name the *file*, not
/// the link — hard-linking a symlink would preserve the wrong thing.
pub fn current_binary() -> Result<PathBuf, UpdateError> {
    let exe = std::env::current_exe().map_err(|e| UpdateError::io(Path::new("argv[0]"), e))?;
    fs::canonicalize(&exe).map_err(|e| UpdateError::io(&exe, e))
}

/// Where the previous binary is kept: a sibling of `current`.
///
/// A sibling, not a cache directory, because the swap has to be one atomic
/// rename: a rename across filesystems is a copy, and a copy has a window.
#[must_use]
pub fn prev_path(current: &Path) -> PathBuf {
    with_suffix(current, "prev")
}

/// Where the candidate is staged before the swap — also a sibling, for the same
/// reason.
#[must_use]
pub fn staged_path(current: &Path) -> PathBuf {
    with_suffix(current, "staged")
}

fn with_suffix(current: &Path, suffix: &str) -> PathBuf {
    let mut name = current
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "arreo".to_string());
    name.push('.');
    name.push_str(suffix);
    current.with_file_name(name)
}

/// Are these two files byte-identical?
///
/// Length first, then contents: the comparison is used to decide whether an
/// update is a no-op, and a length mismatch settles most of those without
/// reading a multi-megabyte binary twice.
pub fn identical(a: &Path, b: &Path) -> Result<bool, UpdateError> {
    let (left, right) = (
        fs::metadata(a).map_err(|e| UpdateError::io(a, e))?,
        fs::metadata(b).map_err(|e| UpdateError::io(b, e))?,
    );
    if left.len() != right.len() {
        return Ok(false);
    }
    let (left, right) = (
        fs::read(a).map_err(|e| UpdateError::io(a, e))?,
        fs::read(b).map_err(|e| UpdateError::io(b, e))?,
    );
    Ok(left == right)
}

/// Is `path` a file this process could execute?
///
/// Checked *before* the swap: a staged copy that cannot be run must never reach
/// the binary path, because the whole reason for staging is that the candidate
/// might be wrong.
pub fn is_runnable(path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Any execute bit: the owner's is what matters for the common case, but
        // refusing a world-executable binary this user can run would be wrong.
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        // Windows decides by extension and the loader's own rules; a file that
        // exists and is not a directory is as far as this check can honestly go.
        true
    }
}

/// Copy `source` beside `current`, ready to be swapped in.
///
/// The copy lands in the **same directory** as the target so the rename that
/// follows is atomic (same filesystem), and it is made executable here so the
/// swap never publishes a binary the system cannot run.
pub fn stage(source: &Path, current: &Path) -> Result<PathBuf, UpdateError> {
    if !is_runnable(source) {
        return Err(UpdateError::NotExecutable(source.display().to_string()));
    }
    let staged = staged_path(current);
    let _ = fs::remove_file(&staged);
    // Copy the *bytes*, not the permissions: the candidate may come from a
    // download with an odd mode, and the installed binary's mode is this
    // tool's decision.
    let bytes = fs::read(source).map_err(|e| UpdateError::io(source, e))?;
    fs::write(&staged, &bytes).map_err(|e| UpdateError::io(&staged, e))?;
    set_executable_like(&staged, current)?;
    Ok(staged)
}

/// Give `path` the mode of the binary it will replace, plus owner-execute.
///
/// **The install's own mode, not a fixed `0755`.** An operator who installed the
/// client `0700` meant it, and an updater that widened the file to `0755` on
/// every run would quietly undo that. Deriving from the current binary also means
/// the mode is *stable* across updates rather than creeping wider each time.
fn set_executable_like(path: &Path, current: &Path) -> Result<(), UpdateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(current)
            .map(|meta| meta.permissions().mode())
            .unwrap_or(0o755);
        fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o100))
            .map_err(|e| UpdateError::io(path, e))?;
    }
    #[cfg(not(unix))]
    {
        let _ = current;
    }
    Ok(())
}

/// Make a file executable for the tests here. Kept separate from the install path
/// so a test can build a "runnable" fixture without implying a mode policy.
#[cfg(test)]
fn set_executable(path: &Path) -> Result<(), UpdateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)
            .map_err(|e| UpdateError::io(path, e))?
            .permissions()
            .mode();
        fs::set_permissions(path, fs::Permissions::from_mode(mode | 0o755))
            .map_err(|e| UpdateError::io(path, e))?;
    }
    Ok(())
}

/// Replace `current` with `staged`, keeping the old binary at `.prev`.
///
/// Unix: link, then **one** atomic rename (see the module docs). Windows: rename
/// away, then rename in, restoring the old binary if the second step fails.
pub fn swap(staged: &Path, current: &Path) -> Result<(), UpdateError> {
    let prev = prev_path(current);
    #[cfg(unix)]
    {
        // A `.prev` from an earlier update is replaced: the previous *binary* is
        // the one being replaced now, not the one replaced a month ago.
        let _ = fs::remove_file(&prev);
        fs::hard_link(current, &prev).map_err(|e| {
            // The one honest way this fails on a normal filesystem is a
            // package-manager install whose directory this user cannot write.
            UpdateError::NotWritable {
                path: current.display().to_string(),
                advice: package_manager_advice(current),
            }
            .tap_io(e)
        })?;
        fs::rename(staged, current).map_err(|e| {
            // Undo the link so a failed swap leaves no misleading `.prev`.
            let _ = fs::remove_file(&prev);
            UpdateError::NotWritable {
                path: current.display().to_string(),
                advice: package_manager_advice(current),
            }
            .tap_io(e)
        })?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = fs::remove_file(&prev);
        fs::rename(current, &prev).map_err(|e| UpdateError::io(current, e))?;
        match fs::rename(staged, current) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Put the old binary back: the path must not be left empty.
                let _ = fs::rename(&prev, current);
                Err(UpdateError::io(current, e))
            }
        }
    }
}

/// Restore `.prev` over `current`.
pub fn rollback(current: &Path) -> Result<(), UpdateError> {
    let prev = prev_path(current);
    if !prev.is_file() {
        return Err(UpdateError::NoPrevious(prev.display().to_string()));
    }
    // On Unix the running binary can be replaced under itself; on Windows the
    // current image can be renamed away but not overwritten, which is the same
    // two-step-and-restore shape `swap` uses.
    #[cfg(unix)]
    {
        fs::rename(&prev, current).map_err(|e| UpdateError::io(current, e))
    }
    #[cfg(not(unix))]
    {
        let failed = with_suffix(current, "failed");
        let _ = fs::remove_file(&failed);
        fs::rename(current, &failed).map_err(|e| UpdateError::io(current, e))?;
        match fs::rename(&prev, current) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::rename(&failed, current);
                Err(UpdateError::io(current, e))
            }
        }
    }
}

/// The advice for a path this user cannot write: name the package manager, if the
/// path looks like one owns it, rather than telling the operator to `sudo` over
/// their package manager's files.
#[must_use]
pub fn package_manager_advice(path: &Path) -> String {
    let text = path.display().to_string();
    for (marker, advice) in [
        ("/Cellar/", "brew upgrade arreo"),
        ("/homebrew/", "brew upgrade arreo"),
        ("/opt/homebrew/", "brew upgrade arreo"),
        ("/.cargo/bin/", "cargo install --force arreo"),
        ("/usr/bin/", "your distribution's package manager"),
        ("/usr/local/bin/", "your distribution's package manager"),
    ] {
        if text.contains(marker) {
            return advice.to_string();
        }
    }
    "reinstall it where this user can write, or run the update as its owner".to_string()
}

/// An exclusive update lock, held for as long as this value lives.
///
/// Dropping it (or the process ending, however it ends) releases the lock,
/// because the kernel owns it — see the module docs for why that beats a pid
/// file.
pub struct UpdateLock {
    inner: crate::lock::ExclusiveLock,
}

impl std::fmt::Debug for UpdateLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateLock")
            .field("path", &self.inner.path())
            .finish_non_exhaustive()
    }
}

impl UpdateLock {
    /// Take the lock beside `current`, or report who holds it.
    pub fn acquire(current: &Path) -> Result<Self, UpdateError> {
        let path = with_suffix(current, "update.lock");
        crate::lock::ExclusiveLock::acquire(&path)
            .map(|inner| Self { inner })
            .map_err(|e| match e {
                crate::lock::LockError::WouldBlock(path) => {
                    UpdateError::Locked(path.display().to_string())
                }
                crate::lock::LockError::Io(path, e) => UpdateError::io(&path, e),
                // `acquire` cannot produce these — they describe a descriptor
                // *received* from another process (T-0038's handoff), which the
                // updater never adopts — but the enum is shared, so they are
                // named rather than wildcarded.
                crate::lock::LockError::NotHeld(path) => {
                    UpdateError::io(&path, std::io::Error::other("the lock is not held"))
                }
                crate::lock::LockError::NotTheLock { path, .. } => UpdateError::io(
                    &path,
                    std::io::Error::other("the descriptor is not the lock"),
                ),
            })
    }
}

impl UpdateLock {
    /// The lock file, for a message that says which one is held.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.inner.path()
    }
}

/// Run `binary --version` and return what it printed, or why it would not run.
///
/// The one child process this module ever spawns, and the only check that proves
/// the swap produced something *runnable* rather than merely present. It asks for
/// `--version`, which reaches no daemon and no PTY — the invariant holds even
/// here.
pub fn verify_runs(binary: &Path) -> Result<String, UpdateError> {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|e| UpdateError::io(binary, e))?;
    if !output.status.success() {
        return Err(UpdateError::NotExecutable(format!(
            "{} exited {} when asked for --version",
            binary.display(),
            output.status
        )));
    }
    let mut text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        text = String::from_utf8_lossy(&output.stderr).trim().to_string();
    }
    Ok(text)
}

/// Small helper so a typed error can carry the underlying io failure in its
/// message without losing the variant that decides the advice.
trait TapIo {
    fn tap_io(self, e: std::io::Error) -> Self;
}

impl TapIo for UpdateError {
    fn tap_io(self, e: std::io::Error) -> Self {
        match self {
            Self::NotWritable { path, advice } => Self::NotWritable {
                path: format!("{path} ({e})"),
                advice,
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory with a fake "installed" binary in it.
    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "arreo-update-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    fn binary(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write");
        set_executable(&path).expect("mode");
        path
    }

    /// A binary installed `0700` stays `0700`: the updater mirrors the install's
    /// own mode instead of widening it on every run.
    #[cfg(unix)]
    #[test]
    fn the_staged_copy_takes_the_installs_mode_not_a_fixed_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("mode");
        let current = binary(&dir, "arreo", "OLD");
        fs::set_permissions(&current, fs::Permissions::from_mode(0o700)).expect("mode");
        let candidate = binary(&dir, "new-arreo", "NEW");

        let staged = stage(&candidate, &current).expect("stage");
        let mode = fs::metadata(&staged).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "an install's own mode is preserved");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The property the whole design exists for: the binary path is never
    /// missing or non-executable, at any step.**
    ///
    /// Checked after each step rather than only at the end, because the failure
    /// this prevents is a crash *between* steps — an install nobody can run to
    /// repair itself.
    #[test]
    fn the_binary_path_is_runnable_after_every_step_of_the_swap() {
        let dir = scratch("steps");
        let current = binary(&dir, "arreo", "OLD");
        let candidate = binary(&dir, "new-arreo", "NEW");

        let staged = stage(&candidate, &current).expect("stage");
        assert!(
            is_runnable(&current),
            "step 1 must not touch the binary path"
        );
        assert!(is_runnable(&staged), "the staged copy must be executable");
        assert_eq!(
            fs::read(&staged).expect("read"),
            b"NEW",
            "staging copies the candidate's bytes"
        );

        swap(&staged, &current).expect("swap");
        assert!(
            is_runnable(&current),
            "after the swap the path is the new binary"
        );
        assert_eq!(fs::read(&current).expect("read"), b"NEW");
        assert_eq!(
            fs::read(prev_path(&current)).expect("read"),
            b"OLD",
            "the previous binary is preserved for rollback"
        );
        assert!(!staged.exists(), "the staged copy was moved, not copied");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A crash *between* the two steps leaves a runnable binary — the exact
    /// scenario the two-rename design would fail.
    #[test]
    fn a_crash_between_the_steps_leaves_a_runnable_binary() {
        let dir = scratch("crash");
        let current = binary(&dir, "arreo", "OLD");
        let candidate = binary(&dir, "new-arreo", "NEW");

        // Crash point 1: after staging, before the link. Nothing has changed.
        let staged = stage(&candidate, &current).expect("stage");
        assert!(is_runnable(&current));
        assert_eq!(fs::read(&current).expect("read"), b"OLD");

        // Crash point 2: after the `.prev` link, before the rename. Still the old
        // binary at the path, and `.prev` is a second name for it.
        let prev = prev_path(&current);
        let _ = fs::remove_file(&prev);
        fs::hard_link(&current, &prev).expect("link");
        assert!(is_runnable(&current));
        assert_eq!(fs::read(&current).expect("read"), b"OLD");

        // Crash point 3: after the rename. The new binary, and the old one still
        // reachable through `.prev` — so a rollback is always possible.
        fs::rename(&staged, &current).expect("rename");
        assert!(is_runnable(&current));
        assert_eq!(fs::read(&current).expect("read"), b"NEW");
        assert_eq!(fs::read(&prev).expect("read"), b"OLD");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rollback_restores_the_previous_binary_and_refuses_without_one() {
        let dir = scratch("rollback");
        let current = binary(&dir, "arreo", "OLD");
        let candidate = binary(&dir, "new-arreo", "NEW");

        // No `.prev` yet: the refusal names the path it looked for.
        let err = rollback(&current).expect_err("nothing to roll back to");
        assert!(matches!(err, UpdateError::NoPrevious(_)), "{err}");

        let staged = stage(&candidate, &current).expect("stage");
        swap(&staged, &current).expect("swap");
        rollback(&current).expect("rollback");
        assert_eq!(
            fs::read(&current).expect("read"),
            b"OLD",
            "rollback puts the previous binary back"
        );
        assert!(
            !prev_path(&current).exists(),
            "and consumes it, so a second rollback is an honest refusal"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A candidate that is not a runnable binary must never reach the binary
    /// path — staging happens first precisely so this can be refused.
    #[test]
    fn a_candidate_that_cannot_run_is_refused_before_it_is_installed() {
        let dir = scratch("not-runnable");
        let current = binary(&dir, "arreo", "OLD");
        let plain = dir.join("not-a-binary");
        std::fs::write(&plain, "just text").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&plain, fs::Permissions::from_mode(0o644)).expect("mode");
        }
        let err = stage(&plain, &current).expect_err("must refuse");
        assert!(matches!(err, UpdateError::NotExecutable(_)), "{err}");
        assert_eq!(
            fs::read(&current).expect("read"),
            b"OLD",
            "a refused update leaves the binary byte-identical"
        );
        assert!(!staged_path(&current).exists(), "and leaves nothing staged");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn identical_separates_equal_files_from_different_ones() {
        let dir = scratch("identical");
        let a = binary(&dir, "a", "SAME");
        let b = binary(&dir, "b", "SAME");
        let c = binary(&dir, "c", "DIFFERENT");
        let d = binary(&dir, "d", "SAMEX");
        assert!(identical(&a, &b).expect("compare"));
        assert!(!identical(&a, &c).expect("compare"));
        assert!(
            !identical(&a, &d).expect("compare"),
            "equal length, different bytes"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The lock refuses a second holder, and — the half a pid file cannot promise
    /// — releases itself when the holder dies.
    #[test]
    fn the_update_lock_is_exclusive_and_cannot_go_stale() {
        let dir = scratch("lock");
        let current = binary(&dir, "arreo", "OLD");

        let held = UpdateLock::acquire(&current).expect("first lock");
        let err = UpdateLock::acquire(&current).expect_err("second must refuse");
        assert!(matches!(err, UpdateError::Locked(_)), "{err}");
        assert!(held.path().exists());

        // Dropping releases it: the next process locks the same file. This is the
        // property that makes "a killed updater leaves a stale lock" impossible —
        // the kernel holds the description, not a number in a file.
        drop(held);
        let again = UpdateLock::acquire(&current).expect("after release");
        drop(again);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_package_manager_install_is_recognised_by_its_path() {
        assert_eq!(
            package_manager_advice(Path::new("/opt/homebrew/Cellar/arreo/0.1.0/bin/arreo")),
            "brew upgrade arreo"
        );
        assert_eq!(
            package_manager_advice(Path::new("/home/dev/.cargo/bin/arreo")),
            "cargo install --force arreo"
        );
        assert!(
            package_manager_advice(Path::new("/home/dev/bin/arreo")).contains("reinstall"),
            "an unknown location gets the generic advice, not a wrong package manager"
        );
    }
}
