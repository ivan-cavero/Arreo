//! T-0092: `arreo diff` — the JSON contract, and the two facts it must not
//! collapse.
//!
//! Process-level, through the real binary, because the contract is what a
//! *script* consumes: a renamed key or a collapsed message is a wire break for
//! something outside this repository, and only running the verb proves what it
//! printed. No daemon is involved — the diff is computed locally by running git,
//! which is why this test needs a repository and not a socket.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The CLI binary under test (cargo builds it before these run).
fn arreo() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_arreo"))
}

/// A scratch repository under the workspace's `target/` (never `/tmp`: it is a
/// tmpfs here, and a git repository in it is a real problem).
struct Scratch {
    dir: PathBuf,
    repo: PathBuf,
    root: PathBuf,
    config: PathBuf,
}

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/T-0092-cli")
            .join(tag);
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        let root = dir.join("worktrees");
        std::fs::create_dir_all(&repo).expect("scratch repo");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.name", "arreo test"]);
        git(&["config", "user.email", "test@arreo.invalid"]);
        std::fs::write(repo.join("tracked.txt"), "one\ntwo\nthree\n").expect("write");
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "base"]);

        let config = dir.join("arreo.toml");
        std::fs::write(
            &config,
            format!(
                "[worktree]\nroot = \"{}\"\nrepo = \"{}\"\n",
                root.display(),
                repo.display()
            ),
        )
        .expect("config");
        let dir = std::fs::canonicalize(&dir).expect("canonical");
        Self {
            repo: std::fs::canonicalize(&repo).expect("canonical"),
            root: std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone()),
            config: std::fs::canonicalize(&config).expect("canonical"),
            dir,
        }
    }

    /// Make the worktree for `pane` with `git worktree add`, so the test does not
    /// need a daemon: the checkout is the only thing `arreo diff` reads.
    fn worktree(&self, pane: &str) -> PathBuf {
        let path = self.root.join(pane);
        std::fs::create_dir_all(&self.root).expect("root");
        let out = Command::new("git")
            .arg("-C")
            .arg(&self.repo)
            .args([
                "worktree",
                "add",
                "-b",
                &format!("arreo/{pane}"),
                &path.display().to_string(),
                "HEAD",
            ])
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "worktree add: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::fs::canonicalize(&path).expect("canonical")
    }

    fn run(&self, args: &[&str]) -> (i32, String, String) {
        let out = Command::new(arreo())
            .args(args)
            .arg("--repo")
            .arg(&self.repo)
            .arg("--config")
            .arg(&self.config)
            .env("ARREO_STATE_DIR", self.dir.join("state"))
            .output()
            .expect("arreo runs");
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
}

/// **The `--json` schema is a contract.** Every key a consumer reads is asserted
/// by name and by shape: a rename here is a break for a script, and this is the
/// test that turns red when someone tidies one away.
#[test]
fn the_json_schema_is_pinned() {
    let scratch = Scratch::new("schema");
    let path = scratch.worktree("fix");
    // One modification and one untracked file — the two shapes a consumer reads
    // differently, and the ones the schema must distinguish.
    std::fs::write(path.join("tracked.txt"), "one\nTWO\nthree\n").expect("write");
    std::fs::write(path.join("added.md"), "fresh\n").expect("write");

    let (code, stdout, stderr) = scratch.run(&["diff", "fix", "--json"]);
    assert_eq!(code, 0, "stderr: {stderr}");
    let value: serde_json::Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("--json must parse: {e}\n{stdout}"));

    assert_eq!(value["schema"], serde_json::json!(1), "{value}");
    assert!(value["summary"].is_string(), "{value}");
    // Absent when nothing was hidden — an absent field is better than a zero a
    // consumer has to interpret.
    assert!(value.get("hidden_untracked").is_none(), "{value}");

    let files = value["files"].as_array().expect("files is an array");
    assert_eq!(files.len(), 2, "{value}");
    let by_path = |want: &str| {
        files
            .iter()
            .find(|f| f["path"] == serde_json::json!(want))
            .unwrap_or_else(|| panic!("{want} in {value}"))
    };

    let modified = by_path("tracked.txt");
    assert_eq!(modified["change"], serde_json::json!("modified"));
    assert_eq!(modified["old_path"], serde_json::json!("tracked.txt"));
    assert_eq!(modified["new_path"], serde_json::json!("tracked.txt"));
    assert_eq!(modified["binary"], serde_json::json!(false));
    assert_eq!(modified["added"], serde_json::json!(1));
    assert_eq!(modified["removed"], serde_json::json!(1));

    let hunks = modified["hunks"].as_array().expect("hunks");
    assert_eq!(hunks.len(), 1, "{modified}");
    let hunk = &hunks[0];
    assert_eq!(hunk["old_start"], serde_json::json!(1));
    assert_eq!(hunk["old_count"], serde_json::json!(3));
    assert_eq!(hunk["new_start"], serde_json::json!(1));
    assert_eq!(hunk["new_count"], serde_json::json!(3));
    assert_eq!(hunk["section"], serde_json::json!(""));
    let lines = hunk["lines"].as_array().expect("lines");
    assert_eq!(lines.len(), 4, "{hunk}");
    // A context line carries both numbers; a removed line only the old one; an
    // added line only the new one. That asymmetry is what tells a consumer which
    // gutter to draw, so it is the contract.
    let context = &lines[0];
    assert_eq!(context["kind"], serde_json::json!("context"));
    assert_eq!(context["old_line"], serde_json::json!(1));
    assert_eq!(context["new_line"], serde_json::json!(1));
    assert_eq!(context["no_newline"], serde_json::json!(false));
    let removed = lines
        .iter()
        .find(|l| l["kind"] == "removed")
        .expect("removed");
    assert_eq!(removed["old_line"], serde_json::json!(2));
    assert_eq!(removed["new_line"], serde_json::Value::Null);
    let added = lines.iter().find(|l| l["kind"] == "added").expect("added");
    assert_eq!(added["old_line"], serde_json::Value::Null);
    assert_eq!(added["new_line"], serde_json::json!(2));

    // The untracked file: git never tracked it, so there is no old side at all.
    let new_file = by_path("added.md");
    assert_eq!(new_file["change"], serde_json::json!("added"));
    assert_eq!(new_file["old_path"], serde_json::json!(""));
    assert_eq!(new_file["added"], serde_json::json!(1));
}

/// **"No worktree" and "no changes" are different facts** and must not collapse
/// into one message — a reviewer who sees "no changes" about a pane that never
/// had a checkout has been told something false.
#[test]
fn no_worktree_and_no_changes_are_different_answers() {
    let scratch = Scratch::new("twofacts");
    let path = scratch.worktree("clean");

    // Clean: exit 0, "no changes", and specifically not the no-worktree line.
    let (code, stdout, stderr) = scratch.run(&["diff", "clean"]);
    assert_eq!(code, 0, "a clean worktree is not a failure: {stderr}");
    let clean = format!("{stdout}{stderr}");
    assert!(clean.contains("no changes"), "{clean}");
    assert!(!clean.contains("no worktree"), "{clean}");

    // No worktree at all: a different exit code and a different sentence.
    let (code, stdout, stderr) = scratch.run(&["diff", "never-spawned"]);
    assert_ne!(code, 0, "a missing worktree is a failure: {stdout}{stderr}");
    let missing = format!("{stdout}{stderr}");
    assert!(missing.contains("no worktree"), "{missing}");
    assert!(
        missing.contains("never-spawned"),
        "the refusal names the pane: {missing}"
    );
    assert!(!missing.contains("no changes"), "{missing}");

    // And a change turns the clean answer into a diff.
    std::fs::write(path.join("tracked.txt"), "one\nCHANGED\nthree\n").expect("write");
    let (code, stdout, _) = scratch.run(&["diff", "clean"]);
    assert_eq!(code, 0);
    assert!(stdout.contains("CHANGED"), "{stdout}");
    assert!(!stdout.contains("no changes"), "{stdout}");
}

/// The human view names what a reviewer needs: the file, its shape, the hunk and
/// both sides of it. Asserted on the real output, because "readable" is the
/// feature.
#[test]
fn the_text_view_shows_the_change() {
    let scratch = Scratch::new("text");
    let path = scratch.worktree("review");
    std::fs::write(path.join("tracked.txt"), "one\nTWO\nthree\n").expect("write");
    std::fs::write(path.join("fresh.md"), "new file\n").expect("write");

    let (code, stdout, stderr) = scratch.run(&["diff", "review"]);
    assert_eq!(code, 0, "{stderr}");
    for needle in [
        "tracked.txt",
        "modified",
        "TWO",
        "two",
        "fresh.md",
        "added",
        "files changed",
    ] {
        assert!(stdout.contains(needle), "missing {needle:?} in:\n{stdout}");
    }
    // A directory that is not a repository is refused rather than reported as an
    // empty diff.
    let out = Command::new(arreo())
        .args(["diff", "review", "--repo", "/does/not/exist"])
        .env("ARREO_STATE_DIR", scratch.dir.join("state"))
        .output()
        .expect("arreo runs");
    assert_ne!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("/does/not/exist"), "{text}");
}

/// The `[worktree] repo` of the configuration is honoured when `--repo` is not
/// given — the same rule the daemon applies when it *creates* the worktree.
///
/// Without it, a consumer running anywhere but the repository reports "no
/// worktree" about a pane that plainly has one: a wrong answer that reads like a
/// fact about the pane. Found by the T-0092 slice, which runs the TUI from the
/// workspace root.
#[test]
fn the_configured_repository_is_used_when_no_repo_flag_is_given() {
    let scratch = Scratch::new("configured-repo");
    let path = scratch.worktree("cfg");
    std::fs::write(path.join("tracked.txt"), "one\nCONFIGURED\nthree\n").expect("write");

    // No `--repo`, and a working directory that is emphatically not the
    // repository.
    let out = Command::new(arreo())
        .args(["diff", "cfg"])
        .arg("--config")
        .arg(&scratch.config)
        .current_dir(Path::new("/"))
        .env("ARREO_STATE_DIR", scratch.dir.join("state"))
        .output()
        .expect("arreo runs");
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("CONFIGURED"),
        "the configured repository was used: {stdout}"
    );
}
