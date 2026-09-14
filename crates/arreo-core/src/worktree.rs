//! Worktree-per-task (T-0091, ROADMAP §6 Phase 4): a pane that owns its own
//! `git worktree`, so two agents on one machine cannot touch each other's files.
//!
//! One sentence: `arreo spawn --worktree` gives the pane its own checkout on its
//! own branch, and this module is the whole of the git plumbing — one place that
//! knows how a worktree is named, made, inspected and removed.
//!
//! ## Why worktrees and not copies
//!
//! `git worktree add` shares the object store and the refs, so a second checkout
//! costs a working directory rather than a second clone, and the branch is a real
//! branch that can be merged, pushed and reviewed. A copy (or a stash dance) is
//! what this replaces, and it replaces it for one reason: two agents editing one
//! checkout silently overwrite each other's work, which is the first collision
//! every multi-agent harness hits.
//!
//! ## The two rules that are not negotiable
//!
//! - **A dirty worktree is never deleted by accident.** Removing a worktree
//!   discards uncommitted work, and uncommitted work is the only unrecoverable
//!   thing in this whole feature. [`status`] is what decides, and the refusal
//!   names the files so the operator can look before deciding.
//! - **The pane id is a directory name, and is validated as one.** A pane id
//!   arrives from a client; `root.join(id)` with an id of `../../etc` escapes the
//!   worktree root entirely. [`is_safe_pane_id`] is the gate, and it is the same
//!   shape as the machine-name rules of T-0043: a conservative charset, a length
//!   bound, and an explicit refusal for the two names that mean something to the
//!   filesystem.
//!
//! ## Shelling out to git
//!
//! `git` is invoked as a subprocess rather than reimplemented: worktree
//! bookkeeping lives in `$GIT_DIR/worktrees/` and the `.git` file of the
//! checkout, and a partial reimplementation would be a second, subtly different
//! answer to "is this a worktree of that repository?" — the class of bug this
//! repository keeps finding. The binary is a hard requirement of the feature, and
//! its absence is reported as such ([`WorktreeError::NoGit`]) rather than
//! surfacing as an empty list.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The longest pane id this module will turn into a directory name.
///
/// A bound rather than a guess: filesystem components are typically limited to
/// 255 bytes and this keeps the whole worktree path comfortably inside `PATH_MAX`
/// once the root and the repository name are added.
pub const MAX_PANE_ID: usize = 64;

/// The branch prefix every Arreo worktree uses, so `git branch` reads as a list
/// of what the fleet is doing.
pub const BRANCH_PREFIX: &str = "arreo/";

/// What can go wrong, each with the path or the reason it happened to.
#[derive(Debug, thiserror::Error)]
pub enum WorktreeError {
    #[error(
        "{path} is not inside a git repository (no `git rev-parse --show-toplevel`): {detail}"
    )]
    NotARepo { path: String, detail: String },
    #[error("`git` is not available: {0}")]
    NoGit(String),
    #[error("{pane:?} cannot name a worktree directory: {reason}")]
    BadPaneId { pane: String, reason: String },
    #[error("{path} exists and is not a worktree of {repo}: refusing to touch it")]
    NotOurWorktree { path: String, repo: String },
    /// A **recorded** worktree path that the configured root does not imply
    /// (T-0107). See [`pane_of_recorded`] for why a record is data rather than a
    /// permission.
    #[error(
        "{path} is not under the worktree root {root} this machine configures — refusing to \
         create or enter it (the record is from a different root: re-spawn the pane, or point \
         `[worktree] root` back at the directory its checkout is in)"
    )]
    OutsideRoot { path: String, root: String },
    #[error("{path} has uncommitted changes: {files} — commit, stash or `--force`")]
    Dirty { path: String, files: String },
    #[error("git {command} failed in {cwd}: {detail}")]
    Git {
        command: String,
        cwd: String,
        detail: String,
    },
}

/// One worktree, as `git worktree list --porcelain` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// The absolute path of the checkout.
    pub path: PathBuf,
    /// The commit it is at.
    pub head: String,
    /// The branch it has checked out, if any (a detached worktree has none).
    pub branch: Option<String>,
    /// Whether git considers it the main working tree.
    pub main: bool,
    /// Whether git could not use it (`prunable`): the directory is gone or is not
    /// a repository any more. Reported rather than hidden, because a worktree
    /// that vanished is exactly what a restore has to notice.
    pub prunable: bool,
}

impl Entry {
    /// The pane this worktree belongs to, when it is one of ours.
    ///
    /// Derived from the branch, which is the association's one home: the
    /// directory name is a convenience, the branch is what `git` records.
    #[must_use]
    pub fn pane(&self) -> Option<&str> {
        self.branch
            .as_deref()
            .and_then(|branch| branch.strip_prefix(BRANCH_PREFIX))
    }
}

/// Whether `pane` may name a directory.
///
/// Conservative on purpose: letters, digits, `.`, `_` and `-`, at most
/// [`MAX_PANE_ID`] bytes, and not `.` or `..`. Anything else is refused with the
/// reason, because the alternative — sanitising silently — turns two panes named
/// `a/b` and `a_b` into one directory, and turns `../../etc` into a write outside
/// the worktree root.
pub fn is_safe_pane_id(pane: &str) -> Result<(), WorktreeError> {
    let bad = |reason: &str| {
        Err(WorktreeError::BadPaneId {
            pane: pane.to_string(),
            reason: reason.to_string(),
        })
    };
    if pane.is_empty() {
        return bad("it is empty");
    }
    if pane.len() > MAX_PANE_ID {
        return bad(&format!(
            "it is {} bytes, over the {MAX_PANE_ID}-byte limit",
            pane.len()
        ));
    }
    if pane == "." || pane == ".." {
        return bad("it names a directory rather than a pane");
    }
    if pane.starts_with('-') {
        // A leading dash would be read as a flag by anything that takes this
        // value on a command line, including `git worktree add`.
        return bad("it starts with `-`, which a command line would read as a flag");
    }
    if let Some(bad_char) = pane
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '.' || *c == '_' || *c == '-'))
    {
        return bad(&format!(
            "it contains {bad_char:?}; a pane id in a worktree may use letters, digits, `.`, `_` and `-`"
        ));
    }
    Ok(())
}

/// The branch a pane's worktree is on: `arreo/<pane>`.
#[must_use]
pub fn branch_for(pane: &str) -> String {
    format!("{BRANCH_PREFIX}{pane}")
}

/// The directory a pane's worktree lives in: `<root>/<pane>`.
#[must_use]
pub fn path_for(root: &Path, pane: &str) -> PathBuf {
    root.join(pane)
}

/// The root with symlinks resolved, so every path this module returns has the
/// same spelling `git` reports.
///
/// The comparison that matters — "is the worktree at this path ours?" — is a
/// path comparison, and `git` reports canonical paths. A root reached through a
/// symlink (a `/tmp` that is a link, a home directory that moved) would otherwise
/// make one worktree look like two, and `prune_clean` would silently remove
/// nothing.
fn canonical_root(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}

/// A path with `.` and `..` resolved **lexically** — no filesystem access.
///
/// Needed because the check [`pane_of_recorded`] makes is a comparison, and a
/// raw string comparison is not the check: `/root/../escape` *starts with*
/// `/root` as text while resolving to `/escape`, so anything built on
/// `starts_with` would accept the escape it exists to refuse.
///
/// `..` cancels the previous component only when that component is a name. It
/// does not climb past a root (`/..` is `/`, per POSIX) and it is not collapsed
/// at the front of a relative path (`../../etc` stays `../../etc`: from an
/// unknown working directory there is nothing to cancel, and pretending
/// otherwise would turn one path into a different one).
fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                let pops = out
                    .components()
                    .next_back()
                    .is_some_and(|last| matches!(last, Component::Normal(_)));
                if pops {
                    out.pop();
                } else if !out.has_root() {
                    out.push(part.as_os_str());
                }
                // Otherwise `out` is a root and `..` is a no-op.
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The two spellings a path comparison should use: the filesystem's when it can
/// answer, and [`normalize`]'s when it cannot.
///
/// **Canonicalize first, because the false *refusal* matters too.** A configured
/// root that is reached through a symlink is the same directory as the canonical
/// spelling our own spawn recorded, and a check that compared the two as text
/// would refuse every pane on such a machine. Canonicalizing falls back to
/// normalization when the path does not exist — which is the interesting case
/// here, since the whole point is a directory that must *not* be created.
fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| normalize(path))
}

/// The pane a **recorded** worktree path is allowed to name, or why it is
/// refused (T-0107).
///
/// ## Why a record is not a permission
///
/// When the daemon starts a pane it does `create_dir_all(root)` and
/// `git worktree add <root>/<name>` — so a *root* taken from a record is an
/// arbitrary-directory primitive: a row naming `<abs>/outside/x` made the daemon
/// create that directory and register it, and a row naming the repository's main
/// checkout started the agent in the shared tree. Both were reproduced against
/// the real binary before this function existed.
///
/// The spawn route cannot be fooled this way: `path_for(root, pane) =
/// root.join(pane)` puts the pane inside the configured root **by construction**.
/// A recorded path has no such property — and [`is_safe_pane_id`] does not give
/// it one, because the name here is the last component of an absolute path the
/// record chose, which is trivially "safe". So the containment is checked
/// explicitly instead, and only the *name* survives the check: the caller passes
/// the **configured** root to [`ensure`], never the recorded one, which makes the
/// containment structural from there on.
///
/// The store is a file the operator's own uid can edit, so this is the "a path
/// that arrives from a file is data" rule rather than a hardening nicety — the
/// same class as the machine-name rules of T-0043. It also fires with no
/// attacker at all: any record written under a different `[worktree] root`.
///
/// ## What it requires
///
/// The recorded path must be exactly `<root>/<pane>` for the configured root —
/// parent and root must resolve to the same directory ([`resolved`], so a
/// symlinked root still matches) and the name must pass [`is_safe_pane_id`]. The
/// rule is deliberately the *shape our own spawn writes*, so every acceptance is
/// a path this machine would have produced itself, and every refusal is reported
/// rather than repaired: moving an agent to a directory it was not working in
/// would silently change which files it edits.
pub fn pane_of_recorded(recorded: &Path, root: &Path) -> Result<String, WorktreeError> {
    let refuse = || WorktreeError::OutsideRoot {
        path: recorded.display().to_string(),
        root: root.display().to_string(),
    };
    // `file_name()` is `None` for a path ending in `..`, `/` or `.` — a record
    // that names a directory rather than a pane.
    let Some(name) = recorded
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
    else {
        return Err(refuse());
    };
    if is_safe_pane_id(&name).is_err() {
        return Err(refuse());
    }
    let Some(parent) = recorded.parent() else {
        return Err(refuse());
    };
    if resolved(parent) != resolved(root) {
        return Err(refuse());
    }
    Ok(name)
}

/// Where worktrees live when nothing says otherwise: `<state>/worktrees`.
///
/// Under the state directory because that is where a program keeps what it needs
/// to pick up where it left off (the same reasoning `update::resume::dir`
/// records) — and because the default must not be inside the repository, where it
/// would show up in `git status` of the very checkout the worktrees come from.
#[must_use]
pub fn default_root() -> PathBuf {
    crate::update::resume::dir().join("worktrees")
}

/// Run `git` with `cwd` as its working directory and return stdout, or a typed
/// error naming the command.
///
/// `-C <cwd>` rather than `Command::current_dir`: a directory that does not exist
/// then produces *git's* own refusal ("cannot change to ..."), which is what the
/// caller wants to report as "not a repository". With `current_dir` the failure
/// arrives as an `io::Error` of kind `NotFound`, which is indistinguishable from
/// "the git binary is missing" — and those two need different advice.
fn git(cwd: &Path, args: &[&str]) -> Result<String, WorktreeError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                WorktreeError::NoGit(e.to_string())
            } else {
                WorktreeError::Git {
                    command: args.join(" "),
                    cwd: cwd.display().to_string(),
                    detail: e.to_string(),
                }
            }
        })?;
    if !output.status.success() {
        return Err(WorktreeError::Git {
            command: args.join(" "),
            cwd: cwd.display().to_string(),
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// The repository a path belongs to — `git rev-parse --show-toplevel`.
///
/// Canonicalized, because every comparison this module makes afterwards
/// ("is this worktree ours?") is a path comparison, and a symlinked `/tmp` or a
/// relative `--worktree` argument would otherwise make two spellings of one
/// directory look like two directories.
pub fn repo_root(from: &Path) -> Result<PathBuf, WorktreeError> {
    let out = git(from, &["rev-parse", "--show-toplevel"]).map_err(|e| match e {
        WorktreeError::Git { detail, .. } => WorktreeError::NotARepo {
            path: from.display().to_string(),
            detail,
        },
        other => other,
    })?;
    let root = PathBuf::from(out.trim());
    Ok(std::fs::canonicalize(&root).unwrap_or(root))
}

/// Every worktree of the repository `repo` belongs to.
pub fn list(repo: &Path) -> Result<Vec<Entry>, WorktreeError> {
    let out = git(repo, &["worktree", "list", "--porcelain"])?;
    Ok(parse_porcelain(&out))
}

/// Parse `git worktree list --porcelain`.
///
/// Split out so the format is testable without a repository: the parser is the
/// part that breaks when git changes its mind, and a test that needs a real repo
/// to check a missing `branch` line would never be written.
#[must_use]
pub fn parse_porcelain(text: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut current: Option<Entry> = None;
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(Entry {
                path: PathBuf::from(path),
                head: String::new(),
                branch: None,
                main: false,
                prunable: false,
            });
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        if let Some(head) = line.strip_prefix("HEAD ") {
            entry.head = head.to_string();
        } else if let Some(branch) = line.strip_prefix("branch ") {
            // `refs/heads/arreo/pane-1` → `arreo/pane-1`.
            entry.branch = Some(
                branch
                    .strip_prefix("refs/heads/")
                    .unwrap_or(branch)
                    .to_string(),
            );
        } else if line == "bare" {
            entry.main = true;
        } else if line == "prunable" || line.starts_with("prunable ") {
            entry.prunable = true;
        }
    }
    if let Some(entry) = current {
        entries.push(entry);
    }
    // `git worktree list` puts the main working tree first; the flag is derived
    // rather than parsed because the porcelain output marks `bare`, not `main`.
    if let Some(first) = entries.first_mut() {
        first.main = true;
    }
    entries
}

/// The worktree of `repo` at `path`, if there is one.
pub fn find(repo: &Path, path: &Path) -> Result<Option<Entry>, WorktreeError> {
    let wanted = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    Ok(list(repo)?.into_iter().find(|entry| {
        std::fs::canonicalize(&entry.path).unwrap_or_else(|_| entry.path.clone()) == wanted
    }))
}

/// The files with uncommitted changes in `path` (staged, unstaged and untracked).
///
/// The *names*, not a count: the refusal an operator sees has to let them decide
/// whether to throw the work away, and "3 files are dirty" does not.
pub fn status(path: &Path) -> Result<Vec<String>, WorktreeError> {
    let out = git(path, &["status", "--porcelain"])?;
    Ok(out
        .lines()
        .filter_map(|line| {
            // `XY path` — the status letters, a space, then the path. A rename
            // carries `old -> new`, which is reported whole: it is what the
            // operator has to reason about.
            line.get(3..).map(|rest| rest.trim().to_string())
        })
        .filter(|path| !path.is_empty())
        .collect())
}

/// Whether `path` has uncommitted changes.
pub fn is_dirty(path: &Path) -> Result<bool, WorktreeError> {
    Ok(!status(path)?.is_empty())
}

/// Make (or reuse) the worktree for `pane`, returning its path.
///
/// **Reuse is the point.** A second `spawn --worktree` of the same pane id — a
/// retry, a restart, an operator running the same command twice — finds the
/// existing checkout and returns it. Creating a second one would either fail
/// (the branch exists) or fork a second branch, and both make the pane↔worktree
/// association ambiguous, which is the one thing this feature must keep exact.
///
/// The branch is created from the repository's current `HEAD`, so a worktree
/// starts where the operator's checkout is — not from a remote, and not from a
/// guess about which branch is "the" base.
pub fn ensure(repo: &Path, root: &Path, pane: &str) -> Result<PathBuf, WorktreeError> {
    is_safe_pane_id(pane)?;
    // The repository first: everything below is a `git worktree` command in it,
    // and a path that is not a repository must be refused as such rather than
    // surfacing as a failed listing.
    let repo = repo_root(repo)?;
    let root = canonical_root(root);
    let path = path_for(&root, pane);
    let branch = branch_for(pane);

    // Already ours: reuse it, whatever state it is in (a dirty worktree is a
    // pane mid-task, which is the normal case).
    if let Some(entry) = find(&repo, &path)? {
        if entry.path.is_dir() && !entry.prunable {
            return Ok(entry.path);
        }
        // **A registration whose directory is gone.** `git worktree list` keeps
        // reporting it — marked `prunable`, which is why [`Entry`] carries that
        // flag — so returning `entry.path` would hand the caller a directory
        // that does not exist. That matters more than it looks: a spawn with a
        // missing working directory is not an error, because `portable-pty` drops
        // a `cwd` that is not a directory and falls back to the process's home.
        // Prune the stale registration and re-create the checkout on the branch,
        // which is where the work is.
        git(&repo, &["worktree", "prune"])?;
    }

    // The path exists but is not a registered worktree of this repository.
    // Refusing is the only safe answer: it may be another repository's checkout,
    // or a directory an operator made by hand, and `git worktree add` would
    // refuse it anyway with a worse message.
    if path.exists() {
        return Err(WorktreeError::NotOurWorktree {
            path: path.display().to_string(),
            repo: repo.display().to_string(),
        });
    }

    std::fs::create_dir_all(&root).map_err(|e| WorktreeError::Git {
        command: "mkdir -p".to_string(),
        cwd: root.display().to_string(),
        detail: e.to_string(),
    })?;
    // The root now exists, so it canonicalizes: do it once more so the path the
    // caller gets is the one `git` recorded.
    let root = canonical_root(&root);
    let path = path_for(&root, pane);

    let path_arg = path.display().to_string();
    // The branch may already exist with the worktree gone (a removed worktree
    // keeps its branch — that is the point of the branch): attach to it rather
    // than failing on a name that is already taken.
    let branch_exists = git(
        &repo,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    )
    .is_ok();
    if branch_exists {
        git(&repo, &["worktree", "add", &path_arg, &branch])?;
    } else {
        git(
            &repo,
            &["worktree", "add", "-b", &branch, &path_arg, "HEAD"],
        )?;
    }
    Ok(path)
}

/// Remove a pane's worktree, refusing a dirty one unless `force`.
///
/// The branch is **not** deleted: it holds the commits, and a worktree is a
/// checkout of it. An operator who wants the branch gone says so with
/// `git branch -D`, which is a different decision from "stop working here".
pub fn remove(repo: &Path, root: &Path, pane: &str, force: bool) -> Result<PathBuf, WorktreeError> {
    is_safe_pane_id(pane)?;
    let repo = repo_root(repo)?;
    let root = canonical_root(root);
    let path = path_for(&root, pane);
    let Some(entry) = find(&repo, &path)? else {
        return Ok(path);
    };
    // A registration whose directory is already gone: there is no status to read
    // and no worktree left to lose, so the only thing to clean up is git's own
    // bookkeeping. Refusing here would leave a pane that can never be cleaned.
    if !entry.path.is_dir() || entry.prunable {
        git(&repo, &["worktree", "prune"])?;
        return Ok(entry.path);
    }
    let files = status(&entry.path)?;
    if !files.is_empty() && !force {
        return Err(WorktreeError::Dirty {
            path: entry.path.display().to_string(),
            files: files.join(", "),
        });
    }
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    let path_arg = entry.path.display().to_string();
    args.push(&path_arg);
    git(&repo, &args)?;
    Ok(entry.path)
}

/// Remove every Arreo worktree of `repo` that is clean, returning what went and
/// what was kept because it was dirty.
///
/// Used at shutdown: a pane that has exited leaves a checkout nobody is working
/// in, and an operator should not accumulate one per task. A dirty one is kept
/// **and reported**, because deleting an agent's uncommitted work is the one
/// unrecoverable thing this module can do.
pub fn prune_clean(
    repo: &Path,
    root: &Path,
) -> Result<(Vec<PathBuf>, Vec<PathBuf>), WorktreeError> {
    let root = canonical_root(root);
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    for entry in list(repo)? {
        let Some(pane) = entry.pane() else { continue };
        if path_for(&root, pane) != entry.path {
            continue;
        }
        match remove(repo, &root, pane, false) {
            Ok(path) => removed.push(path),
            Err(WorktreeError::Dirty { .. }) => kept.push(entry.path),
            Err(e) => return Err(e),
        }
    }
    Ok((removed, kept))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch repository with one commit, under the repo's test scratch (never
    /// `/tmp`: it is a tmpfs here and a git repository in it is a real problem).
    fn scratch_repo(name: &str) -> (PathBuf, PathBuf) {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/T-0091")
            .join(name);
        let _ = std::fs::remove_dir_all(&base);
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).expect("scratch");
        // Canonical from here on: `git` reports canonical paths, and a test that
        // compared a `..`-spelled path against one would be testing spelling
        // rather than behavior.
        let base = std::fs::canonicalize(&base).expect("canonical");
        let repo = std::fs::canonicalize(&repo).expect("canonical");
        let run = |args: &[&str]| {
            let out = Command::new("git")
                .current_dir(&repo)
                .args(args)
                .env("GIT_AUTHOR_NAME", "arreo test")
                .env("GIT_AUTHOR_EMAIL", "test@arreo.invalid")
                .env("GIT_COMMITTER_NAME", "arreo test")
                .env("GIT_COMMITTER_EMAIL", "test@arreo.invalid")
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["init", "-q", "-b", "main"]);
        // The identity is configured rather than passed per command: a later
        // `git commit` in a *worktree* of this repository (the tests do that)
        // reads it from the repository, and a test that sets it only on `run`
        // would fail there for a reason that has nothing to do with the code
        // under test.
        run(&["config", "user.name", "arreo test"]);
        run(&["config", "user.email", "test@arreo.invalid"]);
        std::fs::write(repo.join("README.md"), "hello\n").expect("write");
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "initial"]);
        (base, repo)
    }

    /// The lexical normalizer, checked on the cases the containment rule leans
    /// on — including the two where "resolve `..` naively" is wrong.
    #[test]
    fn normalize_resolves_dots_without_climbing_past_a_root() {
        use std::path::PathBuf;
        let cases = [
            ("/r/../escape", "/escape"),
            ("/r/./x", "/r/x"),
            ("/r/sub/../x", "/r/x"),
            // `..` at a root is a no-op (POSIX), and at the front of a relative
            // path there is nothing to cancel.
            ("/..", "/"),
            ("../../etc", "../../etc"),
            ("/r", "/r"),
        ];
        for (input, want) in cases {
            assert_eq!(
                normalize(&PathBuf::from(input)),
                PathBuf::from(want),
                "{input}"
            );
        }
    }

    /// **A record is validated against the configured root, and only its name
    /// survives** (T-0107).
    ///
    /// The refusal matters because `ensure` *creates* directories: a root taken
    /// from a record let a row make the daemon create `<abs>/outside/nested/x`
    /// and `git worktree add` it, or start the agent in the repository's main
    /// checkout. Both were reproduced against the real binary before this
    /// existed.
    #[test]
    fn a_recorded_path_must_be_what_the_configured_root_implies() {
        use std::path::Path;
        let root = Path::new("/srv/worktrees");

        // The shape our own spawn writes — the only accepted one.
        assert_eq!(
            pane_of_recorded(Path::new("/srv/worktrees/fix"), root).expect("accepted"),
            "fix"
        );
        // A redundant spelling of the same directory is still the same
        // directory: the record is compared resolved, not as text.
        assert_eq!(
            pane_of_recorded(Path::new("/srv/worktrees/./fix"), root).expect("accepted"),
            "fix"
        );

        // Outside the configured root: the reproduced case (a path that does not
        // exist yet — the one that creates directories).
        let outside = pane_of_recorded(Path::new("/abs/outside/nested/x"), root)
            .expect_err("outside the root");
        assert!(
            matches!(outside, WorktreeError::OutsideRoot { .. }),
            "{outside:?}"
        );
        // The message names both facts the operator needs to decide.
        let text = outside.to_string();
        assert!(text.contains("/abs/outside/nested/x"), "{text}");
        assert!(text.contains("/srv/worktrees"), "{text}");

        // The main checkout: a real directory that is not `root/<name>`.
        assert!(pane_of_recorded(Path::new("/srv/project"), root).is_err());

        // **The escape a `starts_with` comparison would accept**: this path
        // begins with the root as text and resolves outside it.
        let escape = pane_of_recorded(Path::new("/srv/worktrees/../escape"), root)
            .expect_err("the dot-dot escape");
        assert!(
            matches!(escape, WorktreeError::OutsideRoot { .. }),
            "{escape:?}"
        );

        // A record naming a directory rather than a pane.
        assert!(pane_of_recorded(Path::new("/srv/worktrees/.."), root).is_err());
        assert!(pane_of_recorded(Path::new("/srv/worktrees"), root).is_err());
        assert!(pane_of_recorded(Path::new("/"), root).is_err());
        // And a name that is not a safe directory name stays refused.
        assert!(pane_of_recorded(Path::new("/srv/worktrees/."), root).is_err());
    }

    /// A root reached through a **symlink** is the same directory as the
    /// canonical spelling the spawn recorded, so the check must not refuse it.
    ///
    /// This is the false-refusal half, and it is a real configuration: an
    /// operator points `[worktree] root` at `/data/worktrees` while `/data` is a
    /// link, and every pane's record carries the resolved path.
    ///
    /// Unix-only, and not merely because `std::os::unix::fs` is: on Windows a
    /// symlink needs a privilege the test cannot assume, so the portable half of
    /// this property is [`pane_of_recorded`]'s use of `canonicalize` itself. The
    /// `#[cfg]` is what keeps the crate type-checking for `windows-msvc`
    /// (`cargo xtask check-targets`), which is how this was caught: the
    /// ungated version broke that gate rather than any Linux test.
    #[cfg(unix)]
    #[test]
    fn a_symlinked_root_is_recognised_as_the_same_directory() {
        use std::os::unix::fs::symlink;
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/T-0107")
            .join("symlink");
        let _ = std::fs::remove_dir_all(&base);
        let real = base.join("real");
        std::fs::create_dir_all(&real).expect("scratch");
        let link = base.join("link");
        symlink(&real, &link).expect("symlink");

        // The record carries the canonical spelling (that is what `ensure`
        // returns), the configuration names the link.
        let canonical = std::fs::canonicalize(&real).expect("canonical");
        let recorded = canonical.join("pane-1");
        assert_eq!(
            pane_of_recorded(&recorded, &link).expect("the same directory"),
            "pane-1"
        );
        // …and the reverse spelling too.
        assert_eq!(
            pane_of_recorded(&link.join("pane-2"), &canonical).expect("the same directory"),
            "pane-2"
        );
        // While a *different* directory behind the same link is still refused.
        assert!(pane_of_recorded(&base.join("elsewhere/pane-3"), &link).is_err());
    }

    /// The gate that keeps a pane id from naming a path.
    #[test]
    fn a_pane_id_is_validated_as_a_directory_name() {
        for good in ["pane-1", "agent", "fix.login", "a_b-c9"] {
            assert!(is_safe_pane_id(good).is_ok(), "{good} should be allowed");
        }
        for bad in [
            "",
            ".",
            "..",
            "-x",
            "a/b",
            "../etc",
            "a b",
            "a\nb",
            &"x".repeat(65),
        ] {
            assert!(is_safe_pane_id(bad).is_err(), "{bad:?} should be refused");
        }
        // The escape this exists for, named in the message so the operator knows
        // what was rejected rather than being told "invalid id".
        let err = is_safe_pane_id("../../etc/passwd").expect_err("escape");
        assert!(err.to_string().contains("../../etc/passwd"), "{err}");
    }

    /// The porcelain parser, without a repository: the parts that matter are the
    /// optional `branch` line, a detached worktree, and `prunable`.
    #[test]
    fn porcelain_parsing_handles_branches_and_prunable_entries() {
        let text = "worktree /repo\nHEAD abc123\nbranch refs/heads/main\n\n\
                    worktree /wt/pane-1\nHEAD def456\nbranch refs/heads/arreo/pane-1\n\n\
                    worktree /wt/detached\nHEAD 999aaa\ndetached\n\n\
                    worktree /wt/gone\nHEAD 111bbb\nbranch refs/heads/arreo/gone\nprunable gitdir file points to non-existent location\n";
        let entries = parse_porcelain(text);
        assert_eq!(entries.len(), 4, "{entries:?}");
        assert!(entries[0].main && entries[0].branch.as_deref() == Some("main"));
        assert_eq!(entries[1].pane(), Some("pane-1"));
        assert_eq!(entries[2].pane(), None, "a detached worktree is not ours");
        assert!(entries[3].prunable);
        assert_eq!(entries[3].pane(), Some("gone"));
    }

    /// A worktree is made, reused, and is a real checkout of the repository.
    #[test]
    fn ensure_makes_a_worktree_and_reuses_it() {
        let (base, repo) = scratch_repo("ensure");
        let root = base.join("worktrees");

        let path = ensure(&repo, &root, "pane-1").expect("make");
        assert_eq!(path, root.join("pane-1"));
        assert!(path.join("README.md").is_file(), "a real checkout");
        assert_eq!(
            git(&path, &["rev-parse", "--abbrev-ref", "HEAD"])
                .expect("branch")
                .trim(),
            "arreo/pane-1"
        );

        // Reuse: the same pane id twice is one worktree, not two.
        let again = ensure(&repo, &root, "pane-1").expect("reuse");
        assert_eq!(again, path);
        let ours: Vec<_> = list(&repo)
            .expect("list")
            .into_iter()
            .filter(|e| e.pane().is_some())
            .collect();
        assert_eq!(ours.len(), 1, "one worktree for one pane: {ours:?}");

        // Two panes are two checkouts of one repository.
        let other = ensure(&repo, &root, "pane-2").expect("second pane");
        assert_ne!(other, path);
        assert_eq!(
            list(&repo)
                .expect("list")
                .iter()
                .filter(|e| e.pane().is_some())
                .count(),
            2
        );
    }

    /// Two panes cannot see each other's files — the collision this feature
    /// exists to prevent, asserted rather than assumed.
    #[test]
    fn two_worktrees_do_not_share_a_working_directory() {
        let (base, repo) = scratch_repo("isolation");
        let root = base.join("worktrees");
        let one = ensure(&repo, &root, "pane-1").expect("one");
        let two = ensure(&repo, &root, "pane-2").expect("two");

        // The same filename, different contents, in both — the case a shared
        // working directory loses.
        std::fs::write(one.join("task.txt"), "pane one\n").expect("write");
        std::fs::write(two.join("task.txt"), "pane two\n").expect("write");
        assert_eq!(
            std::fs::read_to_string(one.join("task.txt")).expect("read"),
            "pane one\n"
        );
        assert_eq!(
            std::fs::read_to_string(two.join("task.txt")).expect("read"),
            "pane two\n"
        );
        assert!(
            !repo.join("task.txt").exists(),
            "the main checkout is untouched"
        );
    }

    /// A dirty worktree is never removed by accident, and the refusal names the
    /// files. With `force` it goes.
    #[test]
    fn a_dirty_worktree_is_refused_and_the_refusal_names_the_files() {
        let (base, repo) = scratch_repo("dirty");
        let root = base.join("worktrees");
        let path = ensure(&repo, &root, "pane-1").expect("make");
        std::fs::write(path.join("work-in-progress.txt"), "not committed\n").expect("write");

        assert!(is_dirty(&path).expect("status"));
        let err = remove(&repo, &root, "pane-1", false).expect_err("dirty");
        assert!(err.to_string().contains("work-in-progress.txt"), "{err}");
        assert!(path.exists(), "the worktree survived the refusal");

        let removed = remove(&repo, &root, "pane-1", true).expect("force");
        assert_eq!(removed, path);
        assert!(!path.exists());
        // The branch survives: it holds the commits, and deleting it is a
        // different decision from "stop working here".
        assert!(git(&repo, &["rev-parse", "--verify", "refs/heads/arreo/pane-1"]).is_ok());
    }

    /// Removing a clean worktree takes the checkout and leaves the branch, and a
    /// second `ensure` attaches to that branch rather than failing on the name.
    #[test]
    fn a_removed_worktree_can_be_recreated_on_its_branch() {
        let (base, repo) = scratch_repo("recreate");
        let root = base.join("worktrees");
        let path = ensure(&repo, &root, "pane-1").expect("make");
        std::fs::write(path.join("committed.txt"), "kept\n").expect("write");
        git(&path, &["add", "."]).expect("add");
        git(&path, &["commit", "-q", "-m", "work"]).expect("commit");

        remove(&repo, &root, "pane-1", false).expect("clean remove");
        assert!(!path.exists());

        let again = ensure(&repo, &root, "pane-1").expect("recreate");
        assert_eq!(again, path);
        assert!(
            again.join("committed.txt").is_file(),
            "the new worktree is on the branch that holds the work"
        );
    }

    /// A directory that exists but is not our worktree is refused, never
    /// clobbered.
    #[test]
    fn a_foreign_directory_at_the_worktree_path_is_refused() {
        let (base, repo) = scratch_repo("foreign");
        let root = base.join("worktrees");
        let path = path_for(&root, "pane-1");
        std::fs::create_dir_all(&path).expect("mkdir");
        std::fs::write(path.join("someone-elses.txt"), "precious\n").expect("write");

        let err = ensure(&repo, &root, "pane-1").expect_err("refused");
        assert!(matches!(err, WorktreeError::NotOurWorktree { .. }), "{err}");
        assert!(
            path.join("someone-elses.txt").is_file(),
            "nothing was touched"
        );
    }

    /// Not a repository is refused, and nothing is created.
    ///
    /// The path is one that does not exist, which is the case that actually
    /// happens (a pane whose repository directory was deleted, a `--worktree`
    /// typed at the wrong root): `git rev-parse` fails and the refusal is
    /// [`WorktreeError::NotARepo`] rather than an empty worktree list. A *real*
    /// non-repository cannot be built under `target/test-scratch` — that
    /// directory is inside this workspace's own repository — so the slice proves
    /// that case with `GIT_CEILING_DIRECTORIES`, where it is free to set the
    /// environment of the processes it spawns.
    #[test]
    fn a_path_that_is_not_a_repository_is_refused_and_creates_nothing() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/T-0091")
            .join("nonrepo");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("scratch");
        let missing = base.join("does-not-exist");

        let err = repo_root(&missing).expect_err("not a repo");
        assert!(matches!(err, WorktreeError::NotARepo { .. }), "{err}");

        let err = ensure(&missing, &base.join("worktrees"), "pane-1").expect_err("not a repo");
        assert!(matches!(err, WorktreeError::NotARepo { .. }), "{err}");
        assert!(
            std::fs::read_dir(&base).expect("read").next().is_none(),
            "nothing was created"
        );
    }

    /// **A worktree whose directory was deleted is re-created, not returned as
    /// a path that is not there.**
    ///
    /// `git worktree list` keeps reporting it (marked `prunable`), so the reuse
    /// branch used to hand back a missing directory — and a spawn with a missing
    /// working directory is *not* an error, because `portable-pty` drops a `cwd`
    /// that is not a directory and falls back to the process's home. The pane
    /// would have run somewhere nobody chose.
    #[test]
    fn a_worktree_whose_directory_vanished_is_recreated() {
        let (base, repo) = scratch_repo("vanished");
        let root = base.join("worktrees");
        let path = ensure(&repo, &root, "pane-1").expect("make");
        std::fs::write(path.join("committed.txt"), "kept\n").expect("write");
        git(&path, &["add", "."]).expect("add");
        git(&path, &["commit", "-q", "-m", "work"]).expect("commit");

        // The operator (or a cleanup job) removes the checkout behind git's back.
        std::fs::remove_dir_all(&path).expect("rm -rf");

        // git still lists it, and says why.
        let entry = find(&repo, &path).expect("list").expect("still registered");
        assert!(
            entry.prunable || !entry.path.is_dir(),
            "the stale registration is reported as such: {entry:?}"
        );

        let again = ensure(&repo, &root, "pane-1").expect("recreate");
        assert!(again.is_dir(), "a real directory, not a stale path");
        assert!(
            again.join("committed.txt").is_file(),
            "the re-created worktree is on the branch that holds the work"
        );
    }

    /// Cleaning up a worktree whose directory is gone is a bookkeeping problem,
    /// not a refusal: otherwise the registration can never be cleared.
    #[test]
    fn removing_a_worktree_whose_directory_vanished_clears_the_registration() {
        let (base, repo) = scratch_repo("remove-vanished");
        let root = base.join("worktrees");
        let path = ensure(&repo, &root, "pane-1").expect("make");
        std::fs::remove_dir_all(&path).expect("rm -rf");

        remove(&repo, &root, "pane-1", false).expect("clears the registration");
        assert!(
            find(&repo, &path).expect("list").is_none(),
            "git no longer reports it"
        );
    }

    /// A spawn into a directory that does not exist is refused, and the refusal
    /// is what makes the silent `$HOME` fallback impossible.
    #[test]
    fn a_spawn_into_a_missing_directory_is_refused() {
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/T-0091")
            .join("missing-cwd");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("scratch");
        let missing = base.join("not-here");

        let err = match crate::pty::Pane::spawn_in_dir(
            "/bin/sh",
            &["-c", "true"],
            80,
            24,
            &[],
            Some(&missing),
        ) {
            Ok(_) => panic!("a missing directory must be refused"),
            Err(e) => e,
        };
        assert!(
            matches!(err, crate::pty::PtyError::NoSuchDirectory(_)),
            "{err:?}"
        );
        assert!(err.to_string().contains("not-here"), "{err}");
    }

    /// Shutdown cleanup removes the clean ones and keeps the dirty ones.
    #[test]
    fn prune_clean_keeps_a_dirty_worktree_and_removes_the_rest() {
        let (base, repo) = scratch_repo("prune");
        let root = base.join("worktrees");
        let clean = ensure(&repo, &root, "pane-1").expect("clean");
        let dirty = ensure(&repo, &root, "pane-2").expect("dirty");
        std::fs::write(dirty.join("uncommitted.txt"), "work\n").expect("write");

        let (removed, kept) = prune_clean(&repo, &root).expect("prune");
        assert_eq!(removed, vec![clean.clone()]);
        assert_eq!(kept, vec![dirty.clone()]);
        assert!(!clean.exists());
        assert!(dirty.join("uncommitted.txt").is_file(), "the work survived");
    }
}
