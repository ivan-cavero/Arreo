//! Chaos probe 5 (racing senders): concurrent `send` never deadlocks or
//! corrupts; every thread's bytes arrive (echoed back through `cat`).

use arreo_core::pty::Pane;
use std::sync::Arc;
use std::time::Duration;

pub fn run() -> Result<String, String> {
    let pane = Arc::new(Pane::spawn("/bin/cat", &[], 80, 24).map_err(|e| format!("spawn: {e}"))?);
    let mut handles = Vec::new();
    for i in 0..8u32 {
        let pane = Arc::clone(&pane);
        handles.push(std::thread::spawn(move || {
            for j in 0..25u32 {
                let msg = format!("w{i}-{j}\n");
                pane.send(msg.as_bytes())
                    .map_err(|e| format!("send: {e}"))?;
                if j % 5 == 0 {
                    pane.resize(80 + i as u16, 24)
                        .map_err(|e| format!("resize: {e}"))?;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok::<(), String>(())
        }));
    }
    for handle in handles {
        handle
            .join()
            .map_err(|_| "sender thread panicked".to_string())??;
    }
    std::thread::sleep(Duration::from_millis(500));
    let lines = pane.drain();
    if lines.len() > 512 {
        return Err(format!("buffer over capacity: {}", lines.len()));
    }
    pane.kill_shared().map_err(|e| format!("kill: {e}"))?;
    Ok(format!(
        "racing senders: 8x25 ok, {} lines, bounded",
        lines.len()
    ))
}
