//! T-0014 api slice: every verb over the real socket, no mocks.
//!
//! Thin runner: the integration tests in `arreo-server/tests/api.rs` ARE the
//! suite (real daemon, temp sockets, framed MessagePack). This slice runs
//! them via `cargo test` and reports counts, so CI gets one command.
//!
//! `--test notify` runs beside them (T-0093). The notification engine's own rule
//! is unit-tested in `arreo-core`, but what the api slice is for is the **socket
//! and the log**: the notify tests drive a real daemon and read the audit rows it
//! wrote, which is the same referee this slice already uses. A daemon-side feature
//! whose rows nobody executes is a claim, not a gate.
//!
//! The notify suite's quick-action tests (T-0094) drive the **CLI** — the
//! contract's single door is `arreo notify act …` — so this slice builds the
//! `arreo` binary first: `cargo test -p arreo-server` alone would not, and a
//! missing binary would fail those tests with a build hint instead of running
//! them.

use std::process::ExitCode;

/// The test binaries that make up this slice, in the order they run.
const SUITES: &[&str] = &["api", "notify"];

pub fn run(_rest: &[String]) -> ExitCode {
    // The notify tests spawn `target/debug/arreo` for the act verb (T-0094);
    // build it up front so the slice is self-contained, not dependent on a prior
    // `cargo test --workspace` having built every binary.
    let built = std::process::Command::new("cargo")
        .args(["build", "-p", "arreo-cli", "--quiet"])
        .output();
    match built {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            eprintln!(
                "api: the CLI did not build — the quick-action tests need it: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("api: could not build the CLI for the quick-action tests: {e}");
            return ExitCode::FAILURE;
        }
    }
    let mut failed = Vec::new();
    let mut total = 0usize;
    for suite in SUITES {
        let output = std::process::Command::new("cargo")
            .args(["test", "-p", "arreo-server", "--test", suite])
            .output();
        match output {
            Ok(output) => {
                let text = String::from_utf8_lossy(&output.stdout).to_string()
                    + &String::from_utf8_lossy(&output.stderr);
                let passed = text.lines().any(|l| l.contains("test result: ok"));
                for line in text
                    .lines()
                    .filter(|l| l.starts_with("test ") || l.contains("test result"))
                {
                    println!("api: {line}");
                }
                if output.status.success() && passed {
                    total += 1;
                } else {
                    failed.push(*suite);
                }
            }
            Err(e) => {
                eprintln!("api: could not run cargo test for {suite}: {e}");
                return ExitCode::FAILURE;
            }
        }
    }
    if failed.is_empty() {
        println!(
            "api: all verbs green over the real socket ({total} suites: {})",
            SUITES.join(", ")
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("api: FAILURES present in {}", failed.join(", "));
        ExitCode::FAILURE
    }
}
