//! The **deferred** update (T-0039, ROADMAP §3.13): a verified artifact staged
//! beside the running binary, and the one window in which it may be promoted.
//!
//! One sentence: when an update cannot be cut over live — because the platform
//! cannot replace a running image, or because the live path was refused — the
//! verified artifact is *kept* rather than thrown away, reported until it is
//! promoted, and promoted only in the window that costs nobody their work.
//!
//! ## The rule, and why it is one function
//!
//! **The only window is `live_panes == 0`.** A live pane is a running agent, and
//! no update is worth an agent's work: the machine keeps serving the binary it
//! has until nothing is running behind it. That is the whole policy, so it is
//! [`window_is_open`] and nothing else — a second place that decides "is it safe
//! now?" is a second answer to the question this task exists to have exactly one
//! answer to.
//!
//! The caller must read the pane list **under the daemon's own lock, immediately
//! before the swap** (see `arreo-server`), so that "no panes" cannot be true of a
//! moment that has already passed by the time the swap happens. This module
//! cannot enforce that — it does not hold the lock — which is why the rule is a
//! function of the pane list rather than of the world: the caller's obligation is
//! visible in the signature.
//!
//! ## Why the artifact is kept instead of discarded
//!
//! Before this module, every path that could not complete a cut did the same
//! thing: `let _ = fs::remove_file(&staged)`. An operator who downloaded a
//! release, had it verified against the compiled-in key, and was then refused
//! because a pane was busy lost the download and had to do it again. Staging it
//! as `<current>.next` plus a marker in the state directory turns that into a
//! state the operator can see and act on.
//!
//! ## Why a refusal never retries
//!
//! A stage that cannot be promoted — truncated after verification, or no longer
//! reporting the version it was recorded with — is **discarded along with its
//! marker**, and the refusal says so. Keeping it would mean the same failure on
//! every boot, for ever, which is a loop dressed as persistence. The same is true
//! of a swap that fails (a directory this user cannot write): the cause is
//! fixable, the artifact is not what is wrong, and a boot loop is still the
//! symptom — so the refusal names the cause and asks for `arreo update` again.
//!
//! ## Why the marker outlives the promotion
//!
//! Promotion does **not** clear the marker. The swap is a file operation and the
//! version is a fact about a running process, and only the second one says the
//! update happened: the marker clears when a binary that reports the staged
//! version confirms it (see [`clear_after_confirm`]). A marker cleared by the
//! swap would report success for a machine that is still serving the old code
//! until its next restart — the exact lie this task exists to prevent.
//!
//! ## The state directory is a parameter
//!
//! Every function that touches the marker has an `_in(state_dir)` form and a
//! convenience wrapper that uses [`resume::dir`]. Same reason
//! [`resume::dir_from`] is split out: two tests that both set `ARREO_STATE_DIR`
//! run in parallel threads of one process and race each other, which is a flaky
//! test rather than a test — and a test that writes the *real* state directory
//! is a test that corrupts the machine it runs on.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{identical, is_runnable, resume, swap, verify_runs, UpdateError};

/// The staged artifact's suffix: `<current>.next`, a sibling of the binary for
/// the same reason `.prev` and `.staged` are — the promotion must be one rename,
/// and a rename across filesystems is a copy with a window.
pub const NEXT_SUFFIX: &str = "next";

/// The marker's file name, inside [`resume::dir`].
pub const PENDING_FILE: &str = "update-pending.json";

/// Where a verified artifact waits to be promoted.
#[must_use]
pub fn next_path(current: &Path) -> PathBuf {
    super::with_suffix(current, NEXT_SUFFIX)
}

/// The marker's path inside a given state directory.
///
/// The state directory, not the install directory: this is per-machine state
/// that has to be findable by *another* process — `arreo status` reporting it,
/// the daemon acting on it — and it must not live in a directory an operator
/// might not be able to write when the binary itself is fine.
#[must_use]
pub fn marker_in(state_dir: &Path) -> PathBuf {
    state_dir.join(PENDING_FILE)
}

/// The marker's path, in the state directory this machine uses.
#[must_use]
pub fn marker() -> PathBuf {
    marker_in(&resume::dir())
}

/// What is waiting, and what it will replace.
///
/// Serialized as JSON rather than a line of text because every field is read
/// back by a *different* process than the one that wrote it, and a marker that
/// cannot be parsed is a marker that silently reports nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pending {
    /// The staged artifact, absolute — `<current>.next` when it was written.
    pub staged: String,
    /// The binary it will replace.
    pub current: String,
    /// The version the *running* binary reported when the artifact was staged.
    pub from_version: String,
    /// The version the staged artifact reported (`--version`, at stage time).
    pub version: String,
}

impl Pending {
    /// The operator's line, in the spelling the task fixes:
    /// `update pending v0.2.0 → v0.3.0 (applies at next restart)`.
    #[must_use]
    pub fn line(&self) -> String {
        format!(
            "update pending {} → {} (applies at next restart)",
            self.from_version, self.version
        )
    }

    /// The staged path the marker names.
    #[must_use]
    pub fn staged_path(&self) -> PathBuf {
        PathBuf::from(&self.staged)
    }

    /// The binary the marker names.
    #[must_use]
    pub fn current_path(&self) -> PathBuf {
        PathBuf::from(&self.current)
    }
}

/// The window rule. **The only window is `live_panes == 0`.**
///
/// One function, deliberately: every other answer to "may we promote now?" is
/// a second policy, and the two would eventually disagree at the one moment
/// that matters.
#[must_use]
pub fn window_is_open(live_panes: usize) -> bool {
    live_panes == 0
}

/// The refusal an open-pane machine gets, naming the panes.
///
/// Names, not just a count: "3 panes are running" leaves an operator guessing
/// which agent they would be killing, and the whole point of refusing is that
/// they get to decide.
#[must_use]
pub fn panes_block(live_panes: &[String]) -> String {
    format!(
        "{} live pane{} ({}) — a pane is a running agent, so the update waits for the window \
         where none is running",
        live_panes.len(),
        if live_panes.len() == 1 { "" } else { "s" },
        live_panes.join(", ")
    )
}

/// A promotion that did not happen, with the reason an operator needs.
#[derive(Debug, thiserror::Error)]
pub enum NotNow {
    /// Panes are running, so the window is shut. Retrying is the remedy.
    #[error("{}", panes_block(.panes))]
    PanesLive { panes: Vec<String> },
    /// The stage cannot be promoted, and it has been discarded so that this
    /// cannot become a loop. `arreo update` again is the remedy.
    #[error(
        "{reason} — the staged update was discarded; run `arreo update` again when that is fixed"
    )]
    NotPromotable { reason: String },
}

/// What a promotion attempt did when it did not refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The staged binary is now at the install path; `.prev` holds the old one.
    Promoted {
        /// The version that was replaced.
        from_version: String,
        /// The version now installed.
        version: String,
        /// The old binary, kept for `--rollback`.
        previous: PathBuf,
    },
    /// Nothing was pending.
    NothingPending,
    /// The staged bytes are already the installed bytes, so there was nothing to
    /// do and the marker was cleared. Not a failure: an operator who runs
    /// `--apply-now` twice must not be told the second one broke.
    AlreadyInstalled { version: String },
}

/// Stage a **verified** artifact as `<current>.next`, in this machine's state
/// directory.
pub fn stage_next(
    verified: &Path,
    current: &Path,
    from_version: &str,
    version: &str,
) -> Result<Pending, UpdateError> {
    stage_next_in(&resume::dir(), verified, current, from_version, version)
}

/// Stage a **verified** artifact as `<current>.next` and record it as pending.
///
/// The caller owns verification (T-0036): this function copies bytes and writes
/// a marker, and the reason it is a separate step from `update::stage` is that a
/// deferred artifact has to survive until the next start — `.staged` is the
/// name the live path uses for the copy it is about to swap, and reusing it
/// would have two meanings for one path.
///
/// The marker is written **after** the copy, so a crash between the two leaves a
/// staged file nobody will promote rather than a marker naming a file that is
/// not there.
pub fn stage_next_in(
    state_dir: &Path,
    verified: &Path,
    current: &Path,
    from_version: &str,
    version: &str,
) -> Result<Pending, UpdateError> {
    if !is_runnable(verified) {
        return Err(UpdateError::NotExecutable(verified.display().to_string()));
    }
    let staged = next_path(current);
    let _ = fs::remove_file(&staged);
    let bytes = fs::read(verified).map_err(|e| UpdateError::io(verified, e))?;
    fs::write(&staged, &bytes).map_err(|e| UpdateError::io(&staged, e))?;
    super::set_executable_like(&staged, current)?;

    let pending = Pending {
        staged: staged.display().to_string(),
        current: current.display().to_string(),
        from_version: from_version.to_string(),
        version: version.to_string(),
    };
    write_marker_in(state_dir, &pending)?;
    Ok(pending)
}

/// Write the marker into a given state directory.
pub fn write_marker_in(state_dir: &Path, pending: &Pending) -> Result<(), UpdateError> {
    fs::create_dir_all(state_dir).map_err(|e| UpdateError::io(state_dir, e))?;
    let path = marker_in(state_dir);
    let json = serde_json::to_vec_pretty(pending).map_err(|e| UpdateError::Io {
        path: path.display().to_string(),
        detail: e.to_string(),
    })?;
    fs::write(&path, json).map_err(|e| UpdateError::io(&path, e))
}

/// Read the marker from a given state directory.
///
/// `Ok(None)` means *nothing is pending* and covers the ordinary case (no file).
/// A file that exists and cannot be read or parsed is an `Err` naming it: a
/// marker nobody can interpret must not be reported as "nothing pending", which
/// is the silent-deferral failure the task names.
pub fn read_marker_in(state_dir: &Path) -> Result<Option<Pending>, UpdateError> {
    let path = marker_in(state_dir);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(UpdateError::io(&path, e)),
    };
    let pending: Pending = serde_json::from_slice(&bytes).map_err(|e| UpdateError::Io {
        path: path.display().to_string(),
        detail: format!("the pending-update marker cannot be read: {e}"),
    })?;
    Ok(Some(pending))
}

/// Read this machine's marker.
pub fn read_marker() -> Result<Option<Pending>, UpdateError> {
    read_marker_in(&resume::dir())
}

/// Remove this machine's marker. Absent is not an error.
pub fn clear_marker() -> Result<bool, UpdateError> {
    clear_marker_in(&resume::dir())
}

/// Remove a marker from a given state directory. Absent is not an error.
pub fn clear_marker_in(state_dir: &Path) -> Result<bool, UpdateError> {
    let path = marker_in(state_dir);
    match fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(UpdateError::io(&path, e)),
    }
}

/// Discard the staged artifact and its marker — the refusal path's cleanup.
fn discard_in(state_dir: &Path, pending: &Pending) {
    let _ = fs::remove_file(pending.staged_path());
    let _ = clear_marker_in(state_dir);
}

/// **Promote the staged update, or refuse with the reason.** The one door.
pub fn promote(live_panes: &[String]) -> Result<Outcome, NotNow> {
    promote_in(&resume::dir(), live_panes)
}

/// **Promote the staged update, or refuse with the reason.**
///
/// `live_panes` is the pane list the caller read under the daemon's lock,
/// immediately before this call — see the module docs for why that is the
/// caller's obligation and not something this function can check.
///
/// The order below is the policy: refuse for panes *before* touching the
/// artifact (a busy machine must not lose its stage to an unrelated check),
/// then prove the stage is still promotable, then swap. Every refusal discards
/// the stage, which is what makes "does not retry in a loop" a property of the
/// code rather than a promise in a comment.
pub fn promote_in(state_dir: &Path, live_panes: &[String]) -> Result<Outcome, NotNow> {
    let Some(pending) = read_marker_in(state_dir).map_err(|e| NotNow::NotPromotable {
        reason: e.to_string(),
    })?
    else {
        return Ok(Outcome::NothingPending);
    };

    // The window, and nothing else, decides whether we may proceed.
    if !window_is_open(live_panes.len()) {
        return Err(NotNow::PanesLive {
            panes: live_panes.to_vec(),
        });
    }

    let staged = pending.staged_path();
    let current = pending.current_path();

    // The stage is proven again here, not trusted from the marker: it was
    // verified when it was written, and a file that has since been truncated,
    // replaced or made non-executable must not reach the install path.
    if !is_runnable(&staged) {
        let reason = format!(
            "the staged update at {} is no longer a runnable binary",
            staged.display()
        );
        discard_in(state_dir, &pending);
        return Err(NotNow::NotPromotable { reason });
    }
    match verify_runs(&staged) {
        Ok(reported) if reported.trim() == pending.version.trim() => {}
        Ok(reported) => {
            let reason = format!(
                "the staged update reports {reported:?} where the marker recorded {:?}",
                pending.version
            );
            discard_in(state_dir, &pending);
            return Err(NotNow::NotPromotable { reason });
        }
        Err(e) => {
            let reason = format!("the staged update will not run: {e}");
            discard_in(state_dir, &pending);
            return Err(NotNow::NotPromotable { reason });
        }
    }

    // Already the installed bytes: nothing to swap, and the marker is stale.
    if identical(&staged, &current).unwrap_or(false) {
        let _ = clear_marker_in(state_dir);
        return Ok(Outcome::AlreadyInstalled {
            version: pending.version,
        });
    }

    if let Err(e) = swap(&staged, &current) {
        let reason = format!("installing {} failed: {e}", current.display());
        discard_in(state_dir, &pending);
        return Err(NotNow::NotPromotable { reason });
    }

    Ok(Outcome::Promoted {
        from_version: pending.from_version,
        version: pending.version,
        previous: super::prev_path(&current),
    })
}

/// Clear the marker once a binary reporting `confirmed` has taken over.
pub fn clear_after_confirm(confirmed: &str) -> Result<bool, UpdateError> {
    clear_after_confirm_in(&resume::dir(), confirmed)
}

/// Clear the marker once a binary reporting `confirmed` has taken over.
///
/// **Only after the version confirms.** The swap moves a file; the version is
/// what a *running* process reports, and until the running one reports the new
/// version the update has not happened — so this returns `false` and leaves the
/// marker alone when `confirmed` is anything else, and `arreo status` keeps
/// saying the update is pending.
pub fn clear_after_confirm_in(state_dir: &Path, confirmed: &str) -> Result<bool, UpdateError> {
    let Some(pending) = read_marker_in(state_dir)? else {
        return Ok(false);
    };
    if pending.version.trim() != confirmed.trim() {
        return Ok(false);
    }
    clear_marker_in(state_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory per test, under the repo's test scratch (never
    /// `/tmp`: it is a tmpfs here and a large binary in it is a real problem).
    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/T-0039")
            .join(name);
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A plain file standing in for a binary. Enough for every test that does
    /// not *execute* the artifact.
    fn binary_file(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        path
    }

    /// A "binary" that reports a version: a shell script is enough, and it keeps
    /// the test about the state machine rather than about a real build.
    ///
    /// Unix-only: the tests that need it prove the promotion by *running* the
    /// staged artifact, which a shell script cannot do on Windows.
    #[cfg(unix)]
    fn fake_binary(path: &Path, version: &str) {
        use std::os::unix::fs::PermissionsExt;
        fs::write(path, format!("#!/bin/sh\necho '{version}'\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The rule is one line and one meaning.
    #[test]
    fn the_only_window_is_zero_live_panes() {
        assert!(window_is_open(0));
        assert!(!window_is_open(1));
        assert!(!window_is_open(9));
    }

    /// The refusal names the panes, because the operator is the one who decides
    /// whether to stop them.
    #[test]
    fn the_refusal_names_the_panes() {
        let one = panes_block(&["build".to_string()]);
        assert!(one.starts_with("1 live pane ("), "{one}");
        assert!(one.contains("build"), "{one}");
        let two = panes_block(&["build".to_string(), "review".to_string()]);
        assert!(two.starts_with("2 live panes ("), "{two}");
        assert!(two.contains("build, review"), "{two}");
    }

    /// A staged update promotes, keeps the old binary, and does **not** clear
    /// its marker — the version confirmation does that, and nothing else.
    #[cfg(unix)]
    #[test]
    fn a_staged_update_promotes_and_waits_for_the_version_to_confirm() {
        let dir = scratch("promote");
        let state = dir.join("state");
        let current = dir.join("arreo-server");
        fake_binary(&current, "arreo-server 0.2.0");
        let candidate = dir.join("downloaded");
        fake_binary(&candidate, "arreo-server 0.3.0");

        let pending = stage_next_in(
            &state,
            &candidate,
            &current,
            "arreo-server 0.2.0",
            "arreo-server 0.3.0",
        )
        .expect("stage");
        assert!(next_path(&current).is_file());
        assert_eq!(
            pending.line(),
            "update pending arreo-server 0.2.0 → arreo-server 0.3.0 (applies at next restart)"
        );

        // The window is shut while a pane runs: refuse, and *keep* the stage.
        let err = promote_in(&state, &["build".to_string()]).expect_err("a live pane refuses");
        assert!(matches!(err, NotNow::PanesLive { .. }), "{err:?}");
        assert!(err.to_string().contains("build"), "{err}");
        assert!(
            next_path(&current).is_file(),
            "the stage survives a refusal"
        );
        assert!(read_marker_in(&state).unwrap().is_some());

        let outcome = promote_in(&state, &[]).expect("promote at zero panes");
        match outcome {
            Outcome::Promoted {
                ref from_version,
                ref version,
                ..
            } => {
                assert_eq!(from_version, "arreo-server 0.2.0");
                assert_eq!(version, "arreo-server 0.3.0");
            }
            other => panic!("want Promoted, got {other:?}"),
        }
        assert_eq!(
            verify_runs(&current).unwrap(),
            "arreo-server 0.3.0",
            "the install path serves the new version"
        );
        assert!(super::super::prev_path(&current).is_file(), ".prev kept");
        assert!(
            !next_path(&current).exists(),
            "the staged name is gone — it was renamed into place"
        );
        // Still pending: the *swap* is not the confirmation.
        assert!(read_marker_in(&state).unwrap().is_some());
        assert!(!clear_after_confirm_in(&state, "arreo-server 0.2.0").unwrap());
        assert!(read_marker_in(&state).unwrap().is_some());
        assert!(clear_after_confirm_in(&state, "arreo-server 0.3.0").unwrap());
        assert!(read_marker_in(&state).unwrap().is_none());
    }

    /// A stage that cannot be promoted is discarded, so the same failure cannot
    /// repeat on every start. Both ways of being unpromotable are checked: a
    /// file that is no longer runnable, and a version that disagrees with what
    /// was recorded.
    #[cfg(unix)]
    #[test]
    fn an_unpromotable_stage_is_refused_and_discarded_never_retried() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("unpromotable");
        let state = dir.join("state");
        let current = dir.join("arreo-server");
        fake_binary(&current, "arreo-server 0.2.0");

        // (a) truncated after verification: it is no longer runnable at all.
        let candidate = dir.join("downloaded");
        fake_binary(&candidate, "arreo-server 0.3.0");
        stage_next_in(
            &state,
            &candidate,
            &current,
            "arreo-server 0.2.0",
            "arreo-server 0.3.0",
        )
        .unwrap();
        let staged = next_path(&current);
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o644)).unwrap();
        let err = promote_in(&state, &[]).expect_err("not runnable");
        assert!(matches!(err, NotNow::NotPromotable { .. }), "{err:?}");
        assert!(!staged.exists(), "the bad stage is discarded");
        assert!(
            read_marker_in(&state).unwrap().is_none(),
            "and so is its marker — no retry loop"
        );

        // (b) a version that disagrees with the marker.
        let candidate = dir.join("downloaded");
        fake_binary(&candidate, "arreo-server 9.9.9");
        stage_next_in(
            &state,
            &candidate,
            &current,
            "arreo-server 0.2.0",
            "arreo-server 0.3.0",
        )
        .unwrap();
        let err = promote_in(&state, &[]).expect_err("version mismatch");
        assert!(
            err.to_string().contains("9.9.9") && err.to_string().contains("0.3.0"),
            "{err}"
        );
        assert!(!next_path(&current).exists());
        assert!(read_marker_in(&state).unwrap().is_none());
    }

    /// Promoting when the staged bytes are already installed is not a failure:
    /// it clears the stale marker and says so.
    #[cfg(unix)]
    #[test]
    fn an_already_installed_stage_is_reported_not_refused() {
        let dir = scratch("already");
        let state = dir.join("state");
        let current = dir.join("arreo-server");
        fake_binary(&current, "arreo-server 0.3.0");
        let candidate = dir.join("downloaded");
        fake_binary(&candidate, "arreo-server 0.3.0");
        stage_next_in(
            &state,
            &candidate,
            &current,
            "arreo-server 0.2.0",
            "arreo-server 0.3.0",
        )
        .unwrap();

        let outcome = promote_in(&state, &[]).expect("already installed is fine");
        assert!(
            matches!(outcome, Outcome::AlreadyInstalled { .. }),
            "{outcome:?}"
        );
        assert!(read_marker_in(&state).unwrap().is_none());
        assert_eq!(verify_runs(&current).unwrap(), "arreo-server 0.3.0");
    }

    /// Nothing pending is a normal answer, not an error.
    #[test]
    fn nothing_pending_is_not_a_failure() {
        let dir = scratch("nothing");
        let state = dir.join("state");
        let _current = binary_file(&dir, "arreo-server", "old bytes");
        assert!(read_marker_in(&state).unwrap().is_none());
        assert_eq!(promote_in(&state, &[]).unwrap(), Outcome::NothingPending);
    }

    /// A marker that exists but cannot be read is an error naming it, never
    /// "nothing pending" — the silent deferral the task forbids.
    #[test]
    fn a_corrupt_marker_is_loud() {
        let dir = scratch("corrupt");
        let state = dir.join("state");
        fs::create_dir_all(&state).unwrap();
        fs::write(marker_in(&state), b"{ not json").unwrap();
        let err = read_marker_in(&state).expect_err("loud");
        assert!(err.to_string().contains("update-pending.json"), "{err}");
    }
}
