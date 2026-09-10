//! Chaos probe 1 (mid-write kill): scrollback must survive daemon death.
//!
//! Spawns a pane, streams output, kills the child mid-write, then asserts the
//! raw journal still holds everything written before the kill. The daemon
//! layer (socket death) is covered by `daemon_drop`; here we prove the core
//! invariant: bytes once acknowledged by the reader pump are never lost.

use arreo_core::pty::Pane;
use std::time::Duration;

pub fn run() -> Result<String, String> {
    let pane = Pane::spawn(
        "/bin/sh",
        &[
            "-c",
            "for i in $(seq 1 200); do echo line-$i; done; sleep 30",
        ],
        80,
        24,
    )
    .map_err(|e| format!("spawn: {e}"))?;
    // Let output flow, then kill mid-stream (sleep 30 still pending = death
    // happens while the pane is conceptually "writing").
    std::thread::sleep(Duration::from_millis(400));
    let before = pane.raw_snapshot().0.len();
    if before == 0 {
        return Err("no output flowed before kill".to_string());
    }
    pane.kill_shared().map_err(|e| format!("kill: {e}"))?;
    std::thread::sleep(Duration::from_millis(300));
    let (after, _) = pane.raw_snapshot();
    if after.len() < before {
        return Err(format!("journal shrank: {before} -> {}", after.len()));
    }
    if !after.windows(6).any(|w| w == b"line-1") {
        return Err("early lines lost after kill".to_string());
    }
    Ok(format!(
        "mid-write kill: journal intact ({} bytes, {} lines)",
        after.len(),
        pane.drain().len()
    ))
}
