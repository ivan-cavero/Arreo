//! An exclusive lock the *kernel* holds, for "only one of these may run".
//!
//! Two places need this and nothing else: the updater (one updater at a time,
//! T-0070) and the daemon (one daemon per socket, T-0071). Both got here the same
//! way — a race that a liveness probe cannot close:
//!
//! - The updater: two `arreo update` runs would both stage and both rename.
//! - The daemon: `is_live()` connects to the socket, and if that fails the daemon
//!   `remove_file`s the path and binds. Two daemons starting together both see a
//!   dead socket, and the second unlinks the first's listener and binds its own —
//!   so the machine ends up with two daemons and clients talking to whichever won.
//!   (Measured: 1 in 12 rounds of eight simultaneous starts produced two live
//!   daemons. The window is microseconds wide and entirely real.)
//!
//! ## Why an OS lock and not a pid file
//!
//! The lock lives in the **open file description**, and the kernel releases it
//! when that description goes away — on clean exit, on a panic, on `SIGKILL`. So:
//!
//! - A stale lock **cannot** arise, so there is no liveness probe and no timeout
//!   to guess with. A pid file answers "is the holder alive?" by guessing: the pid
//!   may have been reused by an unrelated process, or belong to an updater in
//!   another namespace. Every pid-file implementation ends up with a timeout,
//!   which is a guess about time rather than a fact about a process.
//! - `flock`-style locks are advisory, and that is the right strength here: the
//!   point is that *our* processes agree, not that the filesystem enforces it
//!   against strangers.
//!
//! The lock file itself stays on disk after release. That is deliberate: it is the
//! *open description* that carries the lock, so a leftover zero-byte file is not a
//! stale lock — the next process opens and locks it. Removing it on release would
//! race a process that has already opened it and is about to lock.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

/// Why a lock could not be taken.
#[derive(Debug)]
pub enum LockError {
    /// Someone else holds it.
    WouldBlock(PathBuf),
    /// The lock file itself could not be opened.
    Io(PathBuf, std::io::Error),
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WouldBlock(path) => write!(f, "already locked ({})", path.display()),
            Self::Io(path, e) => write!(f, "{}: {e}", path.display()),
        }
    }
}

impl std::error::Error for LockError {}

/// An exclusive lock, held for as long as this value lives.
///
/// The lock is taken with `try_lock`, never a blocking acquire: a caller that
/// cannot proceed needs to *say so*, and waiting would hide which of two
/// processes is the operator's.
pub struct ExclusiveLock {
    file: File,
    path: PathBuf,
}

impl std::fmt::Debug for ExclusiveLock {
    /// Prints the path, never the handle: a debug line is not a place to leak an
    /// open file description (and the handle is not interesting).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExclusiveLock")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl ExclusiveLock {
    /// Take the lock at `path`, creating the file if it does not exist.
    pub fn acquire(path: &Path) -> Result<Self, LockError> {
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| LockError::Io(path.to_path_buf(), e))?;
        match file.try_lock() {
            Ok(()) => Ok(Self {
                file,
                path: path.to_path_buf(),
            }),
            Err(fs::TryLockError::WouldBlock) => Err(LockError::WouldBlock(path.to_path_buf())),
            Err(fs::TryLockError::Error(e)) => Err(LockError::Io(path.to_path_buf(), e)),
        }
    }

    /// The lock file, for a message that says which one is held.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ExclusiveLock {
    fn drop(&mut self) {
        // Closing the file would release the lock anyway (the kernel releases it
        // with the description); unlocking explicitly says so, and keeps the file
        // on disk for the next holder — see the module docs.
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "arreo-lock-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    /// The whole reason this type exists: a second holder is refused while the
    /// first lives, and accepted the moment it does not.
    #[test]
    fn a_second_lock_on_the_same_path_is_refused_until_the_first_goes() {
        let dir = scratch("exclusive");
        let path = dir.join("thing.lock");

        let held = ExclusiveLock::acquire(&path).expect("first");
        let refused = ExclusiveLock::acquire(&path).expect_err("second must be refused");
        assert!(
            matches!(refused, LockError::WouldBlock(_)),
            "expected WouldBlock, got {refused}"
        );

        drop(held);
        let again = ExclusiveLock::acquire(&path).expect("after release");
        assert_eq!(again.path(), path);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The lock file stays after release — the *description* carries the lock, so
    /// a leftover file must not be mistaken for a stale one. Asserted because the
    /// tempting "clean up after yourself" change would introduce exactly the race
    /// the module docs describe.
    #[test]
    fn the_lock_file_outlives_the_lock_and_is_reusable() {
        let dir = scratch("lingering");
        let path = dir.join("thing.lock");
        drop(ExclusiveLock::acquire(&path).expect("first"));
        assert!(path.exists(), "the lock file is not removed on release");
        assert!(ExclusiveLock::acquire(&path).is_ok(), "and is re-lockable");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Locks are per-path: holding one says nothing about another.
    #[test]
    fn locks_on_different_paths_do_not_contend() {
        let dir = scratch("independent");
        let a = ExclusiveLock::acquire(&dir.join("a.lock")).expect("a");
        let b = ExclusiveLock::acquire(&dir.join("b.lock")).expect("b");
        assert_ne!(a.path(), b.path());
        let _ = fs::remove_dir_all(&dir);
    }
}
