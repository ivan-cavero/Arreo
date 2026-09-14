//! Unified-diff parsing (T-0092): turn `git diff` bytes into the structure a
//! reviewer reads.
//!
//! One sentence: a **parser, not a pager** — it answers "what changed, in which
//! file, at which line" as typed data, so the TUI can style a hunk from the theme
//! tokens (T-0016) and a CLI consumer can assert on the same values.
//!
//! ## Written against real bytes, not against the format's documentation
//!
//! Every rule here was measured on this box (git 2.47.3); the transcript is
//! `.loop/evidence/T-0092/git-diff-bytes.txt`. Four rules a careful reader would
//! get wrong from a spec:
//!
//! - **Paths are C-quoted when they need to be** — `diff --git
//!   "a/quote\"name.txt" "b/quote\"name.txt"` and `"a/t\303\244b
//!   \303\247h\303\244r.txt"`. The octal escapes are *bytes*, which are UTF-8 in
//!   practice, so unquoting builds bytes and decodes afterwards rather than
//!   mapping characters one to one.
//! - **The `diff --git` header is not a reliable source of path names.** For
//!   `my file.txt` it is `diff --git a/my file.txt b/my file.txt` — unquoted, with
//!   a space, and therefore ambiguous. It is parsed here as a **fallback** (a
//!   mode-only change has no other source), and `---`/`+++` overrides it.
//! - **A `---`/`+++` line whose path contains a space ends with a TAB**
//!   (`+++ b/my file.txt\t`) and the header line does not, so the tab is stripped
//!   where it appears rather than assumed everywhere.
//! - **A pure rename has no `---`/`+++` lines and no hunks at all.** The paths
//!   live in `rename from`/`rename to`; a parser that reads only the `---`/`+++`
//!   pair loses both names. A mode-only change is the same shape.
//!
//! Also measured: an empty line in the file is emitted as a bare prefix (`+` or
//! `" "` — never a zero-length line), and a clean tree is zero bytes of output,
//! which parses to an empty [`Diff`] rather than an error.
//!
//! ## What it refuses
//!
//! A line in a file section that this parser does not recognize is
//! [`DiffError::UnexpectedLine`], naming the line number and the text. A parser
//! that guessed would render a screen that looks like a diff and is not one. The
//! input is always bytes this workspace ran `git diff --no-color` for, so an
//! unrecognized line is a bug report — which is exactly why it is loud.
//!
//! Binary files are **not** unrecognized: git says so explicitly
//! (`Binary files … differ`, or `GIT binary patch` followed by a base85 payload),
//! and from `GIT binary patch` the rest of the section is skipped, because a
//! payload is not text to parse. `binary` is a fact about the file, and it is
//! reported as one — distinct from "no changes".

use std::fmt;
use std::path::Path;

/// What can go wrong. Every variant carries the line number, so a report can be
/// acted on without re-running git by hand.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiffError {
    /// A line this parser does not recognize inside a file section.
    #[error("line {line}: unrecognized diff line {text:?}")]
    UnexpectedLine { line: usize, text: String },
    /// A `@@` line that is not a hunk header.
    #[error("line {line}: malformed hunk header {text:?}")]
    MalformedHunkHeader { line: usize, text: String },
    /// Text before the first `diff --git` line: not a diff at all.
    #[error("line {line}: content before the first `diff --git` line: {text:?}")]
    ContentBeforeHeader { line: usize, text: String },
    /// `git` refused. Carries the command, so a report is actionable.
    #[error("git {command} failed in {cwd}: {detail}")]
    Git {
        command: String,
        cwd: String,
        detail: String,
    },
    /// The repository has no commits, so there is no `HEAD` to diff against.
    #[error("{path} has no commits (HEAD is unborn): there is nothing to diff against")]
    NoHead { path: String },
}

/// What happened to a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Added,
    Deleted,
    Modified,
    /// `similarity` is git's percentage, absent when it printed none.
    Renamed {
        from: String,
        to: String,
        similarity: Option<u32>,
    },
    Copied {
        from: String,
        to: String,
        similarity: Option<u32>,
    },
    /// Only the file mode changed (a `chmod`, an executable bit).
    ModeChanged {
        old: String,
        new: String,
    },
}

impl Change {
    /// One word, for a listing column.
    #[must_use]
    pub fn word(&self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Deleted => "deleted",
            Self::Modified => "modified",
            Self::Renamed { .. } => "renamed",
            Self::Copied { .. } => "copied",
            Self::ModeChanged { .. } => "mode",
        }
    }

    /// The old path this change names, when the change carries one (a rename or
    /// a copy); empty otherwise.
    #[must_use]
    pub fn from_path(&self) -> &str {
        match self {
            Self::Renamed { from, .. } | Self::Copied { from, .. } => from,
            _ => "",
        }
    }
}

/// How a line relates to the two sides of the diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Present in both sides.
    Context,
    /// Only in the new side.
    Added,
    /// Only in the old side.
    Removed,
}

/// One line of a hunk, with the line numbers a reviewer navigates by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: Kind,
    /// The text **without** the diff prefix and **without** the trailing newline.
    ///
    /// A CR from a CRLF file is kept: it is the file's content, and stripping it
    /// would make this parser disagree with the file it describes.
    pub text: String,
    /// The line number in the old file; `None` for an added line.
    pub old_line: Option<u32>,
    /// The line number in the new file; `None` for a removed line.
    pub new_line: Option<u32>,
    /// git's `\ No newline at end of file` marker, which describes **this** line.
    pub no_newline: bool,
}

/// One `@@` hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// git's trailing context after the second `@@` (the enclosing function, the
    /// nearest heading). Empty when it printed none.
    pub section: String,
    pub lines: Vec<Line>,
}

impl Hunk {
    /// The `@@ -a,b +c,d @@ section` line, reconstructed.
    #[must_use]
    pub fn header(&self) -> String {
        let mut out = format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_count, self.new_start, self.new_count
        );
        if !self.section.is_empty() {
            out.push(' ');
            out.push_str(&self.section);
        }
        out
    }

    #[must_use]
    pub fn added(&self) -> usize {
        self.lines.iter().filter(|l| l.kind == Kind::Added).count()
    }

    #[must_use]
    pub fn removed(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| l.kind == Kind::Removed)
            .count()
    }
}

/// One file's changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// The old-side path; empty when the file did not exist before.
    pub old_path: String,
    /// The new-side path; empty when the file no longer exists.
    pub new_path: String,
    pub change: Change,
    /// git could not show this as text. The hunks are empty and that is the fact —
    /// not "no changes".
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

impl FileDiff {
    /// The path to show a reviewer: the new one, falling back to the old for a
    /// deletion (which has no new side).
    #[must_use]
    pub fn path(&self) -> &str {
        if self.new_path.is_empty() {
            &self.old_path
        } else {
            &self.new_path
        }
    }

    #[must_use]
    pub fn added(&self) -> usize {
        self.hunks.iter().map(Hunk::added).sum()
    }

    #[must_use]
    pub fn removed(&self) -> usize {
        self.hunks.iter().map(Hunk::removed).sum()
    }
}

/// A whole diff.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diff {
    pub files: Vec<FileDiff>,
    /// How many untracked files were **not** shown because the cap was reached.
    /// Zero means nothing was hidden. Reported rather than silent: a review screen
    /// that quietly omits files is worse than one that says how many it left out.
    pub hidden_untracked: usize,
}

impl Diff {
    /// Whether there is nothing to review. A clean tree parses to this, not to an
    /// error: "no changes" is an answer.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// `3 files changed, +12 -4` — the shape a status line shows. When untracked
    /// files were left out, it says so: a summary that hides a truncation is the
    /// one thing an operator cannot detect from the screen.
    #[must_use]
    pub fn summary(&self) -> String {
        let files = self.files.len();
        let added: usize = self.files.iter().map(FileDiff::added).sum();
        let removed: usize = self.files.iter().map(FileDiff::removed).sum();
        let mut out = format!(
            "{files} file{} changed, +{added} -{removed}",
            if files == 1 { "" } else { "s" }
        );
        if self.hidden_untracked > 0 {
            out.push_str(&format!(
                " (and {} more untracked file{} not shown)",
                self.hidden_untracked,
                if self.hidden_untracked == 1 { "" } else { "s" }
            ));
        }
        out
    }
}

/// How many untracked files [`worktree_diff`] reads one by one.
///
/// Each one is a `git diff --no-index` invocation (git's own answer for a file it
/// does not track), so an unbounded loop would run one process per file an agent
/// happened to create. A hundred covers any review a human reads; past it the
/// count of what was left out is reported in [`Diff::hidden_untracked`].
pub const UNTRACKED_LIMIT: usize = 100;

/// The diff of a **working tree**: staged, unstaged, and untracked.
///
/// The entry point both the CLI and the TUI use, so "which git commands produce a
/// diff" is answered once. Three quarters of it are one command — `git diff HEAD`
/// covers staged *and* unstaged changes against the last commit — and the fourth
/// is the part that is easy to get wrong:
///
/// **Untracked files.** `git diff HEAD` cannot show a file git does not track, and
/// for this product that is the *common* case rather than a corner: an agent that
/// wrote a new file and did not `git add` it would be invisible to a review that
/// only ran `git diff`. Each untracked file gets git's own answer
/// (`git diff --no-index -- /dev/null <file>`, which emits the ordinary `new file`
/// shape this parser already reads), bounded by [`UNTRACKED_LIMIT`].
///
/// **Not `git add --intent-to-add`.** That is the other way to make untracked
/// files appear in one `git diff`, and it *writes to the repository's index* — a
/// read-only review verb must not mutate the tree it is reviewing, and an agent
/// mid-task would see its index change under it.
///
/// `--no-color` is passed for the same reason `git diff` is run at all rather than
/// read from a pager: the bytes are parsed, and an escape sequence is not a diff.
pub fn worktree_diff(dir: &Path) -> Result<Diff, DiffError> {
    if !dir.is_dir() {
        return Err(DiffError::Git {
            command: "rev-parse".to_string(),
            cwd: dir.display().to_string(),
            detail: "the directory does not exist".to_string(),
        });
    }
    // `HEAD` first, so an unborn repository is named rather than surfacing as a
    // confusing failure from the diff itself.
    if !git_succeeds(dir, &["rev-parse", "--verify", "--quiet", "HEAD"])? {
        return Err(DiffError::NoHead {
            path: dir.display().to_string(),
        });
    }

    let tracked = run_git(dir, &["diff", "--no-color", "HEAD"])?.unwrap_or_default();
    let mut diff = parse(&tracked)?;

    let listed = run_git(dir, &["ls-files", "--others", "--exclude-standard"])?.unwrap_or_default();
    let untracked: Vec<&str> = listed
        .split('\n')
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    diff.hidden_untracked = untracked.len().saturating_sub(UNTRACKED_LIMIT);

    for file in untracked.iter().take(UNTRACKED_LIMIT) {
        // `--no-index` exits 1 when it found differences, which is the ordinary
        // case here: `run_git` tolerates that for this one command.
        let out = run_git(
            dir,
            &["diff", "--no-color", "--no-index", "--", "/dev/null", file],
        )?;
        let Some(out) = out else { continue };
        for parsed in parse(&out)?.files {
            diff.files.push(parsed);
        }
    }
    Ok(diff)
}

/// Whether `git` **succeeded**, for a command whose exit code *is* the answer.
///
/// Needed as its own function because [`run_git`] reads exit 1 as success-with-
/// output (the right rule for `diff`, which reports "there are differences" that
/// way). For `rev-parse --verify --quiet HEAD`, exit 1 means "no such revision" —
/// so asking `run_git` returned `Ok(Some(""))`, this module's own HEAD check was
/// bypassed, and the failure surfaced later as a confusing `git diff` error
/// instead of the sentence that names the real problem. Found by the unborn-HEAD
/// test.
fn git_succeeds(dir: &Path, args: &[&str]) -> Result<bool, DiffError> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| DiffError::Git {
            command: args.join(" "),
            cwd: dir.display().to_string(),
            detail: e.to_string(),
        })?;
    Ok(output.status.success())
}

/// Run `git` in `dir`, returning stdout — or `Ok(None)` when git exited 1, which
/// for `diff` means "differences found" rather than a failure.
fn run_git(dir: &Path, args: &[&str]) -> Result<Option<String>, DiffError> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| DiffError::Git {
            command: args.join(" "),
            cwd: dir.display().to_string(),
            detail: e.to_string(),
        })?;
    let code = output.status.code();
    if output.status.success() {
        return Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()));
    }
    // `git diff` and `git diff --no-index` report "there are differences" as exit
    // 1. That is this function's normal path, not an error.
    if code == Some(1) {
        return Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()));
    }
    Err(DiffError::Git {
        command: args.join(" "),
        cwd: dir.display().to_string(),
        detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

impl fmt::Display for Diff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.summary())
    }
}

/// Parse a unified diff.
///
/// Two phases rather than one stateful pass: the input is split into file
/// sections at `diff --git` boundaries, then each section is parsed on its own.
/// The split is what makes "which file am I in" impossible to get wrong — the
/// first version of this module tracked that in a live variable across the whole
/// scan, and a section that had not been created yet silently swallowed its own
/// headers.
pub fn parse(input: &str) -> Result<Diff, DiffError> {
    let lines = split_lines(input);
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("diff --git "))
        .map(|(index, _)| index)
        .collect();

    if let Some(first) = starts.first() {
        // Anything before the first header is not a diff. Checked rather than
        // skipped: a caller that handed us the wrong bytes should be told, not
        // handed an empty screen.
        if let Some((index, line)) = lines
            .iter()
            .enumerate()
            .take(*first)
            .find(|(_, line)| !line.trim().is_empty())
        {
            return Err(DiffError::ContentBeforeHeader {
                line: index + 1,
                text: (*line).to_string(),
            });
        }
    } else if let Some((index, line)) = lines.iter().enumerate().find(|(_, l)| !l.trim().is_empty())
    {
        return Err(DiffError::ContentBeforeHeader {
            line: index + 1,
            text: (*line).to_string(),
        });
    }

    let mut files = Vec::with_capacity(starts.len());
    for (n, start) in starts.iter().enumerate() {
        let end = starts.get(n + 1).copied().unwrap_or(lines.len());
        files.push(parse_section(&lines[*start..end], *start)?);
    }
    Ok(Diff {
        files,
        hidden_untracked: 0,
    })
}

/// Split a diff into lines **on `\n` only**.
///
/// Not `str::lines()`, and the difference is not cosmetic: `lines()` also splits
/// `\r\n`, so it **strips the CR** from a line's text. For a diff that is exactly
/// wrong — git writes its own line endings as `\n` and a CR inside a hunk line is
/// the *file's content* (a CRLF source file). Using `lines()` would have made this
/// parser silently disagree with the file it describes, which is the one thing a
/// reviewer cannot check by looking at the screen. Found by writing the CRLF test
/// and asking why it claimed to pass.
///
/// The trailing element a final `\n` produces is dropped: it is not a line.
fn split_lines(input: &str) -> Vec<&str> {
    let mut out: Vec<&str> = input.split('\n').collect();
    if out.last() == Some(&"") {
        out.pop();
    }
    out
}

/// Parse one section, beginning at its `diff --git` line.
///
/// `offset` is that line's index in the whole input, so error line numbers are
/// the caller's, not the section's.
fn parse_section(section: &[&str], offset: usize) -> Result<FileDiff, DiffError> {
    let mut file = FileDiff {
        old_path: String::new(),
        new_path: String::new(),
        change: Change::Modified,
        binary: false,
        hunks: Vec::new(),
    };
    // The header's two paths are a fallback: they are ambiguous when a name has a
    // space, and `---`/`+++` or the rename pair is authoritative. They are kept
    // because a mode-only change has no other source of names at all.
    if let Some(header) = section[0].strip_prefix("diff --git ") {
        if let Some((a, b)) = split_header_paths(header) {
            file.old_path = a;
            file.new_path = b;
        }
    }

    let mut index = 1;
    while index < section.len() {
        let raw = section[index];
        let line_no = offset + index + 1;

        if raw.starts_with("@@") {
            let (hunk, consumed) = parse_hunk(section, index, offset)?;
            file.hunks.push(hunk);
            index += consumed;
            continue;
        }

        if let Some(mode) = raw.strip_prefix("new file mode ") {
            file.change = Change::Added;
            let _ = mode;
        } else if let Some(mode) = raw.strip_prefix("deleted file mode ") {
            file.change = Change::Deleted;
            let _ = mode;
        } else if let Some(old) = raw.strip_prefix("old mode ") {
            file.change = Change::ModeChanged {
                old: old.to_string(),
                new: String::new(),
            };
        } else if let Some(new) = raw.strip_prefix("new mode ") {
            if let Change::ModeChanged { new: slot, .. } = &mut file.change {
                *slot = new.to_string();
            }
        } else if let Some(rest) = raw.strip_prefix("similarity index ") {
            let percent = rest.trim_end_matches('%').trim().parse::<u32>().ok();
            // The `rename`/`copy` line that follows picks the variant; the
            // percentage is attached to whichever it turns out to be.
            file.change = match file.change.clone() {
                Change::Renamed { from, to, .. } => Change::Renamed {
                    from,
                    to,
                    similarity: percent,
                },
                Change::Copied { from, to, .. } => Change::Copied {
                    from,
                    to,
                    similarity: percent,
                },
                _ => Change::Renamed {
                    from: String::new(),
                    to: String::new(),
                    similarity: percent,
                },
            };
        } else if let Some(from) = raw.strip_prefix("rename from ") {
            let (to, similarity) = match file.change.clone() {
                Change::Renamed { to, similarity, .. } => (to, similarity),
                _ => (String::new(), None),
            };
            file.change = Change::Renamed {
                from: from.to_string(),
                to,
                similarity,
            };
            // A rename names both paths on its own lines, so they win over the
            // header's ambiguous guess.
            file.old_path = from.to_string();
        } else if let Some(to) = raw.strip_prefix("rename to ") {
            let (from, similarity) = match file.change.clone() {
                Change::Renamed {
                    from, similarity, ..
                } => (from, similarity),
                _ => (String::new(), None),
            };
            file.change = Change::Renamed {
                from,
                to: to.to_string(),
                similarity,
            };
            file.new_path = to.to_string();
        } else if let Some(from) = raw.strip_prefix("copy from ") {
            file.change = Change::Copied {
                from: from.to_string(),
                to: String::new(),
                similarity: None,
            };
            file.old_path = from.to_string();
        } else if let Some(to) = raw.strip_prefix("copy to ") {
            let (from, similarity) = match file.change.clone() {
                Change::Copied {
                    from, similarity, ..
                } => (from, similarity),
                _ => (String::new(), None),
            };
            file.change = Change::Copied {
                from,
                to: to.to_string(),
                similarity,
            };
            file.new_path = to.to_string();
        } else if raw.starts_with("index ") || raw.starts_with("dissimilarity index ") {
            // Digests say nothing a reviewer reads.
        } else if raw == "GIT binary patch"
            || raw.starts_with("Binary files ")
            || raw == "Binary files differ"
        {
            // A payload follows (`GIT binary patch`) and is not text: skip the
            // rest of this section rather than parsing base85 as diff lines.
            file.binary = true;
            file.hunks.clear();
            return Ok(file);
        } else if let Some(path) = raw.strip_prefix("--- ") {
            let path = decode_path(path);
            file.old_path = if path == "/dev/null" {
                String::new()
            } else {
                path
            };
        } else if let Some(path) = raw.strip_prefix("+++ ") {
            let path = decode_path(path);
            file.new_path = if path == "/dev/null" {
                String::new()
            } else {
                path
            };
        } else {
            return Err(DiffError::UnexpectedLine {
                line: line_no,
                text: raw.to_string(),
            });
        }
        index += 1;
    }

    // The header's guess is only reported when nothing better arrived: a section
    // that has `---`/`+++` has already replaced it.
    if file.change == Change::Modified && file.old_path.is_empty() && file.new_path.is_empty() {
        // No names at all — leave both empty rather than invent one; `path()`
        // then renders nothing rather than a wrong path.
    }
    Ok(file)
}

/// Parse a hunk header plus its body, returning the hunk and how many lines of
/// the section it consumed.
fn parse_hunk(section: &[&str], start: usize, offset: usize) -> Result<(Hunk, usize), DiffError> {
    let header = section[start];
    let hunk_header = parse_hunk_header(header).ok_or_else(|| DiffError::MalformedHunkHeader {
        line: offset + start + 1,
        text: header.to_string(),
    })?;
    let mut hunk = Hunk {
        old_start: hunk_header.0,
        old_count: hunk_header.1,
        new_start: hunk_header.2,
        new_count: hunk_header.3,
        section: hunk_header.4,
        lines: Vec::new(),
    };

    let mut old_line = hunk.old_start;
    let mut new_line = hunk.new_start;
    let mut index = start + 1;
    while index < section.len() {
        let raw = section[index];
        // The four prefixes git emits. Anything else ends the hunk — the caller
        // either recognizes it as a header or refuses it loudly.
        let Some(prefix) = raw.chars().next() else {
            return Err(DiffError::UnexpectedLine {
                line: offset + index + 1,
                text: String::new(),
            });
        };
        match prefix {
            ' ' | '+' | '-' => {
                let kind = match prefix {
                    '+' => Kind::Added,
                    '-' => Kind::Removed,
                    _ => Kind::Context,
                };
                let text = raw[1..].to_string();
                let line = Line {
                    kind,
                    text,
                    old_line: (kind != Kind::Added).then_some(old_line),
                    new_line: (kind != Kind::Removed).then_some(new_line),
                    no_newline: false,
                };
                if kind != Kind::Added {
                    old_line += 1;
                }
                if kind != Kind::Removed {
                    new_line += 1;
                }
                hunk.lines.push(line);
            }
            '\\' => {
                // `\ No newline at end of file` describes the line **before** it.
                match hunk.lines.last_mut() {
                    Some(last) => last.no_newline = true,
                    None => {
                        return Err(DiffError::UnexpectedLine {
                            line: offset + index + 1,
                            text: raw.to_string(),
                        })
                    }
                }
            }
            _ => break,
        }
        index += 1;
    }
    Ok((hunk, index - start))
}

/// `@@ -old[,old_count] +new[,new_count] @@[ section]`.
///
/// Returns `(old_start, old_count, new_start, new_count, section)`. A single-line
/// range omits the count and means 1 — the convention `git diff` relies on, and
/// the reason a parser cannot read the counts as mandatory.
fn parse_hunk_header(text: &str) -> Option<(u32, u32, u32, u32, String)> {
    let rest = text.strip_prefix("@@ -")?;
    let (old, rest) = read_range(rest)?;
    let rest = rest.strip_prefix(" +")?;
    let (new, rest) = read_range(rest)?;
    let rest = rest.strip_prefix(" @@")?;
    // git writes a single space before the section, if there is one.
    let section = rest.strip_prefix(' ').unwrap_or(rest).to_string();
    Some((old.0, old.1, new.0, new.1, section))
}

/// Read `start[,count]`, returning both and the rest. A missing count means one
/// line.
fn read_range(text: &str) -> Option<((u32, u32), &str)> {
    let digits = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    if digits == 0 {
        return None;
    }
    let start: u32 = text[..digits].parse().ok()?;
    let rest = &text[digits..];
    if let Some(after_comma) = rest.strip_prefix(',') {
        let len = after_comma
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(after_comma.len());
        if len == 0 {
            return None;
        }
        let count: u32 = after_comma[..len].parse().ok()?;
        Some(((start, count), &after_comma[len..]))
    } else {
        Some(((start, 1), rest))
    }
}

/// Split the two paths out of a `diff --git` header's remainder.
///
/// **A fallback, and documented as one.** When neither path is quoted the header
/// is genuinely ambiguous for a name containing a space
/// (`diff --git a/my file.txt b/my file.txt`); this splits at the first ` b/`,
/// which is right for everything git emits in practice and is overridden by
/// `---`/`+++` or a rename pair. When both are quoted the split is unambiguous.
fn split_header_paths(header: &str) -> Option<(String, String)> {
    if header.starts_with('"') {
        let (first, rest) = take_quoted(header)?;
        let rest = rest.strip_prefix(' ')?;
        let (second, _) = take_quoted(rest)?;
        return Some((strip_side(&first), strip_side(&second)));
    }
    let split = header.find(" b/")?;
    let old = &header[..split];
    let new = &header[split + 1..];
    Some((strip_side(old), strip_side(new)))
}

/// Take one C-quoted string from the front, returning it unescaped and the rest.
fn take_quoted(text: &str) -> Option<(String, &str)> {
    let inner = text.strip_prefix('"')?;
    let mut bytes = Vec::new();
    let mut chars = inner.char_indices();
    while let Some((index, c)) = chars.next() {
        match c {
            '"' => {
                return Some((
                    String::from_utf8_lossy(&bytes).into_owned(),
                    &inner[index + 1..],
                ))
            }
            '\\' => {
                let (_, escaped) = chars.next()?;
                match escaped {
                    'n' => bytes.push(b'\n'),
                    't' => bytes.push(b'\t'),
                    'r' => bytes.push(b'\r'),
                    '\\' => bytes.push(b'\\'),
                    '"' => bytes.push(b'"'),
                    'a' => bytes.push(0x07),
                    'b' => bytes.push(0x08),
                    'f' => bytes.push(0x0c),
                    'v' => bytes.push(0x0b),
                    digit @ '0'..='7' => {
                        // Up to three octal digits, which are **bytes** — the
                        // reason this builds a Vec<u8> instead of pushing chars.
                        let mut value = digit.to_digit(8).unwrap_or(0);
                        let mut read = 1;
                        while read < 3 {
                            match inner
                                [chars.clone().next().map(|(i, _)| i).unwrap_or(inner.len())..]
                                .chars()
                                .next()
                            {
                                Some(next @ '0'..='7') => {
                                    value = value * 8 + next.to_digit(8).unwrap_or(0);
                                    chars.next();
                                    read += 1;
                                }
                                _ => break,
                            }
                        }
                        bytes.push(value as u8);
                    }
                    other => {
                        // An escape git does not use: keep it verbatim rather than
                        // inventing a byte.
                        bytes.push(b'\\');
                        let mut buf = [0u8; 4];
                        bytes.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
                    }
                }
            }
            other => {
                let mut buf = [0u8; 4];
                bytes.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    None
}

/// Decode a `---`/`+++` path: unquote if quoted, strip the trailing TAB git adds
/// when the name contains a space, and drop the `a/`/`b/` prefix.
fn decode_path(raw: &str) -> String {
    let raw = raw.strip_suffix('\t').unwrap_or(raw);
    if raw.starts_with('"') {
        return take_quoted(raw)
            .map(|(path, _)| strip_side(&path))
            .unwrap_or_else(|| strip_side(raw));
    }
    strip_side(raw)
}

/// Drop git's `a/` or `b/` side prefix.
///
/// One prefix, not all of them: a repository path that begins with `a/` is
/// written `a/a/whatever`, and stripping repeatedly would eat the real directory.
fn strip_side(path: &str) -> String {
    for side in ["a/", "b/"] {
        if let Some(rest) = path.strip_prefix(side) {
            return rest.to_string();
        }
    }
    path.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes this was written against, captured on this box (git 2.47.3).
    ///
    /// `include_str!` of the **raw** diff — the same artifact
    /// `.loop/evidence/T-0092/git-diff-bytes.txt` renders for a human — so "it
    /// parses what git prints" is checked against what git printed rather than
    /// against a hand-written approximation.
    const REAL: &str = include_str!("../../../.loop/evidence/T-0092/git-diff.raw");

    /// Every file in the captured diff parses, and each measured shape comes out
    /// right.
    #[test]
    fn parses_the_real_git_output_it_was_written_against() {
        let diff = parse(REAL).expect("the captured diff parses");
        assert_eq!(diff.files.len(), 12, "{:?}", paths(&diff));

        let by_path = |path: &str| {
            diff.files
                .iter()
                .find(|f| f.path() == path)
                .unwrap_or_else(|| panic!("{path} parsed; have {:?}", paths(&diff)))
        };

        // A new file: no old side, both lines added, correct new line numbers.
        let added = by_path("added.txt");
        assert_eq!(added.change, Change::Added);
        assert!(added.old_path.is_empty());
        assert_eq!(added.added(), 2);
        assert_eq!(added.removed(), 0);
        assert_eq!(
            added.hunks[0].lines[0],
            Line {
                kind: Kind::Added,
                text: "brand new file".to_string(),
                old_line: None,
                new_line: Some(1),
                no_newline: false,
            }
        );

        // A binary file: reported as binary with no hunks — not as "no changes".
        let binary = by_path("blob.bin");
        assert!(binary.binary, "blob.bin is binary");
        assert!(binary.hunks.is_empty());

        // A deletion: no new side, the old line named.
        let deleted = by_path("doomed.txt");
        assert_eq!(deleted.change, Change::Deleted);
        assert!(deleted.new_path.is_empty());
        assert_eq!(deleted.hunks[0].lines[0].old_line, Some(1));
        assert_eq!(deleted.hunks[0].lines[0].new_line, None);

        // A pure rename: no hunks at all, and both names from the rename pair.
        let renamed = by_path("renamed.txt");
        assert_eq!(
            renamed.change,
            Change::Renamed {
                from: "rename-me.txt".to_string(),
                to: "renamed.txt".to_string(),
                similarity: Some(100),
            }
        );
        assert!(renamed.hunks.is_empty(), "a 100% rename has no hunks");

        // CRLF content: the CR stays, because it is the file's content.
        let crlf = by_path("crlf.txt");
        let crlf_lines: Vec<&str> = crlf.hunks[0]
            .lines
            .iter()
            .map(|l| l.text.as_str())
            .collect();
        assert!(crlf_lines.contains(&"CRLF line\r"), "{crlf_lines:?}");
        assert!(crlf_lines.contains(&"second changed\r"), "{crlf_lines:?}");
    }

    /// The three path shapes that are not plain ASCII: a space, a quote, and
    /// non-ASCII — each with the tab and the C-quoting git actually emitted.
    #[test]
    fn unquotes_the_path_shapes_git_emits() {
        let diff = parse(REAL).expect("parses");
        let names: Vec<String> = diff.files.iter().map(|f| f.path().to_string()).collect();
        assert!(names.contains(&"my file.txt".to_string()), "{names:?}");
        assert!(names.contains(&"quote\"name.txt".to_string()), "{names:?}");
        assert!(names.contains(&"täb çhär.txt".to_string()), "{names:?}");
    }

    /// `\ No newline at end of file` describes the line before it — on both sides.
    #[test]
    fn the_no_newline_marker_lands_on_the_line_it_describes() {
        let diff = parse(REAL).expect("parses");
        let file = diff
            .files
            .iter()
            .find(|f| f.path() == "nonewline.txt")
            .expect("nonewline.txt");
        assert_eq!(file.hunks[0].lines.len(), 2);
        assert!(
            file.hunks[0].lines.iter().all(|l| l.no_newline),
            "{:?}",
            file.hunks[0].lines
        );
        // And the line numbers are the ones the file has.
        assert_eq!(file.hunks[0].lines[0].old_line, Some(1));
        assert_eq!(file.hunks[0].lines[1].new_line, Some(1));
    }

    /// A single-line range with no count means one line — the convention a parser
    /// that required both numbers would reject.
    #[test]
    fn a_range_without_a_count_means_one_line() {
        let diff =
            parse("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\n+new\n").unwrap();
        assert_eq!(diff.files[0].hunks[0].old_count, 1);
        assert_eq!(diff.files[0].hunks[0].new_count, 1);
        assert_eq!(diff.files[0].hunks[0].old_start, 1);
    }

    /// A hunk's section heading survives, and `header()` reconstructs the line.
    #[test]
    fn a_hunk_section_heading_round_trips() {
        let text =
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@ fn main() {\n a\n-b\n+c\n";
        let diff = parse(text).unwrap();
        let hunk = &diff.files[0].hunks[0];
        assert_eq!(hunk.section, "fn main() {");
        assert_eq!(hunk.header(), "@@ -1,2 +1,2 @@ fn main() {");
    }

    /// A mode-only change has no `---`/`+++` and no hunks: its names come from the
    /// header, which is the reason that fallback exists at all.
    #[test]
    fn a_mode_only_change_keeps_its_paths() {
        let text = "diff --git a/run.sh b/run.sh\nold mode 100644\nnew mode 100755\n";
        let diff = parse(text).unwrap();
        assert_eq!(
            diff.files[0].change,
            Change::ModeChanged {
                old: "100644".to_string(),
                new: "100755".to_string()
            }
        );
        assert_eq!(diff.files[0].path(), "run.sh");
        assert!(diff.files[0].hunks.is_empty());
    }

    /// Empty output is an empty diff, not an error: a clean tree is an answer.
    #[test]
    fn an_empty_input_is_an_empty_diff() {
        let diff = parse("").expect("empty parses");
        assert!(diff.is_empty());
        assert_eq!(diff.summary(), "0 files changed, +0 -0");
    }

    /// An unparseable line is refused with its number, so it is a bug report
    /// rather than a screen that looks like a diff.
    #[test]
    fn an_unrecognized_line_is_refused_with_its_number() {
        let text = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-old\nnot a diff line\n";
        let err = parse(text).expect_err("refused");
        assert_eq!(
            err,
            DiffError::UnexpectedLine {
                line: 6,
                text: "not a diff line".to_string()
            }
        );

        let header = parse("diff --git a/x b/x\n@@ not a header\n").expect_err("refused");
        assert!(matches!(
            header,
            DiffError::MalformedHunkHeader { line: 2, .. }
        ));

        let prose = parse("this is not a diff\n").expect_err("refused");
        assert!(matches!(
            prose,
            DiffError::ContentBeforeHeader { line: 1, .. }
        ));
    }

    /// The summary counts what a reviewer sees.
    #[test]
    fn the_summary_counts_files_and_lines() {
        let text = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,3 @@\n a\n-b\n+c\n+d\n";
        let diff = parse(text).unwrap();
        assert_eq!(diff.summary(), "1 file changed, +2 -1");
    }

    /// A scratch repository for the working-tree tests, under the repo's own
    /// scratch (never `/tmp`: it is a tmpfs here).
    fn scratch_repo(name: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/T-0092")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
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
        std::fs::write(dir.join("tracked.txt"), "first\nsecond\n").expect("write");
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "base"]);
        std::fs::canonicalize(&dir).expect("canonical")
    }

    /// **An untracked file is in the diff.** `git diff HEAD` cannot see it, and
    /// for this product that is the common case rather than a corner: an agent
    /// that wrote a new file and did not `git add` it would be invisible. This is
    /// the test that would fail if the untracked half were dropped.
    #[test]
    fn a_worktree_diff_includes_untracked_files() {
        let repo = scratch_repo("worktree-untracked");
        std::fs::write(
            repo.join("brand-new.txt"),
            "written by an agent\nline two\n",
        )
        .expect("write");

        let diff = worktree_diff(&repo).expect("diff");
        let file = diff
            .files
            .iter()
            .find(|f| f.path() == "brand-new.txt")
            .unwrap_or_else(|| panic!("untracked file in the diff: {:?}", paths(&diff)));
        assert_eq!(file.change, Change::Added);
        assert_eq!(file.added(), 2);
        assert_eq!(diff.hidden_untracked, 0);
    }

    /// Staged, unstaged and untracked arrive together, and a clean tree is empty.
    #[test]
    fn a_worktree_diff_covers_staged_unstaged_and_untracked() {
        let repo = scratch_repo("worktree-all");
        assert!(
            worktree_diff(&repo).expect("clean").is_empty(),
            "a clean tree has no diff"
        );

        // Unstaged.
        std::fs::write(repo.join("tracked.txt"), "first\nCHANGED\n").expect("write");
        // Staged, and a brand-new tracked file.
        std::fs::write(repo.join("staged.txt"), "staged content\n").expect("write");
        let stage = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["add", "staged.txt"])
            .output()
            .expect("git runs");
        assert!(stage.status.success());

        let diff = worktree_diff(&repo).expect("diff");
        let names = paths(&diff);
        assert!(names.contains(&"tracked.txt".to_string()), "{names:?}");
        assert!(names.contains(&"staged.txt".to_string()), "{names:?}");
        let tracked = diff
            .files
            .iter()
            .find(|f| f.path() == "tracked.txt")
            .expect("tracked.txt");
        assert_eq!(tracked.added(), 1);
        assert_eq!(tracked.removed(), 1);
    }

    /// The cap is reported, never silent — and the limit is reached, not exceeded.
    #[test]
    fn worktree_diff_reports_what_the_untracked_cap_hid() {
        let repo = scratch_repo("worktree-cap");
        for n in 0..(UNTRACKED_LIMIT + 3) {
            std::fs::write(repo.join(format!("many-{n:03}.txt")), "x\n").expect("write");
        }
        let diff = worktree_diff(&repo).expect("diff");
        assert_eq!(diff.files.len(), UNTRACKED_LIMIT);
        assert_eq!(diff.hidden_untracked, 3);
        assert!(
            diff.summary().contains("3 more untracked files not shown"),
            "{}",
            diff.summary()
        );
    }

    /// A directory that is not a repository, and one with no commits, are both
    /// named rather than surfacing as an empty diff.
    #[test]
    fn a_repository_with_no_commits_is_named() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-scratch/T-0092/unborn");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch");
        let run = |args: &[&str]| {
            let _ = std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .expect("git runs");
        };
        run(&["init", "-q", "-b", "main"]);
        let err = worktree_diff(&dir).expect_err("unborn HEAD");
        assert!(matches!(err, DiffError::NoHead { .. }), "{err}");

        let missing = dir.join("not-here");
        let err = worktree_diff(&missing).expect_err("missing directory");
        assert!(matches!(err, DiffError::Git { .. }), "{err}");
    }

    fn paths(diff: &Diff) -> Vec<String> {
        diff.files.iter().map(|f| f.path().to_string()).collect()
    }
}
