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
    if failed {
        ExitCode::FAILURE
    } else {
        println!("package: skeleton valid (real builds on tags; see docs/release.md)");
        ExitCode::SUCCESS
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
