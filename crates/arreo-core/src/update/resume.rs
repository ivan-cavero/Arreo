//! The resume token: where a client was, so a restart is a resume (T-0070).
//!
//! One sentence: a small file naming the machine and pane a client was working
//! with, written **before** a swap and read after the re-exec, so the client comes
//! back to the same place instead of being a stranger to its own session.
//!
//! ## Why a file and not memory
//!
//! The whole point of re-exec is that the process is *gone*: `exec(2)` replaces
//! the image, and on Windows a restarted process is a new one. Anything the new
//! image needs it has to read from disk. That is why the token is written first
//! and read second — and why a token that cannot be written is a reason to refuse
//! the update rather than to lose the operator's place.
//!
//! ## Why it is one mechanism
//!
//! A phone that moves between networks and a client that swaps its binary are the
//! same problem — "I was attached to this pane; put me back" — so they share this
//! shape rather than growing a second one per client.
//!
//! ## What it deliberately does not hold
//!
//! No key material, no credentials, no scrollback. A target (a socket path or a
//! machine name), an optional pane id, and how far the client had read. It is
//! state, not a secret, and it lives in the state directory rather than beside the
//! identity for exactly that reason.

use super::UpdateError;
use std::path::PathBuf;

/// Where a client was, in enough detail to go back.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Resume {
    /// The daemon to reach: a socket path, or a machine name with `--machine`.
    pub target: String,
    /// The pane the client was attached to, when it was attached to one.
    #[serde(default)]
    pub pane: Option<String>,
    /// How many lines of that pane the client had already read, so a resume
    /// continues rather than repeats.
    #[serde(default)]
    pub from_line: usize,
}

impl Resume {
    #[must_use]
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
            pane: None,
            from_line: 0,
        }
    }

    #[must_use]
    pub fn pane(mut self, pane: impl Into<String>) -> Self {
        self.pane = Some(pane.into());
        self
    }

    #[must_use]
    pub fn from_line(mut self, line: usize) -> Self {
        self.from_line = line;
        self
    }
}

/// The state directory: `$XDG_STATE_HOME/arreo`, else `~/.local/state/arreo`.
///
/// XDG *state*, not data or config: this is where a program keeps what it needs
/// to pick up where it left off, which is precisely what this is. (`identity_dir`
/// uses `XDG_DATA_HOME` for keys, which are data; conflating the two would put a
/// resume token in the same directory as a private key.)
///
/// `$ARREO_STATE_DIR` overrides it, so a test — or an operator with an unusual
/// layout — can point it somewhere else without touching `HOME`.
#[must_use]
pub fn dir() -> PathBuf {
    dir_from(
        std::env::var_os("ARREO_STATE_DIR"),
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("HOME"),
    )
}

/// The resolution itself, as a pure function of the three variables.
///
/// Split out so the *rule* is testable without a test mutating process-wide
/// environment: two tests that both set `ARREO_STATE_DIR` run in parallel threads
/// of one process and race each other, which is a flaky test rather than a test.
#[must_use]
pub fn dir_from(
    arreo_state_dir: Option<std::ffi::OsString>,
    xdg_state_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> PathBuf {
    if let Some(custom) = arreo_state_dir {
        return PathBuf::from(custom);
    }
    if let Some(state) = xdg_state_home {
        return PathBuf::from(state).join("arreo");
    }
    if let Some(home) = home {
        return PathBuf::from(home)
            .join(".local")
            .join("state")
            .join("arreo");
    }
    PathBuf::from(".arreo-state")
}

/// The token's path.
#[must_use]
pub fn path() -> PathBuf {
    dir().join("resume.json")
}

/// Write the token, creating the directory if it is missing.
///
/// Called **before** the swap: a client that has swapped but cannot say where it
/// was has lost the operator's place, and no later step can recover it.
pub fn save(resume: &Resume) -> Result<(), UpdateError> {
    save_to(&path(), resume)
}

/// Write a token to a named path. The one implementation; [`save`] is the default
/// location.
pub fn save_to(path: &std::path::Path, resume: &Resume) -> Result<(), UpdateError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| UpdateError::Io {
            path: parent.display().to_string(),
            detail: e.to_string(),
        })?;
    }
    let text = serde_json::to_string_pretty(resume).map_err(|e| UpdateError::Io {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    std::fs::write(path, text).map_err(|e| UpdateError::Io {
        path: path.display().to_string(),
        detail: e.to_string(),
    })
}

/// Read the token, or `None` when there is none or it is unreadable.
///
/// **A malformed token is `None`, not an error.** The token is a convenience
/// written by a process that may have died mid-write; refusing to start because
/// of it would turn a lost convenience into a broken client. The caller's
/// behaviour without one is well-defined: it starts as if it had never run.
#[must_use]
pub fn load() -> Option<Resume> {
    load_from(&path())
}

/// Read a token from a named path.
#[must_use]
pub fn load_from(path: &std::path::Path) -> Option<Resume> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Remove the token, for a client that has finished deliberately.
pub fn clear() {
    let _ = std::fs::remove_file(path());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "arreo-resume-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    /// The token round-trips, including the pane and the read position — the two
    /// fields that make a resume a resume rather than a restart.
    ///
    /// Against an explicit path, never the process-wide environment: two tests
    /// that both set `ARREO_STATE_DIR` would race each other in one process.
    #[test]
    fn a_token_round_trips_through_the_state_directory() {
        let dir = scratch("round-trip");
        let path = dir.join("resume.json");
        assert!(load_from(&path).is_none(), "no token yet");

        let resume = Resume::new("/run/user/1000/arreo.sock")
            .pane("pane-1")
            .from_line(42);
        save_to(&path, &resume).expect("save");
        assert_eq!(load_from(&path), Some(resume));
        std::fs::remove_file(&path).ok();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A token written by a dying process is not a reason to refuse to start: a
    /// client with no token starts fresh, which is what it did the first time.
    #[test]
    fn an_unreadable_token_reads_as_no_token() {
        let dir = scratch("broken");
        let path = dir.join("resume.json");
        std::fs::write(&path, "{ this is not json").expect("write");
        assert!(load_from(&path).is_none());
        assert!(load_from(&dir.join("absent.json")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The location rule, and its precedence, without touching the environment.
    #[test]
    fn the_state_directory_follows_the_xdg_precedence() {
        let s = |text: &str| Some(std::ffi::OsString::from(text));
        assert_eq!(
            dir_from(s("/custom"), s("/xdg"), s("/home/u")),
            PathBuf::from("/custom"),
            "the product's own override wins, so a test or an operator can move it"
        );
        assert_eq!(
            dir_from(None, s("/xdg"), s("/home/u")),
            PathBuf::from("/xdg/arreo"),
            "then XDG_STATE_HOME"
        );
        assert_eq!(
            dir_from(None, None, s("/home/u")),
            PathBuf::from("/home/u/.local/state/arreo"),
            "then the XDG default under HOME — state, not data: this is not a secret"
        );
        assert_eq!(
            dir_from(None, None, None),
            PathBuf::from(".arreo-state"),
            "and a last resort, so the client still has somewhere to remember"
        );
    }
}
