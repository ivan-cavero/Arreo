//! Arreo developer tooling: e2e battery, benchmarks, smoke tests.
//!
//! T-0001: runnable stubs. Real harnesses land in T-0007 (conpty-smoke),
//! T-0008 (bench), T-0021 (demo). By contract (T-0001 notes): without
//! `--enforce` the stubs print "not implemented" and exit 0; with `--enforce`
//! they exit non-zero so CI gates can distinguish "stub" from "gate".

use std::process::ExitCode;

const NOT_IMPLEMENTED: &str = "not implemented";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = match args.split_first() {
        Some((first, rest)) => (first.as_str(), rest),
        None => {
            eprintln!("usage: xtask <e2e|bench|conpty-smoke> [options]");
            return ExitCode::from(2);
        }
    };
    let enforce = rest.iter().any(|a| a == "--enforce");
    match cmd {
        "e2e" | "bench" | "conpty-smoke" => {
            println!("xtask {cmd}: {NOT_IMPLEMENTED} (real harness lands in T-0007/T-0008/T-0021)");
            if enforce {
                eprintln!("xtask {cmd}: --enforce given but no budgets exist yet; failing");
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        other => {
            eprintln!("unknown xtask command: {other}");
            eprintln!("usage: xtask <e2e|bench|conpty-smoke> [options]");
            ExitCode::from(2)
        }
    }
}
