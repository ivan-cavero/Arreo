//! T-0014 api slice: every verb over the real socket, no mocks.
//!
//! Thin runner: the integration tests in `arreo-server/tests/api.rs` ARE the
//! suite (real daemon, temp sockets, framed MessagePack). This slice runs
//! them via `cargo test` and reports counts, so CI gets one command.

use std::process::ExitCode;

pub fn run(_rest: &[String]) -> ExitCode {
    let output = std::process::Command::new("cargo")
        .args(["test", "-p", "arreo-server", "--test", "api"])
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
                println!("api: all verbs green over the real socket");
                ExitCode::SUCCESS
            } else {
                eprintln!("api: FAILURES present");
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("api: could not run cargo test: {e}");
            ExitCode::FAILURE
        }
    }
}
