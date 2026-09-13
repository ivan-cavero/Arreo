//! T-0020: `xtask package` (supply-chain gate) + `bench --probe size`.
//!
//! `package --dry-run` (plan mode): validates the cargo-dist skeleton
//! without building — parses `[workspace.metadata.dist]`, asserts the three
//! release targets + both installers are declared, and reports what a tag
//! build WOULD produce. Real builds happen on tags in CI only.
//!
//! `bench --probe size`: builds `--release` (cached) and asserts the daemon
//! binary ≤ the `daemon_binary_mb` budget row. Slow on cold cache (release
//! link); CI nightly covers it, PRs cover `--dry-run`.

use std::path::PathBuf;
use std::process::ExitCode;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .to_path_buf()
}

const EXPECTED_TARGETS: &[&str] = &[
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
    "aarch64-apple-darwin",
];

const EXPECTED_INSTALLERS: &[&str] = &["shell", "powershell"];

pub fn package(rest: &[String]) -> ExitCode {
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: xtask package --dry-run");
        println!("  validates the cargo-dist skeleton (plan mode); real builds run on tags in CI.");
        return ExitCode::SUCCESS;
    }
    if !rest.iter().any(|a| a == "--dry-run") {
        eprintln!("package: only --dry-run exists locally (real builds run on tags in CI)");
        return ExitCode::from(2);
    }
    let manifest = std::fs::read_to_string(workspace_root().join("Cargo.toml")).unwrap_or_default();
    let mut failed = false;
    for target in EXPECTED_TARGETS {
        if manifest.contains(target) {
            println!("[PASS] package: target {target} declared");
        } else {
            println!("[FAIL] package: target {target} missing from [workspace.metadata.dist]");
            failed = true;
        }
    }
    for installer in EXPECTED_INSTALLERS {
        if manifest.contains(&format!("\"{installer}\"")) {
            println!("[PASS] package: installer {installer} declared");
        } else {
            println!("[FAIL] package: installer {installer} missing");
            failed = true;
        }
    }
    // Binaries that dist would ship must exist as targets.
    for bin in ["arreo-server", "arreo"] {
        let found = [
            "crates/arreo-server/Cargo.toml",
            "crates/arreo-cli/Cargo.toml",
        ]
        .iter()
        .any(|path| {
            std::fs::read_to_string(workspace_root().join(path))
                .map(|text| text.contains(&format!("name = \"{bin}\"")))
                .unwrap_or(false)
        });
        if found {
            println!("[PASS] package: binary {bin} present");
        } else {
            println!("[FAIL] package: binary {bin} missing");
            failed = true;
        }
    }
    signed_release(&mut failed);
    if failed {
        ExitCode::FAILURE
    } else {
        println!("package: skeleton valid (real builds on tags; see docs/release.md)");
        ExitCode::SUCCESS
    }
}

/// T-0036: the signed-release structure, checked on every PR rather than on the
/// first tag.
///
/// Two claims are machine-checkable here and both have failed silently before in
/// other projects:
///
/// * **The pinned key is a real key.** A placeholder public key is worse than
///   none — the release job would verify against a key nobody holds and refuse
///   every artifact, a gate that looks green and can never pass. So the key is
///   parsed, its key id is read, and the file's own comment must agree with it.
/// * **The tag job exists and is the one that ships.** A workflow that builds but
///   does not sign, or signs but never verifies, or verifies with something other
///   than the shipped verifier, is a release pipeline that produces untrusted
///   artifacts while looking complete. The assertions below are the acceptance
///   criteria written where CI can see them; `docs/release.md` records what they
///   cannot prove (the job has never run against a real key).
fn signed_release(failed: &mut bool) {
    let root = workspace_root();

    let key_text = std::fs::read_to_string(root.join("supply-chain/arreo.pub")).unwrap_or_default();
    match arreo_core::update::verify::TrustSet::parse(&key_text) {
        Some(keys) => {
            let ids = keys.ids();
            // Every comment in the file must name the key it introduces: a
            // rotation that edits the blob and forgets the comment is a file
            // whose reader cannot tell which key is which.
            let comments: Vec<&str> = key_text
                .lines()
                .filter(|line| line.starts_with("untrusted comment:"))
                .collect();
            let named = ids
                .split(", ")
                .all(|id| comments.iter().any(|line| line.contains(id)));
            let placeholder = ids
                .split(", ")
                .any(|id| matches!(id, "0000000000000000" | "FFFFFFFFFFFFFFFF"));
            if placeholder {
                println!(
                    "[FAIL] package: supply-chain/arreo.pub holds a placeholder key ({ids}) — \
                     every release would fail closed against a key nobody holds"
                );
                *failed = true;
            } else if named {
                println!("[PASS] package: pinned trust set {ids} (every comment names its key)");
            } else {
                println!(
                    "[FAIL] package: a key in supply-chain/arreo.pub has no comment naming it \
                     ({ids})"
                );
                *failed = true;
            }
        }
        None => {
            println!(
                "[FAIL] package: supply-chain/arreo.pub is not a minisign public key — this \
                 build would have no trust anchor"
            );
            *failed = true;
        }
    }

    let workflow =
        std::fs::read_to_string(root.join(".github/workflows/release.yml")).unwrap_or_default();
    let requirements: &[(&str, &str)] = &[
        ("tags:", "the release job runs on tags"),
        (
            "MINISIGN_SECRET_KEY",
            "signing takes the key from the environment secret",
        ),
        (
            "secrets.MINISIGN_SECRET_KEY",
            "the workflow reads the key from the secret store, not from a file",
        ),
        ("SHA256SUMS", "the manifest is written and signed"),
        (".minisig", "signatures ship beside the artifacts"),
        (
            "arreo update verify",
            "verification goes through the shipped verifier, not a second path",
        ),
        ("tampered", "a tampered copy is refused before publishing"),
    ];
    for (needle, why) in requirements {
        if workflow.contains(needle) {
            println!("[PASS] package: release job — {why}");
        } else {
            println!("[FAIL] package: release.yml is missing {needle:?} ({why})");
            *failed = true;
        }
    }

    // No key material in the workflow. A minisign secret key blob is a long
    // base64 run; anything that long in a workflow is either a leaked key or a
    // mistake, and neither belongs in a file that ships to a public repository.
    let long_base64 = workflow
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='))
        .any(|run| run.len() >= 80 && !run.chars().all(|c| c.is_ascii_digit()));
    if long_base64 {
        println!("[FAIL] package: release.yml contains what looks like embedded key material");
        *failed = true;
    } else {
        println!("[PASS] package: release.yml carries no key material");
    }
}

/// `bench --probe size`: release-build the daemon and assert the budget.
/// Cached: skips the rebuild when the existing release binary is newer than
/// all workspace sources (keeps PR runs fast; nightly builds cold).
pub fn probe_size(json: bool, target_mb: u64) -> ExitCode {
    let root = workspace_root();
    let daemon_bin = root.join("target").join("release").join("arreo-server");
    let needs_build = !daemon_bin.exists() || {
        let bin_time = std::fs::metadata(&daemon_bin)
            .and_then(|m| m.modified())
            .ok();
        let newest_src = workspace_newest_mtime(&root);
        match (bin_time, newest_src) {
            (Some(bin), Some(src)) => src > bin,
            _ => true,
        }
    };
    if needs_build {
        println!("bench --probe size: release build (cold cache, one-time cost)...");
        let status = std::process::Command::new("cargo")
            .args([
                "build",
                "--release",
                "-p",
                "arreo-server",
                "-p",
                "arreo-cli",
            ])
            .current_dir(&root)
            .status();
        match status {
            Ok(status) if status.success() => {}
            _ => {
                eprintln!("bench --probe size: release build failed");
                return ExitCode::FAILURE;
            }
        }
    } else {
        println!("bench --probe size: using cached release binary");
    }
    let bytes = std::fs::metadata(&daemon_bin).map(|m| m.len()).unwrap_or(0);
    let mb = bytes as f64 / 1_048_576.0;
    let pass = (bytes <= target_mb * 1_048_576) && bytes > 0;
    if json {
        println!("{{\"probe\":\"size\",\"bytes\":{bytes},\"mb\":{mb:.1},\"target_mb\":{target_mb},\"pass\":{pass}}}");
    } else {
        println!(
            "bench --probe size: arreo-server release {mb:.1} MB (target ≤ {target_mb} MB) → {}",
            if pass { "PASS" } else { "FAIL" }
        );
    }
    if pass {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn workspace_newest_mtime(root: &std::path::Path) -> Option<std::time::SystemTime> {
    let mut newest: Option<std::time::SystemTime> = None;
    let mut stack = vec![root.join("crates"), root.join("xtask")];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(dir).ok()?;
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs" || e == "toml") {
                if let Ok(modified) = entry.metadata().and_then(|m| m.modified()) {
                    newest =
                        Some(newest.map_or(modified, |n: std::time::SystemTime| n.max(modified)));
                }
            }
        }
    }
    newest
}
