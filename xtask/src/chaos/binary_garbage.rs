//! Chaos probe 4 (binary garbage): VT parser + state engine must not panic,
//! hang, or corrupt history on hostile bytes.

use arreo_core::state::{Adapter, Engine};
use arreo_core::vt::VtPane;

const GARBAGE_CASES: &[&[u8]] = &[
    b"\x00\xff\xfe\x1b[Z\x07\x00",
    b"\x1b[\x1b[\x1b[99999999999999999999X",
    b"\x9b\x9c\x9d\x90hello\x9c",
    b"\x1b]0;\x07\x1b]999;",
    b"\xc3\x28\xe2\x28\xa0\xf0\x28\x8c\x28",
    b"\x1b[?1049h\x1b[?1049h\x1b[?1049l\x1b[?1049l",
    b"\r\r\r\n\n\n\x00\x00\x07\x07\x07",
    b"\x1b[38;2;999;999;999mcolor\x1b[0m",
];

pub fn run() -> Result<String, String> {
    let mut vt = VtPane::new(80, 24);
    vt.feed(b"clean-marker\n");
    let mut engine = Engine::new(Adapter::default(), 0);
    let mut t = 0u64;
    for (i, case) in GARBAGE_CASES.iter().enumerate() {
        vt.feed(case);
        for event in engine.feed(case, t) {
            let _ = event;
        }
        t += 100;
        // Interleave clean output: parser must resync, never wedge.
        vt.feed(b"after-garbage\n");
        for event in engine.feed(b"after-garbage\n", t) {
            let _ = event;
        }
        t += 100;
        let _ = i;
    }
    vt.spill_to_disk().map_err(|e| format!("spill: {e}"))?;
    let page = vt.page(0, 1).map_err(|e| format!("page: {e}"))?;
    if page.first().map(String::as_str) != Some("clean-marker") {
        return Err(format!("history corrupted: {page:?}"));
    }
    if vt.ram_bytes() > 3 * 1024 * 1024 {
        return Err("vt ram blown by garbage".to_string());
    }
    Ok(format!(
        "binary garbage: {} cases, no panic, history intact",
        GARBAGE_CASES.len()
    ))
}
