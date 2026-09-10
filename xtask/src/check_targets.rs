//! T-0010: layered portability gate (see `check_targets` for the model).

use std::process::ExitCode;

/// Layer 1 (local, minutes): `cargo check --workspace --target <t>` with the
/// rustup std for that target. Catches cfg/type errors in OUR code with zero
/// SDK downloads. Layer 2 (CI runners): real build+test+behavior per OS.
/// Full xwin/osxcross SDKs deferred (see docs/cross-os.md) — when a target's
/// check fails ONLY inside a C build script (missing linker/libs), the target
/// reports SKIP with the reason instead of fake-red; any rustc error in our
/// code FAILS.
///
/// Exit 0: all targets PASS or SKIP. Exit 1: any target FAIL, or (with
/// `--enforce`) any target SKIP — CI pre-merge runs `--enforce` once the SDK
/// steps land; until then CI runs without it and the matrix job is authority.
pub fn check_targets(rest: &[String]) -> ExitCode {
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: xtask check-targets [--enforce] [--targets t1,t2]");
        println!("  default targets: x86_64-unknown-linux-gnu, x86_64-pc-windows-msvc");
        println!("  darwin targets need an osxcross SDK (see docs/cross-os.md); absent = SKIP.");
        return ExitCode::SUCCESS;
    }
    let enforce = rest.iter().any(|a| a == "--enforce");
    let targets: Vec<String> = rest
        .windows(2)
        .find(|w| w[0] == "--targets")
        .map(|w| w[1].split(',').map(str::to_string).collect())
        .unwrap_or_else(|| {
            vec![
                "x86_64-unknown-linux-gnu".to_string(),
                "x86_64-pc-windows-msvc".to_string(),
            ]
        });
    let mut failed = Vec::new();
    let mut skipped = Vec::new();
    for target in &targets {
        match check_one(target) {
            TargetResult::Pass => println!("check-targets: {target} PASS"),
            TargetResult::Skip(reason) => {
                println!("check-targets: {target} SKIP ({reason})");
                skipped.push(target.clone());
            }
            TargetResult::Fail(reason) => {
                println!("check-targets: {target} FAIL ({reason})");
                failed.push(target.clone());
            }
        }
    }
    if !failed.is_empty() {
        eprintln!("check-targets: FAILED targets: {}", failed.join(", "));
        return ExitCode::FAILURE;
    }
    if enforce && !skipped.is_empty() {
        eprintln!(
            "check-targets: --enforce given but SKIPPED targets remain: {}",
            skipped.join(", ")
        );
        return ExitCode::FAILURE;
    }
    if skipped.is_empty() {
        println!("check-targets: all {} targets PASS", targets.len());
    } else {
        println!(
            "check-targets: {} pass, {} skip (CI matrix is authority for skipped)",
            targets.len() - skipped.len(),
            skipped.len()
        );
    }
    ExitCode::SUCCESS
}

enum TargetResult {
    Pass,
    Skip(String),
    Fail(String),
}

/// Markers proving the failure is a missing SDK or linker, NOT our code.
const SDK_MARKERS: &[&str] = &[
    "lib.exe",
    "failed to find tool",
    "failed to run custom build command",
    "cannot find -l",
    "osxcross",
    "xwin",
    "SDKROOT",
    "MacOSX.sdk",
];

fn check_one(target: &str) -> TargetResult {
    // Two passes: full workspace first (real product surface); if that fails
    // ONLY on a C build script, retry the C-free surface: `arreo-core` with
    // `--no-default-features` drops the `sqlite` feature (its only C dep),
    // proving all pure-Rust code is portable. (Workspace-wide
    // `--no-default-features` cannot work: dependents' default edges
    // re-enable `sqlite` via feature unification — cargo semantics, verified
    // during T-0010 development.)
    let full = cargo_check_workspace(target);
    if full.success {
        return TargetResult::Pass;
    }
    if has_rustc_error(&full.stderr) {
        return TargetResult::Fail(fail_line(&full.stderr));
    }
    if !is_sdk_failure(&full.stderr) {
        return TargetResult::Fail(unclassified(&full.stderr));
    }
    let lite = cargo_check_lite(target);
    if lite.success {
        return TargetResult::Skip(format!(
            "pure-Rust (arreo-core, no sqlite) PASS; C deps need SDK ({}) — CI covers",
            first_marker(&full.stderr)
        ));
    }
    if has_rustc_error(&lite.stderr) {
        return TargetResult::Fail(fail_line(&lite.stderr));
    }
    TargetResult::Fail(unclassified(&lite.stderr))
}

/// Real portability bug = rustc errors pointing at OUR files.
fn has_rustc_error(stderr: &str) -> bool {
    stderr.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with("error[") || (line.starts_with("error:") && line.contains("-->"))
    })
}

fn fail_line(stderr: &str) -> String {
    let first = stderr
        .lines()
        .find(|l| l.trim_start().starts_with("error"))
        .unwrap_or("unknown rustc error");
    format!("rustc error: {}", first.trim())
}

fn is_sdk_failure(stderr: &str) -> bool {
    SDK_MARKERS.iter().any(|m| stderr.contains(m))
}

fn first_marker(stderr: &str) -> &str {
    SDK_MARKERS
        .iter()
        .find(|m| stderr.contains(*m))
        .unwrap_or(&"SDK")
}

fn unclassified(stderr: &str) -> String {
    format!(
        "unclassified check failure (first line: {})",
        stderr.lines().next().unwrap_or("(empty)").trim()
    )
}

struct CheckOutput {
    success: bool,
    stderr: String,
}

fn cargo_check_workspace(target: &str) -> CheckOutput {
    run_check(&["check", "--workspace", "--all-targets", "--target", target])
}

/// C-free surface: arreo-core without the `sqlite` feature (its only C dep).
/// `-p` form because workspace-wide `--no-default-features` is defeated by
/// dependents' default edges (feature unification).
fn cargo_check_lite(target: &str) -> CheckOutput {
    run_check(&[
        "check",
        "-p",
        "arreo-core",
        "--no-default-features",
        "--all-targets",
        "--target",
        target,
    ])
}

fn run_check(args: &[&str]) -> CheckOutput {
    let mut cmd = std::process::Command::new("cargo");
    cmd.args(args);
    match cmd.output() {
        Ok(output) => CheckOutput {
            success: output.status.success(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        Err(e) => CheckOutput {
            success: false,
            stderr: format!("error: could not run cargo check: {e}"),
        },
    }
}
