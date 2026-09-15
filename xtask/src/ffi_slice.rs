//! T-0125 ffi slice: the real daemon behind the exported FFI surface.
//!
//! Thin runner: `crates/arreo-core-ffi/tests/real_daemon.rs` IS the check (it
//! spawns `arreo-relay serve` and `arreo-server`, pairs a viewer and an owner
//! through the real `arreo` CLI, spawns panes and drives the exported surface),
//! and this slice is what runs it and decides what the run means.
//!
//! ## Why the check is not a unit test
//!
//! T-0114 shipped a green, wrong tree: every check the battery ran was a unit
//! test or a fixture, and **the fixture was not a daemon** — it closed its
//! session after each answer, so the one case a real machine produces on the
//! second read was never exercised. A property that only real processes exhibit
//! belongs in an xtask slice, and this is that slice: the same judgment as the
//! handoff-abort slice and T-0039's deferred case.
//!
//! The driver is `#[ignore]`d (it needs three built binaries, a QUIC listener
//! and ~40 s of real sampling), so `cargo test --workspace` stays a unit suite
//! and this verb is the gate.
//!
//! ## SKIP, and the evidence gate
//!
//! A missing binary or a relay this machine cannot run is a **SKIP naming what
//! is missing** — the `check-targets` pattern, and `--enforce` turns it into a
//! failure for CI. And a PASS is only a PASS when the run's own summary says it
//! read real metrics (`reads`, `rows_min`, `tier_ms`, `empty_rows`, `acts`),
//! which this re-checks: a green run that read nothing would be the green-and-
//! wrong tree all over again.
//!
//! ## What is not linked
//!
//! Nothing: the relay and the daemon are spawned as processes. The relay is AGPL
//! (no Apache work may link it) and the daemon is the machine under test, so the
//! harness must not be able to call into either — the same rule the CLI's own
//! real-process tests follow.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;

/// The binaries the driver spawns, in the order a failure names them.
const BINARIES: &[&str] = &["arreo-relay", "arreo-server", "arreo"];

/// How the driver is run: its own integration test, ignored by default.
const DRIVER: &[&str] = &[
    "test",
    "-p",
    "arreo-core-ffi",
    "--test",
    "real_daemon",
    "--",
    "--ignored",
    "--nocapture",
];

/// The tiers `arreo_core::store::metrics_retention` keeps. A read that named
/// anything else would be the ask echoed back rather than the tier served.
const REAL_TIERS: [u64; 3] = [10_000, 60_000, 3_600_000];

pub fn run(rest: &[String]) -> ExitCode {
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: xtask e2e --slice ffi [--enforce]");
        println!(
            "  Spawns the real `arreo-relay serve`, `arreo-server` and `arreo` binaries, pairs a"
        );
        println!(
            "  viewer and an owner through the real CLI, and drives the exported FFI surface:"
        );
        println!(
            "  metrics read three times on one conversation, a wrong-but-valid pinned key, an"
        );
        println!("  empty window, and the act door. No AGPL code is linked.");
        println!(
            "  SKIP (not FAIL) when the binaries or the relay cannot run; --enforce fails instead."
        );
        return ExitCode::SUCCESS;
    }
    let enforce = rest.iter().any(|a| a == "--enforce");

    // The check is unix-only: unix sockets, an owner-only identity store, a pty
    // pane. Its file is `#![cfg(unix)]`, so the cross-target gate still compiles
    // it (as nothing) everywhere.
    if !cfg!(unix) {
        return skip(
            enforce,
            "the real-daemon check is unix-only (unix sockets, an owner-only identity store, a pty pane)",
        );
    }

    let debug = debug_dir();
    let missing: Vec<String> = BINARIES
        .iter()
        .map(|name| debug.join(name))
        .filter(|path| !path.exists())
        .map(|path| path.display().to_string())
        .collect();
    if !missing.is_empty() {
        return skip(
            enforce,
            &format!(
                "missing {} — build them first: \
                 `cargo build -p arreo-cli -p arreo-server -p arreo-relay`",
                missing.join(", ")
            ),
        );
    }

    let output = match std::process::Command::new("cargo").args(DRIVER).output() {
        Ok(output) => output,
        Err(e) => return fail(&format!("cargo could not start the driver: {e}")),
    };
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    report(&text, output.status.success(), enforce)
}

/// Decide what the run means, and echo the evidence it carries.
fn report(text: &str, ok: bool, enforce: bool) -> ExitCode {
    if ok {
        // The driver's own lines are the evidence; the rest is cargo's noise.
        for line in text
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("ffi-slice:") || l.starts_with("test result:"))
        {
            println!("ffi: {line}");
        }
    } else {
        // A red run is pasted into a report, so it is echoed verbatim.
        println!("ffi: --- the driver's output, verbatim ---");
        for line in text.lines() {
            println!("ffi: {line}");
        }
        println!("ffi: --- end of the driver's output ---");
        return fail(&failure_line(text).unwrap_or_else(|| {
            "the driver failed without naming a check (see the output above)".to_string()
        }));
    }

    if let Some(summary) = summary_line(text) {
        return match verify(&summary) {
            Ok(()) => {
                println!("ffi: PASS — {summary}");
                ExitCode::SUCCESS
            }
            Err(why) => fail(&format!(
                "the driver reported a pass without the evidence: {why} ({summary})"
            )),
        };
    }
    if let Some(reason) = skip_line(text) {
        return skip(enforce, &reason);
    }
    fail("the driver reported neither a pass nor a skip")
}

/// The evidence gate: a PASS is only a PASS if the run read real metrics.
///
/// The numbers are the driver's own; re-checking them here is what makes "the
/// check ran and found nothing to read" unable to cross as a pass.
fn verify(summary: &str) -> Result<(), String> {
    let mut fields = BTreeMap::new();
    for token in summary.split_whitespace() {
        let (key, value) = token
            .split_once('=')
            .ok_or_else(|| format!("{token:?} is not key=value"))?;
        fields.insert(key.to_string(), value.to_string());
    }
    let number = |key: &str| -> Result<u64, String> {
        fields
            .get(key)
            .ok_or_else(|| format!("{key} is missing"))?
            .parse::<u64>()
            .map_err(|e| format!("{key}: {e}"))
    };
    if number("reads")? < 2 {
        return Err("fewer than two reads on the one session".to_string());
    }
    if number("rows_min")? < 1 {
        return Err("a read returned no rows: the metrics were not real".to_string());
    }
    let tier = number("tier_ms")?;
    if !REAL_TIERS.contains(&tier) {
        return Err(format!("tier_ms={tier} is not a tier the store keeps"));
    }
    if number("empty_rows")? != 0 {
        return Err("the empty window was not empty".to_string());
    }
    if number("acts")? < 2 {
        return Err("fewer than two acts reached the machine".to_string());
    }
    Ok(())
}

/// `ffi-slice: PASS reads=… rows_min=…` — the driver's terminal summary.
fn summary_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("ffi-slice: PASS "))
        .map(str::to_string)
}

/// `ffi-slice: SKIP (…)` — the driver naming what this machine cannot run.
fn skip_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix("ffi-slice: SKIP ("))
        .map(|rest| rest.trim_end_matches(')').to_string())
}

/// The named check a red run failed on — the sentence T-0114's M1' produced,
/// rather than "the slice failed".
fn failure_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| line.starts_with("ffi-slice: ") && line.contains(" FAIL — "))
        .map(str::to_string)
}

fn skip(enforce: bool, reason: &str) -> ExitCode {
    println!("ffi: SKIP ({reason})");
    if enforce {
        eprintln!("ffi: --enforce given, but the real-daemon check did not run");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn fail(reason: &str) -> ExitCode {
    eprintln!("ffi: FAIL — {reason}");
    ExitCode::FAILURE
}

fn debug_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the workspace root")
        .join("target")
        .join("debug")
}
