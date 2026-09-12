//! T-0009 chaos suite: break the spike on purpose, report every finding.
//!
//! Probes (each a module): mid-write kill, resize spam, giant line, binary
//! garbage, racing senders, attach disconnect, deterministic fuzz. Each
//! returns `Ok(evidence)` or `Err(finding)`; the runner prints a table and
//! exits non-zero if ANY probe fails. `--slice chaos` selects this suite
//! (the only slice today); `--list` names the probes.

use std::process::ExitCode;

mod attach_disconnect;
mod binary_garbage;
mod fuzz_corpus;
mod giant_line;
mod mesh_reconnect;
mod mid_write_kill;
mod racing_senders;
mod resize_spam;

type Probe = (&'static str, fn() -> Result<String, String>);

const PROBES: &[Probe] = &[
    ("mid-write-kill", mid_write_kill::run),
    ("resize-spam", resize_spam::run),
    ("giant-line", giant_line::run),
    ("binary-garbage", binary_garbage::run),
    ("racing-senders", racing_senders::run),
    ("attach-disconnect", attach_disconnect::run),
    ("fuzz-corpus", fuzz_corpus::run),
    ("mesh-reconnect", mesh_reconnect::run),
];

pub fn run(rest: &[String]) -> ExitCode {
    if rest.iter().any(|a| a == "--list") {
        for (name, _) in PROBES {
            println!("{name}");
        }
        return ExitCode::SUCCESS;
    }
    let filter = rest
        .windows(2)
        .find(|w| w[0] == "--probe")
        .map(|w| w[1].as_str());
    let mut failed = 0usize;
    let mut passed = 0usize;
    for (name, probe) in PROBES {
        if let Some(filter) = filter {
            if *name != filter {
                continue;
            }
        }
        let start = std::time::Instant::now();
        match probe() {
            Ok(evidence) => {
                println!("[PASS] {name} ({:?}): {evidence}", start.elapsed());
                passed += 1;
            }
            Err(finding) => {
                println!("[FAIL] {name} ({:?}): {finding}", start.elapsed());
                failed += 1;
            }
        }
    }
    println!("chaos: {passed} passed, {failed} failed");
    if failed > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
