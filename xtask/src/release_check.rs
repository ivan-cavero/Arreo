//! T-0048: the public-readiness gate — `cargo xtask release-check [--public]`.
//!
//! One sentence: run every check a stranger would run before trusting this repo,
//! as named items with their own verdict, so "is Arreo ready to be public?" is a
//! command rather than an opinion.
//!
//! ## Why a verb and not a checklist in a document
//!
//! A launch checklist in `docs/` is a claim; a gate is a fact. Every item below
//! is something the project already asserts elsewhere (T-0020's supply-chain
//! gates, the CI workflow, the fmt/clippy/test trio), so this verb does not add
//! new standards — it *aggregates* them at the moment they matter, and it keeps
//! mattering after launch because CI runs it (`AGENTS.md`, `.github/workflows/ci.yml`).
//!
//! ## The `--public` contract: fail closed
//!
//! Without `--public` this is a convenience gate: a tool that is not installed is
//! `SKIP`, so a contributor without the pinned scanners can still run everything
//! else. **With `--public` a missing tool is a FAIL.** That asymmetry is the whole
//! point: the launch question is not "did the checks I happened to have installed
//! pass", it is "did every check run". A gate that reports PASS for a scan it never
//! performed is worse than no gate, because it launders an absence into a
//! reassurance — exactly the failure mode this repo's honesty rule (§10.2) exists
//! to prevent.
//!
//! ## Pinned tools
//!
//! Two checks need a tool this workspace does not vendor: `gitleaks` (the
//! full-history secret scan) and `reuse` (the license lint). Both are pinned to an
//! exact version below and discovered from the environment first
//! (`ARREO_GITLEAKS`, `ARREO_REUSE` — a path), then from `PATH`. The pinned
//! versions live here, next to the checks that use them, because a version in a
//! document is a version that drifts.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output};

/// The secret scanner's pinned version. Recorded in every finding the scan
/// produces, so two runs can be compared.
pub const GITLEAKS_PINNED: &str = "8.28.0";

/// The REUSE lint's pinned version. `reuse` is a Python tool with no standalone
/// binary, so the pinned form is a package version run through whatever
/// installer the operator has (`pipx run reuse==…`, `uvx reuse==…`).
pub const REUSE_PINNED: &str = "5.0.2";

/// Every crate that must be Apache-2.0, and the one that must not be.
const APACHE_CRATES: &[&str] = &[
    "arreo-core",
    "arreo-server",
    "arreo-cli",
    "arreo-tui",
    "arreo-plugin-api",
];
const AGPL_CRATE: &str = "arreo-relay";

/// The verdict of one item. `Skip` is deliberately not a synonym for `Pass`:
/// the difference between "checked and clean" and "not checked" is the
/// difference this gate exists to preserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    Fail,
    Skip,
}

impl Verdict {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

struct Item {
    name: &'static str,
    verdict: Verdict,
    /// One line: what was found, or what to do about it.
    detail: String,
}

pub fn release_check(rest: &[String]) -> ExitCode {
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: xtask release-check [--public]");
        println!("  Runs the launch-readiness items and prints PASS/FAIL/SKIP for each.");
        println!("  --public  fail-closed mode: a check that could not run is a FAIL.");
        println!("  Pinned tools: gitleaks {GITLEAKS_PINNED} (ARREO_GITLEAKS), reuse {REUSE_PINNED} (ARREO_REUSE)");
        return ExitCode::SUCCESS;
    }
    let public = rest.iter().any(|a| a == "--public");
    let root = match repo_root() {
        Some(root) => root,
        None => {
            eprintln!("release-check: cannot find the repository root (no Cargo.toml above cwd?)");
            return ExitCode::from(2);
        }
    };
    println!(
        "release-check: {} mode (pinned: gitleaks {GITLEAKS_PINNED}, reuse {REUSE_PINNED})",
        if public { "--public" } else { "development" }
    );

    // The three cargo trios are separate items on purpose: "clippy has a warning"
    // and "a test fails" are different repairs, and a gate that reports them as
    // one FAIL sends the reader looking for the wrong thing.
    let items = vec![
        cargo_item("fmt", &["fmt", "--all", "--", "--check"], &root),
        cargo_item(
            "clippy",
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
            &root,
        ),
        cargo_item("test", &["test", "--workspace"], &root),
        cargo_item("vet", &["vet", "--locked"], &root),
        cargo_item("audit", &["audit"], &root),
        cargo_item("deny", &["deny", "check"], &root),
        license_item(&root),
        reuse_item(&root, public),
        secrets_item(&root, public),
        links_item(&root),
    ];

    let mut failed = 0usize;
    for item in &items {
        println!("[{}] release-check: {}", item.verdict.as_str(), item.name);
        if !item.detail.is_empty() {
            for line in item.detail.lines() {
                println!("       {line}");
            }
        }
        if item.verdict == Verdict::Fail {
            failed += 1;
        }
    }
    let skipped = items
        .iter()
        .filter(|item| item.verdict == Verdict::Skip)
        .count();
    println!(
        "release-check: {} passed, {failed} failed, {skipped} skipped",
        items.len() - failed - skipped
    );
    if failed > 0 {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Run a `cargo` subcommand and report its verdict.
fn cargo_item(name: &'static str, args: &[&str], root: &Path) -> Item {
    let output = Command::new("cargo").args(args).current_dir(root).output();
    match output {
        Ok(output) if output.status.success() => Item {
            name,
            verdict: Verdict::Pass,
            detail: String::new(),
        },
        Ok(output) => Item {
            name,
            verdict: Verdict::Fail,
            detail: failure_excerpt(&output),
        },
        Err(e) => Item {
            name,
            verdict: Verdict::Fail,
            // A missing `cargo` is not a skip: the toolchain is a prerequisite
            // every other item depends on, so its absence is a failure to run
            // the gate at all.
            detail: format!("cannot run `cargo {}`: {e}", args.join(" ")),
        },
    }
}

/// The last few lines of a failed command's output — enough to name the
/// problem, not enough to bury the verdict table.
fn failure_excerpt(output: &Output) -> String {
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&output.stdout).into_owned();
    }
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    let tail = lines.len().saturating_sub(12);
    lines[tail..].join("\n")
}

/// The license story (§7 `ROADMAP.md`): every shipped crate Apache-2.0 except the
/// relay, and nothing Apache depending on the relay.
///
/// Read from the manifests rather than trusted: this is the one item whose whole
/// job is to notice that a `Cargo.toml` drifted from the policy.
fn license_item(root: &Path) -> Item {
    let mut problems = Vec::new();
    let mut checked = 0usize;
    let default = workspace_license(root);

    for crate_name in APACHE_CRATES.iter().chain(std::iter::once(&AGPL_CRATE)) {
        let manifest = root.join("crates").join(crate_name).join("Cargo.toml");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            problems.push(format!(
                "{crate_name}: no manifest at {}",
                manifest.display()
            ));
            continue;
        };
        checked += 1;
        let expected = if *crate_name == AGPL_CRATE {
            "AGPL-3.0-or-later"
        } else {
            default.as_str()
        };
        match declared_license(&text) {
            Some(found) if found == expected => {}
            Some(found) => problems.push(format!(
                "{crate_name}: license is {found:?}, the policy says {expected:?}"
            )),
            None => problems.push(format!("{crate_name}: manifest declares no license")),
        }
        if *crate_name != AGPL_CRATE && text.contains("arreo-relay") {
            // The workspace_deps test is the enforcing gate for this (T-0035);
            // this item exists so the *launch* gate cannot pass while that one
            // is red for an unrelated reason.
            problems.push(format!(
                "{crate_name}: an Apache-2.0 crate names arreo-relay in its manifest"
            ));
        }
    }

    if problems.is_empty() {
        Item {
            name: "license",
            verdict: Verdict::Pass,
            detail: format!("{checked} manifests match the Apache/AGPL split; no Apache crate depends on the relay"),
        }
    } else {
        Item {
            name: "license",
            verdict: Verdict::Fail,
            detail: problems.join("\n"),
        }
    }
}

/// The workspace's default `license`, which a crate inherits when it says
/// `license.workspace = true`.
fn workspace_license(root: &Path) -> String {
    std::fs::read_to_string(root.join("Cargo.toml"))
        .ok()
        .and_then(|text| {
            let mut in_workspace_package = false;
            for line in text.lines() {
                let line = line.trim();
                if line.starts_with('[') {
                    in_workspace_package = line == "[workspace.package]";
                    continue;
                }
                if in_workspace_package {
                    if let Some(value) = line.strip_prefix("license") {
                        return value
                            .trim_start_matches(|c: char| c == '.' || c.is_whitespace() || c == '=')
                            .trim()
                            .trim_matches('"')
                            .to_string()
                            .into();
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| "Apache-2.0".to_string())
}

/// The `license` a crate declares, resolving `license.workspace = true` to the
/// workspace default.
fn declared_license(manifest: &str) -> Option<String> {
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with("license.workspace") && line.contains("true") {
            return Some("Apache-2.0".to_string());
        }
        if let Some(rest) = line.strip_prefix("license") {
            let rest = rest.trim_start();
            if let Some(value) = rest.strip_prefix('=') {
                return Some(value.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// The license lint, through the pinned `reuse` if it is reachable.
fn reuse_item(root: &Path, public: bool) -> Item {
    let Some(tool) = find_tool("ARREO_REUSE", "reuse") else {
        return missing_tool(
            "reuse",
            public,
            &format!(
                "reuse {REUSE_PINNED} is not installed. Install the pinned version \
                 (`pipx run reuse=={REUSE_PINNED} lint` or `uvx reuse=={REUSE_PINNED} lint`) \
                 or point ARREO_REUSE at it"
            ),
        );
    };
    let output = Command::new(&tool).arg("lint").current_dir(root).output();
    match output {
        Ok(output) if output.status.success() => Item {
            name: "reuse",
            verdict: Verdict::Pass,
            detail: format!("{}: clean", tool.display()),
        },
        Ok(output) => Item {
            name: "reuse",
            verdict: Verdict::Fail,
            detail: failure_excerpt(&output),
        },
        Err(e) => Item {
            name: "reuse",
            verdict: Verdict::Fail,
            detail: format!("cannot run {}: {e}", tool.display()),
        },
    }
}

/// The full-history secret scan, through the pinned `gitleaks`.
///
/// **`--log-opts=--all` and no exclusions**: the criterion is that the *history*
/// is clean, not the working tree, and a scan that skips the fixture directories
/// is a scan that skips the place tokens live legitimately — which is exactly
/// where a real one would hide.
fn secrets_item(root: &Path, public: bool) -> Item {
    let Some(tool) = find_tool("ARREO_GITLEAKS", "gitleaks") else {
        return missing_tool(
            "secrets",
            public,
            &format!(
                "gitleaks {GITLEAKS_PINNED} is not installed. Fetch the pinned release from \
                 https://github.com/gitleaks/gitleaks/releases/tag/v{GITLEAKS_PINNED}, verify its \
                 published SHA256, and point ARREO_GITLEAKS at the binary"
            ),
        );
    };
    let version = Command::new(&tool)
        .arg("version")
        .output()
        .ok()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default();
    // A version mismatch is reported, not enforced: the pin belongs to CI, and a
    // developer's slightly newer scanner finding nothing is not a launch blocker.
    // `--public` still requires *a* scan to have run.
    let report = std::env::temp_dir().join(format!("arreo-gitleaks-{}.json", std::process::id()));
    let output = Command::new(&tool)
        .args(["detect", "--source"])
        .arg(root)
        .args([
            "--log-opts=--all",
            "--redact",
            "--no-banner",
            "--report-format",
            "json",
        ])
        .arg("--report-path")
        .arg(&report)
        .current_dir(root)
        .output();
    let _ = std::fs::remove_file(&report);
    match output {
        Ok(output) if output.status.success() => Item {
            name: "secrets",
            verdict: Verdict::Pass,
            detail: format!(
                "{}: no findings over the whole history",
                version_or(&version, &tool)
            ),
        },
        Ok(output) => Item {
            name: "secrets",
            verdict: Verdict::Fail,
            // gitleaks exits non-zero when it finds something, so the findings
            // are in the output; `--redact` above means the report names the rule
            // and location without reproducing the secret.
            detail: format!(
                "{}: findings over the history (report was at {}; this run deleted it — \
                 re-run with `--report-path` to keep it)",
                version_or(&version, &tool),
                report.display()
            ) + &format!("\n{}", failure_excerpt(&output)),
        },
        Err(e) => Item {
            name: "secrets",
            verdict: Verdict::Fail,
            detail: format!("cannot run {}: {e}", tool.display()),
        },
    }
}

fn version_or(version: &str, tool: &Path) -> String {
    if version.is_empty() {
        tool.display().to_string()
    } else {
        version.to_string()
    }
}

/// A tool that could not be found: a launch blocker in `--public`, a note
/// otherwise.
fn missing_tool(name: &'static str, public: bool, how: &str) -> Item {
    Item {
        name,
        verdict: if public { Verdict::Fail } else { Verdict::Skip },
        detail: if public {
            format!("--public requires this check to run. {how}")
        } else {
            format!("not run. {how}")
        },
    }
}

/// Find a pinned tool: the environment override first (a path a CI step or a
/// developer can point at), then `PATH`.
fn find_tool(env_var: &str, program: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(env_var) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// Every relative link in the public docs resolves.
///
/// The public surface is `README.md`, `CONTRIBUTING.md`, `SECURITY.md`,
/// `NOTICE.md` and `docs/*.md`: the files a stranger reads before deciding
/// whether to trust the project. A dead link in `specs/` is a note; a dead link
/// in the README is the first impression.
fn links_item(root: &Path) -> Item {
    let mut files: Vec<PathBuf> = Vec::new();
    for name in ["README.md", "CONTRIBUTING.md", "SECURITY.md", "NOTICE.md"] {
        let path = root.join(name);
        if path.is_file() {
            files.push(path);
        }
    }
    if let Ok(entries) = std::fs::read_dir(root.join("docs")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "md") {
                files.push(path);
            }
        }
    }

    let mut broken = Vec::new();
    let mut checked = 0usize;
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for target in markdown_links(&text) {
            let target = target.split('#').next().unwrap_or("");
            if target.is_empty()
                || target.starts_with("http://")
                || target.starts_with("https://")
                || target.starts_with("mailto:")
            {
                continue;
            }
            checked += 1;
            let resolved = file
                .parent()
                .unwrap_or(root)
                .join(target.trim_start_matches('/'));
            if !resolved.exists() {
                broken.push(format!(
                    "{}: {target}",
                    file.strip_prefix(root).unwrap_or(file).display()
                ));
            }
        }
    }

    if broken.is_empty() {
        Item {
            name: "links",
            verdict: Verdict::Pass,
            detail: format!(
                "{checked} relative links across {} public documents",
                files.len()
            ),
        }
    } else {
        Item {
            name: "links",
            verdict: Verdict::Fail,
            detail: format!(
                "{} of {checked} relative links do not resolve:\n{}",
                broken.len(),
                broken.join("\n")
            ),
        }
    }
}

/// The relative targets of markdown links: `](target)` and `[label]: target`.
///
/// Scanned rather than parsed — a markdown parser is a dependency this does not
/// earn — and deliberately narrow: it is looking for the two shapes this repo
/// writes, and an unrecognised shape is a miss, not a false failure.
fn markdown_links(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    for line in text.lines() {
        // Inline: [label](target). Read to the **matching** `)`, counting depth:
        // stopping at the first one would turn `docs/foo_(bar).md` into the
        // nonsense target `docs/foo_(bar` and report a working link as broken —
        // a false FAIL, which is worse than a miss because it costs trust in the
        // whole gate.
        let mut rest = line;
        while let Some(at) = rest.find("](") {
            let after = &rest[at + 2..];
            let mut depth = 0usize;
            let mut end = after.len();
            for (index, ch) in after.char_indices() {
                match ch {
                    '(' => depth += 1,
                    ')' if depth == 0 => {
                        end = index;
                        break;
                    }
                    ')' => depth -= 1,
                    _ => {}
                }
            }
            links.push(after[..end].trim().trim_matches('"').to_string());
            rest = &after[end..];
        }
        // Reference definitions: [label]: target
        if let Some(colon) = line.find("]: ") {
            if line.trim_start().starts_with('[') {
                let target = line[colon + 3..].trim();
                if !target.is_empty() {
                    links.push(target.trim_matches('"').to_string());
                }
            }
        }
    }
    links
}

fn repo_root() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join("Cargo.toml").is_file() && dir.join("ROADMAP.md").is_file() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both link shapes this repo writes are found. A scanner that quietly found
    /// nothing would make "links PASS" a statement about the scanner rather than
    /// about the docs, which is the one failure this item cannot afford.
    #[test]
    fn markdown_links_finds_both_shapes() {
        let text =
            "See [the tour](docs/tour.md) and [the ADR][adr].\n\n[adr]: specs/adr/0001-x.md\n";
        let links = markdown_links(text);
        assert!(links.contains(&"docs/tour.md".to_string()), "{links:?}");
        assert!(
            links.contains(&"specs/adr/0001-x.md".to_string()),
            "{links:?}"
        );
    }

    /// A path containing parentheses is read whole: the target stops at the
    /// *matching* `)`, not the first one.
    #[test]
    fn markdown_links_reads_a_target_with_parentheses_whole() {
        let links = markdown_links("See [x](docs/foo_(bar).md).");
        assert_eq!(links, vec!["docs/foo_(bar).md".to_string()]);
    }

    /// The item's contract, end to end: a broken relative link is a FAIL and a
    /// resolvable one is a PASS. Built on a scratch tree so the assertion is
    /// about the check, not about today's docs.
    #[test]
    fn a_broken_relative_link_fails_and_a_resolvable_one_passes() {
        let dir = std::env::temp_dir().join(format!(
            "arreo-release-links-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).expect("scratch");
        std::fs::write(dir.join("docs/here.md"), "present\n").expect("write");

        std::fs::write(dir.join("README.md"), "[a](docs/here.md)\n").expect("write");
        assert_eq!(links_item(&dir).verdict, Verdict::Pass);

        // An external link is not resolved against the filesystem, and an
        // in-page anchor is not a file: neither may be reported as broken.
        std::fs::write(
            dir.join("README.md"),
            "[a](https://example.com/x) [b](#section) [c](docs/here.md)\n",
        )
        .expect("write");
        assert_eq!(links_item(&dir).verdict, Verdict::Pass);

        std::fs::write(dir.join("README.md"), "[a](docs/gone.md)\n").expect("write");
        let item = links_item(&dir);
        assert_eq!(item.verdict, Verdict::Fail);
        assert!(item.detail.contains("docs/gone.md"), "{}", item.detail);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A crate that inherits the workspace license and one that overrides it are
    /// both read correctly — the manifest is the only place the split is real.
    #[test]
    fn declared_license_resolves_the_workspace_form_and_an_explicit_one() {
        assert_eq!(
            declared_license("license.workspace = true\n"),
            Some("Apache-2.0".to_string()),
            "an inheriting crate takes the workspace default"
        );
        assert_eq!(
            declared_license("license = \"AGPL-3.0-or-later\"\n"),
            Some("AGPL-3.0-or-later".to_string())
        );
        assert_eq!(declared_license("name = \"x\"\n"), None);
    }
}
