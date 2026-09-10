//! T-0003 failing-first probes: VT grid + cursor + dirty ranges + scrollback.
//!
//! Written before `src/vt.rs` exists — this file MUST fail to compile until
//! the implementation lands. That is the TDD gate, not an accident.

use arreo_core::vt::{CursorPos, VtPane};

#[test]
fn plain_text_lands_in_cells_with_cursor_after_it() {
    let mut pane = VtPane::new(80, 24);
    let dirty = pane.feed(b"hello");
    assert_eq!(pane.line_text(0), "hello");
    assert_eq!(pane.cursor(), CursorPos { line: 0, col: 5 });
    assert!(
        dirty
            .iter()
            .any(|r| r.line == 0 && r.left == 0 && r.right >= 4),
        "line 0 cols 0-4 dirty, got {dirty:?}"
    );
}

#[test]
fn escape_sequences_never_leak_into_cell_text() {
    let mut pane = VtPane::new(80, 24);
    pane.feed(b"\x1b[1;32mgreen\x1b[0m plain");
    assert_eq!(pane.line_text(0), "green plain");
}

#[test]
fn scrollback_pages_back_losslessly() {
    let mut pane = VtPane::new(80, 4);
    for i in 0..200 {
        pane.feed(format!("scroll-line-{i:04}\n").as_bytes());
    }
    assert_eq!(pane.total_lines(), 200, "all lines retained");
    assert_eq!(pane.page(0, 3).unwrap()[0], "scroll-line-0000");
    assert_eq!(pane.page(197, 3).unwrap()[0], "scroll-line-0197");
}

#[test]
fn feed_cost_scales_with_input_not_grid() {
    let mut pane = VtPane::new(200, 60);
    for i in 0..500 {
        pane.feed(format!("bulk-{i}\n").as_bytes());
    }
    let start = std::time::Instant::now();
    pane.feed(b"x");
    let one_byte = start.elapsed();
    assert!(
        one_byte.as_millis() < 50,
        "1-byte feed took {one_byte:?} — smells like a grid scan"
    );
}

#[test]
fn million_line_replay_stays_bounded() {
    let mut pane = VtPane::new(80, 24);
    for i in 0..1_000_000u32 {
        pane.feed(format!("m{i}\n").as_bytes());
        if i % 100_000 == 0 {
            pane.spill_to_disk().unwrap();
        }
    }
    pane.spill_to_disk().unwrap();
    assert!(
        pane.ram_bytes() <= 3 * 1024 * 1024,
        "VT state ≤ 3 MiB, held {} bytes",
        pane.ram_bytes()
    );
    assert_eq!(pane.total_lines(), 1_000_000);
    assert_eq!(pane.page(0, 1).unwrap()[0], "m0");
    assert_eq!(pane.page(999_999, 1).unwrap()[0], "m999999");
}

#[test]
fn alt_screen_never_pollutes_scrollback() {
    let mut pane = VtPane::new(80, 24);
    pane.feed(b"real-line-1\n");
    pane.feed(b"\x1b[?1049h");
    assert!(pane.is_alt_screen(), "vim-style alt-screen detected");
    pane.feed(b"vim-content\nmore-vim\n");
    pane.feed(b"\x1b[?1049l");
    assert!(!pane.is_alt_screen());
    assert_eq!(
        pane.total_lines(),
        1,
        "app surface is transient, not history"
    );
    assert_eq!(pane.page(0, 1).unwrap()[0], "real-line-1");
}

#[test]
fn binary_garbage_cannot_corrupt_the_log() {
    let mut pane = VtPane::new(80, 24);
    pane.feed(&[0x00, 0xFF, 0xFE, 0x1b, b'[', b'Z', 0x07, 0x00]);
    pane.feed(b"after-garbage\n");
    pane.spill_to_disk().unwrap();
    let got = pane.page(0, 1).unwrap()[0].clone();
    assert!(!got.contains('\u{1b}'), "no escapes leak, got {got:?}");
    assert!(got.contains("after-garbage"), "text survives, got {got:?}");
}

#[test]
fn empty_feed_reports_no_damage() {
    let mut pane = VtPane::new(80, 24);
    assert!(pane.feed(b"").is_empty());
}

#[test]
fn giant_terminated_line_is_capped_and_counted() {
    let mut pane = VtPane::new(80, 24);
    pane.feed(&vec![b'A'; 5 * 1024 * 1024]);
    pane.feed(b"\n");
    assert!(
        pane.ram_bytes() <= 3 * 1024 * 1024,
        "ram {} bytes",
        pane.ram_bytes()
    );
    assert!(pane.dropped_partial_bytes() > 0, "drop counted, not silent");
}
