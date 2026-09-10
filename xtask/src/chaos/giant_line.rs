//! Chaos probe 3 (giant line): a 5 MB single line must stay bounded.
//!
//! Feeds through both the ring buffer and the VT pane; asserts the ≤ 3 MB
//! per-pane budget holds and truncation is counted, not silent.

use arreo_core::pty::RingBuffer;
use arreo_core::vt::VtPane;

pub fn run() -> Result<String, String> {
    let mut ring = RingBuffer::new(512);
    ring.push_bytes(&vec![b'G'; 5 * 1024 * 1024]);
    ring.push_bytes(b"\n");
    ring.flush_partial();
    let held = ring.bytes_held();
    if held > 3 * 1024 * 1024 {
        return Err(format!("ring over budget: {held} bytes"));
    }
    if ring.dropped_bytes() == 0 {
        return Err("giant-line truncation not counted".to_string());
    }
    let mut vt = VtPane::new(80, 24);
    vt.feed(&vec![b'H'; 5 * 1024 * 1024]);
    vt.feed(b"\n");
    if vt.ram_bytes() > 3 * 1024 * 1024 {
        return Err(format!("vt over budget: {} bytes", vt.ram_bytes()));
    }
    if vt.dropped_partial_bytes() == 0 {
        return Err("vt giant-line drop not counted".to_string());
    }
    Ok(format!(
        "giant line: ring {held}B + vt {}B, drops counted",
        vt.ram_bytes()
    ))
}
