//! T-0028 compat slice: the N−1 window over the real socket, no mocks.
//!
//! Thin runner: the compat suites in `arreo-core/tests/compat.rs` (matrix over
//! frozen corpora) and `arreo-server/tests/compat.rs` (both directions against
//! a live daemon) ARE the suite. This slice runs them via `cargo test` and
//! reports counts, so CI gets one command — the same shape as the api slice
//! (T-0014).

use std::process::ExitCode;

pub fn run(_rest: &[String]) -> ExitCode {
    let mut failures = 0usize;
    let mut passes = 0usize;
    for (label, args) in [
        (
            "matrix",
            vec!["test", "-p", "arreo-core", "--test", "compat"],
        ),
        (
            "daemon",
            vec!["test", "-p", "arreo-server", "--test", "compat"],
        ),
    ] {
        let output = std::process::Command::new("cargo").args(&args).output();
        match output {
            Ok(output) => {
                let text = String::from_utf8_lossy(&output.stdout).to_string()
                    + &String::from_utf8_lossy(&output.stderr);
                let ok =
                    output.status.success() && text.lines().any(|l| l.contains("test result: ok"));
                for line in text
                    .lines()
                    .filter(|l| l.starts_with("test ") || l.contains("test result"))
                {
                    println!("compat[{label}]: {line}");
                }
                if ok {
                    passes += 1;
                } else {
                    eprintln!("compat[{label}]: FAILURES present");
                    failures += 1;
                }
            }
            Err(e) => {
                eprintln!("compat[{label}]: could not run cargo test: {e}");
                failures += 1;
            }
        }
    }
    if failures == 0 {
        println!("compat: N−1 window green in both directions (matrix + live daemon)");
        ExitCode::SUCCESS
    } else {
        eprintln!("compat: {failures} suite(s) failed, {passes} passed");
        ExitCode::FAILURE
    }
}
