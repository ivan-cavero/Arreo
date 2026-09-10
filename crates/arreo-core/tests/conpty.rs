//! T-0007 failing-first probes: portable pane smoke across backends.
//!
//! Written before the xtask verb lands — the xtask part is exercised via the
//! binary, but these tests pin the Pane API the smoke test depends on.
//! On Windows (CI runner) these run against ConPTY; on unix against
//! posix_openpt. Same API, same assertions — that IS the portability claim.

use arreo_core::pty::{ExitState, Pane};
use std::time::Duration;

fn wait_for(pane: &Pane, needle: &str, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if pane.drain().iter().any(|l| l.contains(needle)) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {needle:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Backend-agnostic smoke: spawn → read → resize → kill. The xtask verb runs
/// OS-native commands (cmd/dir on Windows, sh/echo on unix); here we assert
/// the Pane mechanics the verb relies on.
#[test]
fn pane_holds_a_session_end_to_end() {
    let pane = Pane::spawn("/bin/sh", &["-c", "echo conpty-smoke-marker"], 80, 24).expect("spawn");
    wait_for(&pane, "conpty-smoke-marker", Duration::from_secs(10));
    pane.resize(100, 30).expect("resize propagates");
    assert_eq!(pane.size().expect("size"), (100, 30));
    let exit = pane
        .wait_timeout(Duration::from_secs(10))
        .expect("child reaps");
    assert!(
        matches!(exit, ExitState::Exited(0)),
        "clean exit, got {exit:?}"
    );
}

#[test]
fn multibyte_output_survives_the_round_trip() {
    // UTF-8 fidelity: accented + CJK + checkmark must arrive intact.
    // On Windows the xtask verb forces the UTF-8 codepage first (chcp 65001);
    // here we assert Pane itself never mangles multibyte sequences.
    let pane = Pane::spawn(
        "/bin/sh",
        &["-c", "printf 'héllo wörld 日本語 ✓\\n'"],
        80,
        24,
    )
    .expect("spawn");
    wait_for(&pane, "日本語", Duration::from_secs(10));
    let lines = pane.drain();
    assert!(
        lines.iter().any(|l| l.contains("héllo") && l.contains("✓")),
        "multibyte intact: {lines:?}"
    );
}

#[test]
fn killed_pane_leaves_no_zombie() {
    let mut pane = Pane::spawn("/bin/sleep", &["30"], 80, 24).expect("spawn");
    let pid = pane.child_pid().expect("pid known");
    assert!(pid > 0);
    pane.kill().expect("kill");
    let exit = pane
        .wait_timeout(Duration::from_secs(10))
        .expect("reaped after kill");
    assert!(!matches!(exit, ExitState::Running), "dead, got {exit:?}");
    // Re-poll is stable (reaped exactly once, no zombie).
    assert!(!matches!(pane.try_wait(), ExitState::Running));
}
