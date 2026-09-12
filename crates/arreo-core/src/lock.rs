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
    /// The lock at `path` is not held by anyone, so a descriptor claiming to be
    /// it cannot be that lock.
    NotHeld(PathBuf),
    /// The descriptor offered as the lock at `path` is not on that inode — or
    /// is a description of it that does not hold the lock.
    NotTheLock {
        path: PathBuf,
        got_dev: u64,
        got_ino: u64,
        want_dev: u64,
        want_ino: u64,
    },
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WouldBlock(path) => write!(f, "already locked ({})", path.display()),
            Self::Io(path, e) => write!(f, "{}: {e}", path.display()),
            Self::NotHeld(path) => write!(
                f,
                "{}: the lock is not held, so no descriptor can be it",
                path.display()
            ),
            Self::NotTheLock {
                path,
                got_dev,
                got_ino,
                want_dev,
                want_ino,
            } => write!(
                f,
                "the received descriptor is device {got_dev} inode {got_ino}, but the lock {} \
                 is device {want_dev} inode {want_ino}",
                path.display()
            ),
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
    /// The lock came from another process's open file description (see
    /// [`ExclusiveLock::inherited_checked`]) rather than from [`acquire`].
    /// `Drop` must not unlock one of these: `flock(LOCK_UN)` on a shared
    /// description releases the lock for every holder, not just this one.
    inherited: bool,
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
                inherited: false,
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

    /// Borrow the lock's descriptor (for `SCM_RIGHTS` sends, which dup it into
    /// the peer — the borrow stays here, held for the daemon's life).
    ///
    /// Unix only, like everything descriptor-passing: a Windows daemon takes
    /// §3.13's deferred-update path instead of a live handoff (T-0039), so there
    /// is no descriptor to lend. Gated rather than stubbed — a function that
    /// cannot exist on a platform should not compile there, or the next person
    /// reads it as available.
    #[cfg(unix)]
    #[must_use]
    pub fn fd(&self) -> std::os::unix::io::BorrowedFd<'_> {
        use std::os::unix::io::AsFd;
        self.file.as_fd()
    }

    /// Adopt the lock another process already holds, from a descriptor that
    /// arrived over `SCM_RIGHTS` (T-0038 stage 1: the daemon handoff).
    ///
    /// The descriptor is a `dup` of the outgoing daemon's lock file, so it
    /// shares the same **open file description** — and the lock lives in that
    /// description, which is why no lock is *taken* here: the lock is already
    /// ours by inheritance, and `try_lock` below is a check, not an acquire.
    ///
    /// ## Why the checks are what they are
    ///
    /// "A descriptor arrived in the lock slot" is evidence of nothing, and the
    /// path reading as held (`is_held`) is evidence only that *someone* holds
    /// the lock — a hostile sender can satisfy it while handing over a
    /// descriptor that is not the lock at all (a `/etc/hostname` descriptor
    /// made the daemon serve holding nothing). So the descriptor itself is
    /// judged, in three steps:
    ///
    /// 1. It is on the same inode as `path` (device + inode). Any other file is
    ///    refused, naming what arrived.
    /// 2. The lock at `path` is held by someone: a fresh open of the path meets
    ///    a held lock (`WouldBlock`). Nobody holding it is a refusal, not a
    ///    serve.
    /// 3. The descriptor is *that* holder. `flock` on an already-locked
    ///    description with the same type is a no-op success, so `try_lock`
    ///    here succeeds for the shared description and would report
    ///    `WouldBlock` for an independent descriptor of the same inode while
    ///    someone else holds it — the asymmetry is what makes this positive
    ///    evidence. (`try_lock` succeeding while nobody holds the lock cannot
    ///    reach this line: step 2 refused first.)
    ///
    /// A refusal never leaves a lock held by this process: the descriptor is
    /// closed on the way out, and nothing took a lock step 3 did not already
    /// prove was held by the shared description.
    ///
    /// The `Drop` of the value this returns **closes the descriptor and never
    /// unlocks** (see the `inherited` flag): with a shared open file
    /// description, `flock(LOCK_UN)` releases the lock for *every* holder, so
    /// unlocking on drop would hand the socket to a third daemon the moment
    /// this value went away. Closing releases nothing — the kernel's own rule
    /// (the lock ends when the last holder's descriptor closes) is what keeps
    /// the socket ours until the process ends, the same rule the acquired lock
    /// relies on.
    #[cfg(unix)]
    pub fn inherited_checked(
        fd: std::os::unix::io::OwnedFd,
        path: &Path,
    ) -> Result<Self, LockError> {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::io::{FromRawFd, IntoRawFd};
        // Soundness: `fd` is owned, so this process holds the only reference to
        // this descriptor number; `into_raw_fd` transfers that ownership to the
        // `File` without duplicating or closing anything, and the `File` takes
        // over closing it exactly once.
        let raw = std::os::unix::io::OwnedFd::into_raw_fd(fd);
        // SAFETY: `raw` came from an owned descriptor this line just consumed,
        // so it is valid, open, and uniquely owned — the three conditions
        // `from_raw_fd` requires.
        let file = unsafe { File::from_raw_fd(raw) };

        let (got_dev, got_ino) = match file.metadata() {
            Ok(meta) => (meta.dev(), meta.ino()),
            Err(e) => return Err(LockError::Io(path.to_path_buf(), e)),
        };
        // Not `is_held`'s `create(true)`: the lock file must already exist for
        // the descriptor to be its lock, and creating it here would hide a
        // missing lock behind a fresh empty file.
        let (want_dev, want_ino) = match fs::metadata(path) {
            Ok(meta) => (meta.dev(), meta.ino()),
            Err(e) => return Err(LockError::Io(path.to_path_buf(), e)),
        };
        if (got_dev, got_ino) != (want_dev, want_ino) {
            return Err(LockError::NotTheLock {
                path: path.to_path_buf(),
                got_dev,
                got_ino,
                want_dev,
                want_ino,
            });
        }
        if !Self::is_held(path) {
            return Err(LockError::NotHeld(path.to_path_buf()));
        }
        match file.try_lock() {
            Ok(()) => Ok(Self {
                file,
                path: path.to_path_buf(),
                inherited: true,
            }),
            Err(fs::TryLockError::WouldBlock) => Err(LockError::NotTheLock {
                path: path.to_path_buf(),
                got_dev,
                got_ino,
                want_dev,
                want_ino,
            }),
            Err(fs::TryLockError::Error(e)) => Err(LockError::Io(path.to_path_buf(), e)),
        }
    }

    /// Is `path`'s lock currently held by someone (this process or another)?
    ///
    /// Opens the path afresh and tries the lock: `true` means a `try_lock`
    /// would block, i.e. a holder exists. The probe descriptor is closed on
    /// return and never locked, so calling this changes nothing — it is the
    /// read-only half of `acquire`, for the incoming daemon's inheritance
    /// check (proving the descriptor it received really carried the lock).
    #[must_use]
    pub fn is_held(path: &Path) -> bool {
        let Ok(file) = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
        else {
            return false;
        };
        matches!(file.try_lock(), Err(fs::TryLockError::WouldBlock))
    }
}

impl Drop for ExclusiveLock {
    fn drop(&mut self) {
        // An inherited lock's descriptor shares the outgoing daemon's open file
        // description: `unlock` there would release the lock for *every*
        // holder, not just this process — including a daemon still serving
        // after an aborted handoff. Closing the descriptor is all that is
        // correct, and `File`'s own `Drop` does that.
        if self.inherited {
            return;
        }
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

    /// The review's shape: a *regular file* in the lock slot. The descriptor is
    /// not the lock, and `is_held` on the path would have said "held" — which
    /// is why the descriptor itself is judged.
    #[test]
    #[cfg(unix)]
    fn a_descriptor_that_is_not_the_lock_is_refused() {
        use std::os::unix::io::AsFd;
        let dir = scratch("not-the-lock");
        let path = dir.join("thing.lock");
        let held = ExclusiveLock::acquire(&path).expect("first");
        let other = File::create(dir.join("hostname")).expect("other file");
        let received = other.as_fd().try_clone_to_owned().expect("dup");
        let refused = ExclusiveLock::inherited_checked(received, &path)
            .expect_err("a regular file is not the lock");
        assert!(
            matches!(refused, LockError::NotTheLock { .. }),
            "expected NotTheLock, got {refused}"
        );
        drop(held);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A descriptor *of the lock file* that does not hold the lock is refused:
    /// an independent description of the right inode meets the holder's lock.
    #[test]
    #[cfg(unix)]
    fn an_unheld_description_of_the_lock_is_refused() {
        use std::os::unix::io::AsFd;
        let dir = scratch("unheld");
        let path = dir.join("thing.lock");
        let held = ExclusiveLock::acquire(&path).expect("first");
        let fresh = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .expect("independent open");
        let refused = ExclusiveLock::inherited_checked(
            fresh.as_fd().try_clone_to_owned().expect("dup"),
            &path,
        )
        .expect_err("an independent description is not the held lock");
        assert!(
            matches!(refused, LockError::NotTheLock { .. }),
            "expected NotTheLock, got {refused}"
        );
        drop(held);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Nobody holds it: a descriptor claiming to be the lock is refused rather
    /// than adopted (adopting would serve a socket while holding nothing).
    #[test]
    #[cfg(unix)]
    fn a_descriptor_for_an_unheld_lock_is_refused() {
        use std::os::unix::io::AsFd;
        let dir = scratch("unheld-path");
        let path = dir.join("thing.lock");
        // Create the file without locking it.
        let idle = File::create(&path).expect("file");
        let refused = ExclusiveLock::inherited_checked(
            idle.as_fd().try_clone_to_owned().expect("dup"),
            &path,
        )
        .expect_err("nobody holds this lock");
        assert!(
            matches!(refused, LockError::NotHeld(_)),
            "expected NotHeld, got {refused}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// The happy path: a descriptor on the holder's own open file description
    /// is adopted, and it *is* the lock afterwards.
    #[test]
    #[cfg(unix)]
    fn a_shared_descriptor_of_the_held_lock_is_adopted() {
        let dir = scratch("shared");
        let path = dir.join("thing.lock");
        let held = ExclusiveLock::acquire(&path).expect("first");
        let dup = held.fd().try_clone_to_owned().expect("dup");
        let adopted = ExclusiveLock::inherited_checked(dup, &path).expect("the lock itself");
        assert_eq!(adopted.path(), path);
        assert!(
            ExclusiveLock::is_held(&path),
            "the adopted lock still holds the path"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Dropping an inherited lock must **not** unlock it: the descriptor shares
    /// the outgoing daemon's open file description, so `flock(LOCK_UN)` there
    /// would release the lock for every holder — including a daemon that is
    /// still serving because the handoff aborted.
    #[test]
    #[cfg(unix)]
    fn dropping_an_inherited_lock_does_not_release_it() {
        let dir = scratch("inherited-drop");
        let path = dir.join("thing.lock");
        let held = ExclusiveLock::acquire(&path).expect("first");
        let dup = held.fd().try_clone_to_owned().expect("dup");
        let adopted = ExclusiveLock::inherited_checked(dup, &path).expect("the lock itself");
        drop(adopted);
        assert!(
            ExclusiveLock::is_held(&path),
            "the holder's lock survives an inherited value being dropped"
        );
        drop(held);
        assert!(
            !ExclusiveLock::is_held(&path),
            "and is released when the last holder goes"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
