//! T-0011 failing-first probes: fixture record → save → load → replay.
//!
//! Written before `src/fixtures.rs` exists — MUST fail to compile until it lands.

use arreo_core::fixtures::{scan_secrets, Fixture};
use std::time::Duration;

#[test]
fn record_replay_round_trip_is_byte_exact() {
    let rec = Fixture::record(
        &["/bin/echo", "round-trip-bytes-99"],
        Duration::from_secs(10),
    )
    .expect("record echo");
    assert!(
        rec.text().contains("round-trip-bytes-99"),
        "capture holds the output"
    );
    let dir = std::env::temp_dir().join(format!("arreo-fixt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("roundtrip.pty");
    rec.save(&path).expect("save");
    let loaded = Fixture::load(&path).expect("load");
    assert_eq!(
        loaded.events.len(),
        rec.events.len(),
        "event count survives"
    );
    // Accelerated replay feeds the same bytes, pacing collapsed.
    let replayed = loaded.replay_accelerated();
    let orig: Vec<u8> = rec.events.iter().flat_map(|e| e.raw_bytes()).collect();
    assert_eq!(replayed, orig, "replay is byte-exact");
}

#[test]
fn secret_scan_flags_keys_before_commit() {
    let flagged = scan_secrets("export OPENAI_API_KEY=sk-abc123XYZ4567890abcdef\n");
    assert!(!flagged.is_empty(), "api-key-shaped content flagged");
    let clean = scan_secrets("echo hello world\n");
    assert!(clean.is_empty(), "benign output passes");
}

#[test]
fn empty_session_records_and_replays_cleanly() {
    let rec = Fixture::record(&["/bin/true"], Duration::from_secs(10)).expect("record true");
    let bytes: Vec<u8> = rec.events.iter().flat_map(|e| e.raw_bytes()).collect();
    let _ = bytes;
    let dir = std::env::temp_dir().join(format!("arreo-fixt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("empty.pty");
    rec.save(&path).expect("save");
    let loaded = Fixture::load(&path).expect("load");
    assert_eq!(loaded.replay_accelerated(), Vec::<u8>::new());
}

/// Committed fixtures stay loadable, byte-stable, and secret-free.
/// This is the CI gate for the T-0011 acceptance criteria.
#[test]
fn committed_fixtures_are_stable_and_clean() {
    use arreo_core::fixtures::{scan_secrets, Fixture};
    use std::path::PathBuf;

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("../../fixtures");
    let expected = [
        "question-permission.pty",
        "working-stream.pty",
        "idle-shell.pty",
        "vim-edit.pty",
        "locale-utf8.pty",
    ];
    for name in expected {
        let path = dir.join(name);
        assert!(path.exists(), "fixture committed: {name}");
        let fixture = Fixture::load(&path).expect("fixture loads");
        assert!(!fixture.events.is_empty(), "{name} holds events");
        // Deterministic replay: two loads, same bytes.
        let again = Fixture::load(&path).expect("fixture reloads");
        assert_eq!(
            fixture.replay_accelerated(),
            again.replay_accelerated(),
            "{name} replays deterministically"
        );
        // Small: every committed fixture < 64 KiB.
        let size = std::fs::metadata(&path).expect("stat").len();
        assert!(size < 64 * 1024, "{name} is small ({size} bytes)");
        // Clean: no secret-shaped content.
        let findings = scan_secrets(&fixture.text());
        assert!(findings.is_empty(), "{name} secret-free, got {findings:?}");
    }
    // Spot content: the question fixture really asks, the vim one really
    // drove the alternate screen, the locale one kept its multibyte text.
    let q = Fixture::load(&dir.join("question-permission.pty")).unwrap();
    assert!(q.text().contains("[y/n]"), "question fixture asks");
    let vim = Fixture::load(&dir.join("vim-edit.pty")).unwrap();
    assert!(
        vim.replay_accelerated().contains(&0x1b),
        "vim fixture holds real escapes"
    );
    let loc = Fixture::load(&dir.join("locale-utf8.pty")).unwrap();
    assert!(
        loc.text().contains("日本語"),
        "locale fixture keeps multibyte"
    );
}
