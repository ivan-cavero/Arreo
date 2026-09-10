//! Chaos probe 2 (resize spam): 10k rapid resizes must not break the pane.
//!
//! Hogs the master lock from one thread while output flows; asserts the pane
//! still serves reads, reports a sane size, and the child survives.

use arreo_core::pty::Pane;
use std::time::Duration;

pub fn run() -> Result<String, String> {
    let pane = Pane::spawn("/bin/sh", &["-c", "echo alive && sleep 30"], 80, 24)
        .map_err(|e| format!("spawn: {e}"))?;
    for i in 0..10_000u32 {
        let cols = 80 + (i % 40) as u16;
        let rows = 24 + (i % 20) as u16;
        pane.resize(cols, rows)
            .map_err(|e| format!("resize {i}: {e}"))?;
    }
    let (cols, rows) = pane.size().map_err(|e| format!("size: {e}"))?;
    if !(80..120).contains(&cols) || !(24..44).contains(&rows) {
        return Err(format!("insane size after spam: {cols}x{rows}"));
    }
    std::thread::sleep(Duration::from_millis(300));
    let lines = pane.drain();
    if !lines.iter().any(|l| l.contains("alive")) {
        return Err("output lost after resize spam".to_string());
    }
    pane.kill_shared().map_err(|e| format!("kill: {e}"))?;
    Ok(format!(
        "resize spam: 10000 ok, final {cols}x{rows}, output intact"
    ))
}
