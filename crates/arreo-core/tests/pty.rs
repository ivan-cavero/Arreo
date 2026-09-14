//! T-0002 acceptance tests: spawn/read/write/resize/kill + ring-buffer bounds.
//!
//! Portable-first: these run on Linux here; the ConPTY path is exercised by
//! T-0007 on a Windows runner against the same API.

use arreo_core::pty::{ExitState, Pane, RingBuffer, HOT_LINES};
use std::time::Duration;

fn wait_for_output(pane: &Pane, needle: &str, timeout: Duration) -> Vec<String> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let lines = pane.drain();
        if lines.iter().any(|l| l.contains(needle)) {
            return lines;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {needle:?}; got: {lines:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn spawn_echo_read_and_exit_code() {
    let pane = Pane::spawn("/bin/echo", &["hello-pty"], 80, 24).expect("spawn echo");
    let lines = wait_for_output(&pane, "hello-pty", Duration::from_secs(10));
    assert!(lines.iter().any(|l| l.contains("hello-pty")));
    let exit = pane
        .wait_timeout(Duration::from_secs(10))
        .expect("child exits promptly");
    assert!(
        matches!(exit, ExitState::Exited(0)),
        "echo exits 0, got {exit:?}"
    );
}

#[test]
fn write_input_reaches_shell_and_resize_propagates() {
    let pane = Pane::spawn("/bin/sh", &[], 80, 24).expect("spawn sh");
    pane.send(b"echo typed-line-42\r").expect("send input");
    let lines = wait_for_output(&pane, "typed-line-42", Duration::from_secs(10));
    assert!(lines.iter().any(|l| l.contains("typed-line-42")));

    pane.resize(100, 30).expect("resize");
    assert_eq!(pane.size().expect("size"), (100, 30));

    // `stty size` reports rows cols of the resized PTY.
    pane.send(b"stty size\r").expect("send stty");
    let lines = wait_for_output(&pane, "30 100", Duration::from_secs(10));
    assert!(lines.iter().any(|l| l.contains("30 100")));
}

#[test]
fn child_exit_is_observed_and_zombie_free() {
    let pane = Pane::spawn("/bin/sh", &["-c", "exit 17"], 80, 24).expect("spawn");
    let exit = pane
        .wait_timeout(Duration::from_secs(10))
        .expect("short-lived child reaped");
    assert!(
        matches!(exit, ExitState::Exited(17)),
        "exit code 17 observed, got {exit:?}"
    );
    // Second poll is stable (no zombie, no panic on re-wait).
    assert!(matches!(pane.try_wait(), ExitState::Exited(17)));
}

#[test]
fn kill_terminates_a_sleeping_child() {
    let mut pane = Pane::spawn("/bin/sleep", &["60"], 80, 24).expect("spawn sleep");
    assert_eq!(pane.try_wait(), ExitState::Running);
    assert!(pane.child_pid().is_some(), "child has a pid");
    pane.kill().expect("kill");
    let exit = pane
        .wait_timeout(Duration::from_secs(10))
        .expect("killed child reaps");
    assert!(
        !matches!(exit, ExitState::Running),
        "killed child terminates, got {exit:?}"
    );
}

#[test]
fn ring_buffer_evicts_oldest_and_counts_dropped() {
    let mut buf = RingBuffer::new(4);
    for i in 0..10 {
        buf.push_bytes(format!("line-{i}\n").as_bytes());
    }
    let lines = buf.lines();
    assert_eq!(lines.len(), 4, "capacity respected");
    assert_eq!(lines[0], "line-6", "oldest evicted first");
    assert_eq!(buf.dropped(), 6);
}

#[test]
fn ring_buffer_memory_per_pane_within_budget() {
    // Worst case for the hot buffer: 512 lines × 64 KiB would be 32 MiB, but
    // realistic terminal lines are short. Fill with 512 typical lines (200 B
    // of text each) plus one adversarial 1 MiB single line, then assert the
    // ≤ 3 MiB per-pane budget holds and truncation was counted.
    let mut buf = RingBuffer::new(HOT_LINES);
    let line = "x".repeat(200);
    for _ in 0..HOT_LINES {
        buf.push_bytes(line.as_bytes());
        buf.push_bytes(b"\n");
    }
    let huge = "y".repeat(1024 * 1024);
    buf.push_bytes(huge.as_bytes());
    buf.push_bytes(b"\n");
    assert!(buf.len() <= HOT_LINES, "capacity respected under flood");
    assert!(
        buf.bytes_held() <= 3 * 1024 * 1024,
        "hot buffer ≤ 3 MiB, held {} bytes",
        buf.bytes_held()
    );
    assert!(
        buf.dropped_bytes() > 0,
        "giant-line truncation is counted, not silent"
    );
}

#[test]
fn giant_output_flood_never_grows_unbounded() {
    // 200k short lines through a live pane's buffer path: bounded + counted.
    let mut buf = RingBuffer::new(HOT_LINES);
    for i in 0..200_000u32 {
        buf.push_bytes(format!("flood-line-{i:06}-padding-padding\n").as_bytes());
    }
    assert!(buf.len() <= HOT_LINES);
    assert_eq!(buf.dropped(), 200_000 - HOT_LINES as u64);
    assert!(buf.bytes_held() <= 3 * 1024 * 1024);
}

/// **A partial line is visible to a reader and never enters the ring**
/// (T-0088). The prompt a harness prints without a trailing newline is the
/// product's flagship signal, so a reader must see it — but materialising it in
/// the ring made one written line two entries, permanently, on every surface
/// (`arreo read`, `attach`, the TUI, the handoff manifest, the snapshot). Both
/// halves are asserted here, and the second is the one that used to be a
/// `flush_partial()` + append.
#[test]
fn unterminated_prompt_line_is_visible_to_a_reader_and_not_materialised() {
    let mut buf = RingBuffer::new(8);
    buf.push_bytes(b"output line\n$ ");
    assert_eq!(
        buf.lines_with_partial(),
        vec!["output line".to_string(), "$ ".to_string()],
        "a reader sees the prompt"
    );
    assert_eq!(
        buf.lines(),
        vec!["output line".to_string()],
        "and the ring holds only terminated lines"
    );
    assert_eq!(buf.pending_line(), "$ ");

    // The program finishes the line: it becomes ONE line, not two.
    buf.push_bytes(b"echo hi\n");
    assert_eq!(
        buf.lines(),
        vec!["output line".to_string(), "$ echo hi".to_string()],
        "one written line is one ring line"
    );
    assert_eq!(buf.lines_with_partial(), buf.lines());
}

/// The measured defect, at the ring: a poll between two writes of one line must
/// not split it (T-0088).
#[test]
fn a_poll_between_two_writes_of_one_line_does_not_split_it() {
    let mut buf = RingBuffer::new(8);
    buf.push_bytes(b"burst-2|aaaa");
    // A reader polls here — this is what `drain()` does, and it must not mutate.
    let seen = buf.lines_with_partial();
    assert_eq!(seen, vec!["burst-2|aaaa".to_string()]);
    buf.push_bytes(b"bbbb\nburst-3|done\n");
    assert_eq!(
        buf.lines(),
        vec!["burst-2|aaaabbbb".to_string(), "burst-3|done".to_string()],
        "the ring holds the line the program wrote, whole"
    );
}

#[test]
fn concurrent_sends_never_deadlock_or_corrupt() {
    use std::sync::Arc;
    let pane = Arc::new(Pane::spawn("/bin/cat", &[], 80, 24).expect("spawn cat"));
    let mut handles = Vec::new();
    for i in 0..8 {
        let pane = Arc::clone(&pane);
        handles.push(std::thread::spawn(move || {
            for _ in 0..25 {
                let _ = pane.send(format!("writer-{i}\n").as_bytes());
            }
        }));
    }
    for handle in handles {
        handle.join().expect("sender thread joins");
    }
    // Drain works after the storm; buffer stayed bounded.
    let lines = pane.drain();
    assert!(lines.len() <= HOT_LINES);
    assert!(pane.bytes_held() <= 3 * 1024 * 1024);
}

#[test]
fn raw_journal_is_byte_exact_and_capped() {
    use arreo_core::pty::{RingBuffer, MAX_RAW_JOURNAL};
    let mut buf = RingBuffer::new(512);
    buf.push_bytes(b"plain\n\x1b[1;32mgreen\x1b[0m\n");
    let (raw, truncated) = (buf.raw_bytes(), buf.raw_truncated());
    assert!(!truncated);
    assert_eq!(raw, b"plain\n\x1b[1;32mgreen\x1b[0m\n");
    // Flood past the cap: journal stops at exactly 1 MiB and flags it.
    let mut buf = RingBuffer::new(512);
    buf.push_bytes(&vec![b'z'; MAX_RAW_JOURNAL + 100]);
    assert!(buf.raw_truncated());
    assert_eq!(buf.raw_bytes().len(), MAX_RAW_JOURNAL);
}
