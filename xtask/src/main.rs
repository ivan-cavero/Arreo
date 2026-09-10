//! Arreo developer tooling: e2e battery, benchmarks, smoke tests.
//!
//! Verbs: `e2e` (battery, T-0008+), `bench` (budgets, T-0008), `conpty-smoke`
//! (T-0007, real), `check-targets` (T-0010, real). By contract (T-0001 notes):
//! unimplemented verbs print "not implemented" and exit 0 — except with
//! `--enforce`, which exits non-zero so CI gates distinguish "stub" from "gate".

use std::process::ExitCode;
use std::time::{Duration, Instant};

mod api_slice;
mod bench;
mod chaos;
mod check_targets;
mod demo;
mod lifecycle_slice;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (cmd, rest) = match args.split_first() {
        Some((first, rest)) => (first.as_str(), rest),
        None => {
            eprintln!("usage: xtask <e2e|bench|conpty-smoke|check-targets> [options]");
            return ExitCode::from(2);
        }
    };
    match cmd {
        "e2e" => e2e(rest),
        "bench" => bench::bench(rest),
        "demo" => demo::demo(rest),
        "conpty-smoke" => conpty_smoke(rest),
        "check-targets" => check_targets::check_targets(rest),
        other => {
            eprintln!("unknown xtask command: {other}");
            eprintln!("usage: xtask <e2e|bench|conpty-smoke|check-targets> [options]");
            ExitCode::from(2)
        }
    }
}

fn e2e(rest: &[String]) -> ExitCode {
    let slice = rest
        .windows(2)
        .find(|w| w[0] == "--slice")
        .map(|w| w[1].as_str());
    match slice {
        Some("chaos") => chaos::run(rest),
        Some("api") => api_slice::run(rest),
        Some("lifecycle") => lifecycle_slice::run(rest),
        Some(other) => {
            eprintln!("xtask e2e: unknown slice {other:?} (have: chaos)");
            ExitCode::from(2)
        }
        None => stub("e2e", rest),
    }
}

fn stub(cmd: &str, rest: &[String]) -> ExitCode {
    let enforce = rest.iter().any(|a| a == "--enforce");
    println!("xtask {cmd}: not implemented (real harness lands in T-0008/T-0021)");
    if enforce {
        eprintln!("xtask {cmd}: --enforce given but no budgets exist yet; failing");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// T-0007: ConPTY smoke (Windows) / portable-pty smoke (unix proxy).
///
/// On Windows this spawns `cmd /c dir` through ConPTY (the native backend —
/// `native_pty_system()` IS `ConPtySystem` there), forces the UTF-8 codepage,
/// resizes, asserts accented output, kills, and checks the reap. On unix it
/// runs the same Pane mechanics via `sh` (backend is posix_openpt; the API
/// under test is identical — backend selection is portable-pty's job, proven
/// by the type alias, not by us).
///
/// Exit 0 = smoke passed. `--enforce` is accepted (CI passes it) and behaves
/// identically — the smoke is real, not a stub, so there is no stub/gate
/// distinction for this verb.
fn conpty_smoke(rest: &[String]) -> ExitCode {
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: xtask conpty-smoke [--timeout-secs N]");
        println!("  Windows: cmd/dir/UTF-8/resize/kill via ConPTY.");
        println!("  unix:    same Pane mechanics via sh (local proxy; CI Windows run is the ConPTY proof).");
        return ExitCode::SUCCESS;
    }
    let timeout_secs: u64 = rest
        .windows(2)
        .find(|w| w[0] == "--timeout-secs")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(30);
    let timeout = Duration::from_secs(timeout_secs);
    match smoke(timeout) {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("conpty-smoke FAILED: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(windows)]
fn smoke(timeout: Duration) -> Result<String, String> {
    use std::io::Write;
    // Force UTF-8 codepage first: ConPTY inherits the console codepage, and
    // legacy conhost defaults to a local 8-bit page that mangles multibyte.
    let output = std::process::Command::new("cmd")
        .args(["/c", "chcp 65001 >NUL && echo codepage-ok"])
        .output()
        .map_err(|e| format!("chcp probe failed: {e}"))?;
    if !output.status.success() {
        return Err("chcp 65001 failed — UTF-8 codepage unavailable".to_string());
    }
    let _ = std::io::stdout().write_all(b"");
    run_pane_smoke(
        "cmd",
        &[
            "/c",
            "dir && echo SMOKE-DIR-OK && chcp 65001 >NUL && echo h\u{e9}llo w\u{f6}rld \u{2713}",
        ],
        "SMOKE-DIR-OK",
        "h\u{e9}llo",
        timeout,
    )
}

#[cfg(not(windows))]
fn smoke(timeout: Duration) -> Result<String, String> {
    run_pane_smoke(
        "/bin/sh",
        &[
            "-c",
            "ls / && echo SMOKE-DIR-OK && printf 'héllo wörld ✓\n'",
        ],
        "SMOKE-DIR-OK",
        "héllo",
        timeout,
    )
}

fn run_pane_smoke(
    program: &str,
    args: &[&str],
    dir_marker: &str,
    utf8_marker: &str,
    timeout: Duration,
) -> Result<String, String> {
    let deadline = Instant::now() + timeout;
    let remaining = || deadline.saturating_duration_since(Instant::now());
    // Pane lives in arreo-core; xtask depends on it (dev-only, never shipped).
    let pane = arreo_core::pty::Pane::spawn(program, args, 80, 24)
        .map_err(|e| format!("spawn failed: {e}"))?;
    let pid = pane.child_pid().unwrap_or(0);
    // 1. Directory listing arrives (backend holds a session).
    wait_for(&pane, dir_marker, remaining(), "dir output")?;
    // 2. Resize propagates to the kernel.
    pane.resize(100, 30)
        .map_err(|e| format!("resize failed: {e}"))?;
    let size = pane.size().map_err(|e| format!("size failed: {e}"))?;
    if size != (100, 30) {
        return Err(format!("resize mismatch: got {size:?}, want (100, 30)"));
    }
    // 3. UTF-8 round-trips (codepage forced on Windows before spawn).
    wait_for(&pane, utf8_marker, remaining(), "utf-8 marker")?;
    // 4. Child reaps (no zombie console / no zombie process).
    match pane.wait_timeout(remaining()) {
        Some(arreo_core::pty::ExitState::Exited(_)) => {}
        other => return Err(format!("child did not reap cleanly: {other:?}")),
    }
    Ok(format!(
        "conpty-smoke: PASS backend={} pid={pid} resize=100x30 utf8=ok reap=ok",
        std::env::consts::OS,
    ))
}

fn wait_for(
    pane: &arreo_core::pty::Pane,
    needle: &str,
    timeout: Duration,
    what: &str,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if pane.drain().iter().any(|l| l.contains(needle)) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("timed out waiting for {what} ({needle:?})"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
